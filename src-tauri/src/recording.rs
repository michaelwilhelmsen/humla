use parking_lot::Mutex;
use serde::Serialize;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::process::Child;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

pub type Inflight = Arc<Mutex<Vec<JoinHandle<()>>>>;

/// Bounded ring of recent transcript words. Used as Whisper's `initial_prompt`
/// so each chunk decodes with knowledge of what was just said — sentence
/// continuity, proper-noun spelling, and a real prior context that suppresses
/// silence-driven hallucinations like "Thanks for watching".
pub struct TranscriptTrail {
    words: VecDeque<String>,
    capacity: usize,
}

impl TranscriptTrail {
    pub fn new(capacity: usize) -> Self {
        Self {
            words: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    pub fn push(&mut self, text: &str) {
        for w in text.split_whitespace() {
            if self.words.len() == self.capacity {
                self.words.pop_front();
            }
            self.words.push_back(w.to_string());
        }
        self.collapse_trailing_repetition();
    }

    // Collapse any trailing pair of identical N-grams down to a single copy.
    // Whisper's repetition pathology produces output like "X? X? X? X? X?",
    // and feeding that back as `initial_prompt` for the next chunk biases
    // decoding toward more of the same — the loop becomes self-sustaining.
    // Iteratively dropping trailing repeats breaks the feedback even if a bad
    // chunk slipped past the per-chunk repetition filter.
    fn collapse_trailing_repetition(&mut self) {
        loop {
            let mut collapsed = false;
            for phrase_len in 1..=7 {
                let n = self.words.len();
                if n < phrase_len * 2 {
                    continue;
                }
                let mut equal = true;
                for i in 0..phrase_len {
                    if self.words[n - phrase_len + i].to_lowercase()
                        != self.words[n - 2 * phrase_len + i].to_lowercase()
                    {
                        equal = false;
                        break;
                    }
                }
                if equal {
                    for _ in 0..phrase_len {
                        self.words.pop_back();
                    }
                    collapsed = true;
                    break;
                }
            }
            if !collapsed {
                return;
            }
        }
    }

    pub fn as_prompt(&self) -> Option<String> {
        if self.words.is_empty() {
            None
        } else {
            Some(self.words.iter().cloned().collect::<Vec<_>>().join(" "))
        }
    }

    pub fn clear(&mut self) {
        self.words.clear();
    }
}

impl Default for TranscriptTrail {
    fn default() -> Self {
        // 150 words ≈ ~200 Whisper tokens, which fits inside the 224-token
        // prompt budget alongside ~50 tokens of custom vocabulary.
        Self::new(150)
    }
}

/// Which audio stream a chunk came from. The mic stream is always the user
/// (we label its chunks "You" without diarization). The system stream
/// captures remote participants on calls; we run the offline diarizer on it
/// to separate multiple remote speakers. In-person meetings produce only
/// mic chunks (system is silent → no chunks emitted) and the diarizer runs
/// on the mic stream instead so multiple humans in the same room get
/// distinct labels.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChunkSource {
    Mic,
    Sys,
}

impl Default for ChunkSource {
    fn default() -> Self {
        // Pre-v0.8.0 sidecars didn't emit `source`. If we ever load an old
        // sidecar event for any reason (stale dev cache mid-upgrade), treat
        // the chunk as mic — the safer default since mic always exists.
        ChunkSource::Mic
    }
}

/// Per-chunk word timing relative to the chunk's start. Populated by the
/// local Whisper path's token-level timestamps; empty when transcribe
/// came from a provider that doesn't expose word data (current OpenAI
/// API for the chunk-streaming flow). Word `start_ms` / `end_ms` are
/// **chunk-relative** — add the parent ChunkRecord's start_ms to map
/// back into stream-absolute time.
#[derive(Clone, Debug)]
pub struct ChunkWord {
    pub text: String,
    pub start_ms: u64,
    pub end_ms: u64,
}

/// Per-chunk metadata captured during recording. The diarization step needs
/// to align speaker segments (timestamps relative to the per-source full
/// recording WAV) against chunk-level transcripts; this log holds the link
/// between "chunk N's text", which source it came from, and where it sits
/// on that source's timeline.
#[derive(Clone, Debug)]
pub struct ChunkRecord {
    pub source: ChunkSource,
    pub start_ms: u64,
    pub text: String,
    pub words: Vec<ChunkWord>,
    /// What the STT provider thought this chunk was spoken in, when it
    /// says at all (issue #167). Per-chunk rather than per-recording
    /// because a 2-second "mm-hm" detects as anything — the post-stop
    /// vote in `majority_language` is what turns these into an answer.
    pub detected_language: Option<String>,
}


/// What happens to each chunk a capture produces — the one axis on which live
/// recording, a "Transcribe manually" capture and a deferred replay differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SinkMode {
    /// The live capture slot. Each finished chunk appends to `note.transcript`
    /// and emits `transcript_replaced`, so the user watches the transcript
    /// build — skipped once the slot no longer belongs to that note, since the
    /// post-stop rewrite is about to land.
    #[default]
    Live,
    /// A capture that never reaches a provider as it runs: the "Transcribe
    /// manually" setting (#146). The audio is still chunked and written, and
    /// the take's full streams are still retained — nothing is transcribed
    /// until the note's Transcribe action asks for it.
    Deferred,
    /// A deferred replay of one already-recorded take's retained audio (#146).
    /// Chunks accumulate in this sink's own log and the note's transcript is
    /// left alone until the diarize pass rebuilds it from every session's
    /// timeline (ADR-0004) — so a replay that dies half-way leaves no text
    /// behind that no timeline accounts for.
    ///
    /// `source` re-tags every chunk the sidecar reports. Its `--import` mode
    /// always writes through the mic writers, and a replay covers one retained
    /// stream at a time, so the source is the caller's knowledge rather than
    /// the sidecar's.
    Replay { source: ChunkSource },
}

/// Where one capture's chunks go, and how they get there.
///
/// Live recording and a deferred replay run the *same* chunk pipeline. They
/// differ only in whose rolling context a chunk decodes against, which log it
/// lands in, and whether the note's transcript moves as it goes. Holding that
/// in one shared value is what lets a deferred replay run without occupying
/// the single live-capture slot — the thing that would otherwise block the
/// user from starting a new recording while an hour of audio transcribes.
#[derive(Clone)]
pub struct CaptureSink {
    // Per-source rolling context windows of the last ~150 committed words.
    // Fed to Whisper's `initial_prompt` for every chunk so decoding stays
    // anchored to its own stream rather than mixing the user's side with
    // the remote side's vocabulary, which would harm proper-noun spelling
    // and pull each Whisper invocation toward the wrong language.
    pub mic_trail: Arc<Mutex<TranscriptTrail>>,
    pub sys_trail: Arc<Mutex<TranscriptTrail>>,
    // Per-chunk metadata. Read by the offline diarization pass to align
    // FluidAudio's speaker segments back to the chunks the user saw stream in.
    pub chunk_log: Arc<Mutex<Vec<ChunkRecord>>>,
    // Paths to the per-source full-recording WAV files the sidecar wrote.
    // Consumed by the diarization pass, then deleted alongside the temp dir.
    // Either may be `None` if its source produced no audio (mic permission
    // denied, no system audio active for the whole recording, etc).
    pub mic_full_wav_path: Arc<Mutex<Option<PathBuf>>>,
    pub sys_full_wav_path: Arc<Mutex<Option<PathBuf>>>,
    // Longest full-recording stream this capture wrote, in ms, as the sidecar
    // reported it on shutdown. The manifest's `duration_ms` normally comes from
    // the take's timeline (it reflects content, and trailing silence isn't
    // worth showing), but a "Transcribe manually" take has no timeline at
    // finalise — so this is where its length comes from (#146).
    //
    // It has to be settled at CAPTURE time, not filled in later: the sync
    // engine derives a session's last-write-wins key from its start time on the
    // stated grounds that "index / started_at / duration / streams never
    // change" (`cloud-sync`'s `push_session`), so a duration corrected after
    // the fact would re-push under a byte-identical key and converge only on
    // the server comparing strictly.
    pub captured_duration_ms: Arc<Mutex<u64>>,
    pub mode: SinkMode,
}

impl Default for CaptureSink {
    fn default() -> Self {
        Self::new(SinkMode::default())
    }
}

impl CaptureSink {
    pub fn new(mode: SinkMode) -> Self {
        Self {
            mic_trail: Arc::new(Mutex::new(TranscriptTrail::default())),
            sys_trail: Arc::new(Mutex::new(TranscriptTrail::default())),
            chunk_log: Arc::new(Mutex::new(Vec::new())),
            mic_full_wav_path: Arc::new(Mutex::new(None)),
            sys_full_wav_path: Arc::new(Mutex::new(None)),
            captured_duration_ms: Arc::new(Mutex::new(0)),
            mode,
        }
    }

    /// The rolling context for one stream. Per-source because the mic and
    /// system streams are separate conversations.
    pub fn trail(&self, source: ChunkSource) -> &Arc<Mutex<TranscriptTrail>> {
        match source {
            ChunkSource::Mic => &self.mic_trail,
            ChunkSource::Sys => &self.sys_trail,
        }
    }

    /// Record how long one stream turned out to be. Kept as the max across
    /// sources: the two streams of one take run for the same wall clock, and
    /// either may be absent or shorter (a mic that joined late, a system stream
    /// that never carried anything).
    pub fn note_stream_duration(&self, duration_ms: u64) {
        let mut slot = self.captured_duration_ms.lock();
        *slot = (*slot).max(duration_ms);
    }

    /// The slot for one stream's full-recording WAV path.
    pub fn full_wav_slot(&self, source: ChunkSource) -> &Arc<Mutex<Option<PathBuf>>> {
        match source {
            ChunkSource::Mic => &self.mic_full_wav_path,
            ChunkSource::Sys => &self.sys_full_wav_path,
        }
    }

    /// Whether arriving chunks are transcribed at all.
    pub fn transcribes_on_arrival(&self) -> bool {
        !matches!(self.mode, SinkMode::Deferred)
    }

    /// Whether a finished chunk streams into `note.transcript`.
    pub fn streams_to_note(&self) -> bool {
        matches!(self.mode, SinkMode::Live)
    }

    /// The same sink re-pointed at another retained stream. Shares the chunk
    /// log and both trails, so one take's two streams accumulate into a single
    /// log for the diarize pass (which has to see both to tell an in-person
    /// meeting from a call) while each keeps its own rolling context.
    pub fn for_stream(&self, source: ChunkSource) -> Self {
        Self {
            mode: SinkMode::Replay { source },
            ..self.clone()
        }
    }

    /// The stream a chunk belongs to: what the caller knows for a replay,
    /// otherwise what the sidecar reported.
    pub fn resolve_source(&self, reported: ChunkSource) -> ChunkSource {
        match self.mode {
            SinkMode::Replay { source } => source,
            _ => reported,
        }
    }
}

/// Live in-memory state for the *currently capturing* recording — child
/// process handles, in-flight transcribe tasks, rolling context, and the
/// id/timestamp allocated for the persisted session this capture will
/// finalize into. Named `LiveCapture` (not `RecordingSession`) to avoid
/// colliding with the persisted per-note *session* concept in
/// [`crate::sessions`], which is what `sessions.json`, the carousel, and the
/// on-disk `recordings/<note_id>/<session_id>/` layout all refer to.
#[derive(Default)]
pub struct LiveCapture {
    pub note_id: Option<String>,
    pub child: Option<Child>,
    pub temp_dir: Option<PathBuf>,
    pub stop_tx: Option<mpsc::Sender<()>>,
    // Persisted-session bookkeeping. Allocated at `recording_start` and
    // snapshotted into the post-stop chain, which writes this capture's
    // assets into `recordings/<note_id>/<session_id>/` and appends a manifest
    // entry stamped `session_started_at`.
    pub session_id: Option<String>,
    pub session_started_at: Option<String>,
    // Handles for in-flight transcribe tasks. Drained on stop so the
    // transcript is fully written before we flip to Idle.
    pub inflight: Inflight,
    // Handle for the stdout reader task that spawns transcribes. Awaiting
    // it guarantees no further pushes to `inflight` are coming.
    pub reader: Option<JoinHandle<()>>,
    // This capture's chunk destination: rolling context, chunk log, full-WAV
    // paths, and what happens to each chunk as it lands. Replaced (not
    // cleared) at the start of every capture, so a straggler chunk from the
    // previous one can't push into the next one's log.
    pub sink: Arc<CaptureSink>,
    // Snapshot of the note's transcript at recording_start. Used by the
    // offline diarization step to prepend prior content to this session's
    // diarized output, so resuming a recording adds to the transcript
    // instead of clobbering it. Empty string means "fresh recording, no
    // prior content."
    pub transcript_at_start: Arc<Mutex<String>>,
    // Cloud recording-lock bookkeeping for shared (workspace) notes. `lock_id`
    // is the PocketBase `note_locks` record we hold while recording, so two
    // teammates can't record the same note at once (their transcripts would
    // clobber each other under last-write-wins sync). `lock_heartbeat` is the
    // task that keeps the lock's `expires` fresh; it MUST be aborted on stop /
    // crash or it would keep a dead recording's lock alive. Both `None` for
    // Personal notes or when the cloud is unreachable (recording proceeds
    // unlocked — a flaky network shouldn't block capture).
    pub lock_id: Option<String>,
    pub lock_heartbeat: Option<JoinHandle<()>>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingStatus {
    pub note_id: Option<String>,
    pub phase: Phase,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Idle,
    Starting,
    Recording,
    Paused,
    Stopping,
    Diarizing,
    // A file import is replaying through the transcribe pipeline. Occupies the
    // same single capture slot as a live recording (Record and Import are
    // mutually exclusive), but the sidecar runs a one-shot `--import` replay
    // instead of live mic/system capture. Streams the transcript in exactly
    // like recording; on completion it flows into the same Diarizing → Idle
    // post-stop chain.
    Importing,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptPayload {
    pub note_id: String,
    pub text: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryPayload {
    pub note_id: String,
    pub summary: String,
}

/// Per-note summary lifecycle. Lives on its own channel
/// (`summary_status`) so summarising note B doesn't clobber the
/// `recording_status` slot while note A is recording — that was the
/// failure mode pre-v0.19.3 when summary used the same channel and
/// emitted `note_id: None, phase: Idle` on completion, blanking the
/// real recording state.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryStatusPayload {
    pub note_id: String,
    pub active: bool,
}

/// Per-note title lifecycle (#90), on its own channel (`title_status`) for the
/// same reason the summary one has its own: titling note B must not touch the
/// state of note A. Brackets the model call only — an ineligible note never
/// emits, because no call is made for it.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TitleStatusPayload {
    pub note_id: String,
    pub active: bool,
}

/// Per-note deferred-transcription lifecycle (#146), on its own channel for
/// the same reason `summary_status` has one: a replay on note A must not touch
/// note B's state, and — unlike a live capture — it must not reach
/// `recording_status` at all, since a recording may be running on another note
/// the whole time.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscribeStatusPayload {
    pub note_id: String,
    pub active: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamDeltaPayload {
    pub note_id: String,
    pub delta: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorPayload {
    pub note_id: Option<String>,
    pub message: String,
}

#[derive(serde::Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum SidecarEvent {
    Chunk {
        // Which audio stream produced this chunk. Older sidecars (pre-v0.8.0)
        // didn't emit a source — `Default` for `ChunkSource` is `Mic`, which
        // matches the legacy "single-mixed-stream" semantics where everything
        // ended up labeled as mic.
        #[serde(default)]
        source: ChunkSource,
        path: String,
        // Time (in milliseconds) at which this chunk's audio starts relative
        // to the first frame of its source stream's full WAV. Defaults to 0
        // for older sidecar builds that didn't emit this.
        #[serde(default)]
        start_ms: u64,
    },
    FullRecording {
        // See `Chunk.source`. The two streams produce two `full_recording`
        // events, one each for `mic` and `sys`. Either may be absent if its
        // source never wrote any frames (e.g. screen permission denied).
        #[serde(default)]
        source: ChunkSource,
        path: String,
        duration_ms: u64,
    },
    Error {
        message: String,
    },
    Stopped,
    Paused,
    Resumed,
    Heartbeat {
        mic_frames: u64,
        sys_frames: u64,
        chunks: u64,
        mic_peak: f32,
        sys_peak: f32,
    },
    // Non-fatal notice from the sidecar — e.g. it recovered mic capture after
    // an audio device change mid-recording. Surfaced to the user as a transient
    // toast (see the reader loop in commands.rs).
    Diagnostic {
        message: String,
    },
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticPayload {
    pub note_id: String,
    pub mic_frames: u64,
    pub sys_frames: u64,
    pub chunks: u64,
    pub mic_peak: f32,
    pub sys_peak: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trail_keeps_last_n_words() {
        let mut t = TranscriptTrail::new(5);
        t.push("one two three");
        t.push("four five six");
        // capacity 5, total seen 6 words → drops "one"
        assert_eq!(t.as_prompt(), Some("two three four five six".to_string()));
    }

    #[test]
    fn trail_returns_none_when_empty() {
        let t = TranscriptTrail::new(10);
        assert_eq!(t.as_prompt(), None);
    }

    #[test]
    fn trail_caps_at_max_when_pushing_long_text() {
        let mut t = TranscriptTrail::new(3);
        t.push("alpha beta gamma delta epsilon");
        assert_eq!(t.as_prompt(), Some("gamma delta epsilon".to_string()));
    }

    #[test]
    fn trail_clear_drops_history() {
        let mut t = TranscriptTrail::new(5);
        t.push("hello world");
        t.clear();
        assert_eq!(t.as_prompt(), None);
    }

    #[test]
    fn trail_collapses_trailing_word_repetition() {
        let mut t = TranscriptTrail::new(50);
        t.push("yes yes yes yes yes");
        // Five reps of a single-word phrase collapse to one. Otherwise the
        // next chunk's prompt would be "yes yes yes yes yes" and bias the
        // decoder toward another "yes" loop.
        assert_eq!(t.as_prompt(), Some("yes".to_string()));
    }

    #[test]
    fn trail_collapses_trailing_phrase_repetition() {
        let mut t = TranscriptTrail::new(100);
        t.push("Er det en bok? Er det en bok? Er det en bok?");
        // The four-word phrase repeats three times → collapse to one copy.
        assert_eq!(t.as_prompt(), Some("Er det en bok?".to_string()));
    }

    #[test]
    fn trail_preserves_unique_repetitions_with_different_words_around() {
        let mut t = TranscriptTrail::new(50);
        t.push("hello world hello friend");
        // Not a contiguous N-gram repeat — leave it alone.
        assert_eq!(t.as_prompt(), Some("hello world hello friend".to_string()));
    }

    #[test]
    fn trail_collapse_is_case_insensitive() {
        let mut t = TranscriptTrail::new(50);
        t.push("Yes YES yes");
        // Casing varies but the words are the same → collapse.
        // The collapse strips trailing duplicates, leaving the earliest copy.
        assert_eq!(t.as_prompt(), Some("Yes".to_string()));
    }

    #[test]
    fn sidecar_event_deserializes_diagnostic() {
        // The sidecar emits {"event":"diagnostic","message":"..."} when it
        // recovers mic capture after an audio device change. `rename_all =
        // "snake_case"` must map the `Diagnostic` variant to the "diagnostic"
        // tag — lock that contract so a rename on either side can't silently
        // drop the notice.
        let json = r#"{"event":"diagnostic","message":"device changed"}"#;
        match serde_json::from_str::<SidecarEvent>(json).unwrap() {
            SidecarEvent::Diagnostic { message } => assert_eq!(message, "device changed"),
            _ => panic!("expected Diagnostic variant"),
        }
    }

    // -----------------------------------------------------------------------
    // CaptureSink (#146)
    // -----------------------------------------------------------------------

    #[test]
    fn live_capture_transcribes_and_streams() {
        let sink = CaptureSink::new(SinkMode::Live);
        assert!(sink.transcribes_on_arrival());
        assert!(sink.streams_to_note());
    }

    #[test]
    fn a_deferred_capture_transcribes_nothing() {
        // The whole point: chunks are written and never dispatched, so the Mac
        // stays quiet for the length of the meeting.
        let sink = CaptureSink::new(SinkMode::Deferred);
        assert!(!sink.transcribes_on_arrival());
        assert!(!sink.streams_to_note());
    }

    #[test]
    fn a_replay_transcribes_without_touching_the_transcript() {
        // Its text reaches the note once, from the timeline rebuild — so a
        // replay that dies half-way leaves no text behind that no timeline
        // accounts for (ADR-0004).
        let sink = CaptureSink::new(SinkMode::Replay {
            source: ChunkSource::Mic,
        });
        assert!(sink.transcribes_on_arrival());
        assert!(!sink.streams_to_note());
    }

    #[test]
    fn a_replay_overrides_the_source_the_sidecar_reported() {
        // The sidecar's `--import` mode writes through its mic writers whatever
        // it is fed, so a replay of a retained `sys.wav` would otherwise land
        // every chunk on the mic side and make a call look in-person.
        let sink = CaptureSink::new(SinkMode::Replay {
            source: ChunkSource::Sys,
        });
        assert_eq!(sink.resolve_source(ChunkSource::Mic), ChunkSource::Sys);
        // A live capture trusts the event — the two streams are real there.
        let live = CaptureSink::new(SinkMode::Live);
        assert_eq!(live.resolve_source(ChunkSource::Sys), ChunkSource::Sys);
        assert_eq!(live.resolve_source(ChunkSource::Mic), ChunkSource::Mic);
    }

    #[test]
    fn for_stream_shares_one_chunk_log_across_a_takes_two_streams() {
        // Both retained streams of one take have to accumulate into a single
        // log: the diarize pass reads mic-vs-sys presence to tell an in-person
        // meeting from a call, and two separate logs would make every take
        // look single-stream.
        let base = CaptureSink::new(SinkMode::Replay {
            source: ChunkSource::Mic,
        });
        let mic = base.for_stream(ChunkSource::Mic);
        let sys = base.for_stream(ChunkSource::Sys);
        mic.chunk_log.lock().push(ChunkRecord {
            source: ChunkSource::Mic,
            start_ms: 0,
            text: "hei".into(),
            words: vec![],
            detected_language: None,
        });
        sys.chunk_log.lock().push(ChunkRecord {
            source: ChunkSource::Sys,
            start_ms: 10,
            text: "hallo".into(),
            words: vec![],
            detected_language: None,
        });
        assert_eq!(base.chunk_log.lock().len(), 2);
        // Trails stay per-source, though: each stream is its own conversation.
        mic.trail(ChunkSource::Mic).lock().push("hei");
        assert_eq!(sys.trail(ChunkSource::Sys).lock().as_prompt(), None);
        assert_eq!(
            base.trail(ChunkSource::Mic).lock().as_prompt(),
            Some("hei".to_string())
        );
    }
}
