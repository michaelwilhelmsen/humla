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
    // How far the replay this sink belongs to has got, when it is one (#146).
    // `None` for a live capture and for a "Transcribe manually" one: neither
    // measures its work in takes, and a live capture's progress is the
    // transcript arriving on screen.
    pub replay_progress: Option<Arc<Mutex<ReplayProgress>>>,
    pub mode: SinkMode,
}

/// One take's share of a replay's work. Each retained stream is its own pass
/// over the same audio, so a take that kept both costs twice its duration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TakeWork {
    pub streams: u32,
    pub duration_ms: u64,
}

/// Which half of a take's work a replay is in (#188). A take is replayed
/// through the provider and then diarized, and the diarize half has no measure
/// of its own — the sidecar emits no progress — so the phase is what tells the
/// bar to stop drawing the audio-position fraction rather than sit at 100%
/// under a label that no longer describes the work.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayPhase {
    Transcribing,
    Diarizing,
}

/// What a replay reports: audio position against the run's total, which take of
/// how many it is on, and which half of that take's work is running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplaySnapshot {
    pub phase: ReplayPhase,
    pub done_ms: u64,
    pub total_ms: u64,
    pub take: u32,
    pub takes: u32,
}

/// How far a deferred transcription has got (#146), weighted by **audio
/// position** rather than chunks: a replay's chunk count isn't known until it
/// ends, while every take's duration is known before it starts.
///
/// `done_ms` is a high-water mark clamped to the total, so the fraction can
/// never retreat — not at a mic→sys handover, and not on a chunk whose
/// transcribe finished out of order. A bar that goes backwards reads as a fault
/// in the transcription rather than in the measurement.
///
/// The phase moves with the run rather than being derived from the fraction: a
/// take is fully replayed before its diarize starts, so "done_ms == total_ms"
/// and "diarizing" would otherwise be the same reading (#188).
pub struct ReplayProgress {
    takes: Vec<TakeWork>,
    total_ms: u64,
    // 0-based take being replayed.
    take: usize,
    // Audio behind the stream being replayed.
    base_ms: u64,
    done_ms: u64,
    phase: ReplayPhase,
}

impl ReplayProgress {
    pub fn new(takes: Vec<TakeWork>) -> Self {
        let total_ms = takes.iter().map(Self::work).sum();
        Self {
            takes,
            total_ms,
            take: 0,
            base_ms: 0,
            done_ms: 0,
            phase: ReplayPhase::Transcribing,
        }
    }

    fn work(t: &TakeWork) -> u64 {
        t.duration_ms.saturating_mul(t.streams as u64)
    }

    /// Move to take `index` (0-based), snapping the base to everything the
    /// takes before it were worth. That makes a take boundary exact even if a
    /// stream ended without reporting.
    pub fn begin_take(&mut self, index: usize) {
        self.take = index;
        self.phase = ReplayPhase::Transcribing;
        let behind: u64 = self.takes.iter().take(index).map(Self::work).sum();
        self.base_ms = self.base_ms.max(behind).min(self.total_ms);
        self.done_ms = self.done_ms.max(self.base_ms);
    }

    /// This take's audio is through the provider and its diarize is starting.
    /// Leaves the position alone: the fraction it reached is what the next
    /// take resumes from, so it can't retreat across the boundary.
    pub fn begin_diarize(&mut self) {
        self.phase = ReplayPhase::Diarizing;
    }

    /// A chunk of the stream being replayed has been through the provider.
    /// `start_ms` is its position in that stream, and it can claim no more than
    /// the stream's own share of the total.
    pub fn note_chunk(&mut self, start_ms: u64) {
        let stream_ms = self.takes.get(self.take).map(|t| t.duration_ms).unwrap_or(0);
        let cap = self.base_ms.saturating_add(stream_ms).min(self.total_ms);
        let reached = self.base_ms.saturating_add(start_ms).min(cap);
        self.done_ms = self.done_ms.max(reached);
    }

    /// One retained stream has been replayed end to end.
    pub fn finish_stream(&mut self) {
        let stream_ms = self.takes.get(self.take).map(|t| t.duration_ms).unwrap_or(0);
        self.base_ms = self.base_ms.saturating_add(stream_ms).min(self.total_ms);
        self.done_ms = self.done_ms.max(self.base_ms);
    }

    pub fn snapshot(&self) -> ReplaySnapshot {
        let takes = self.takes.len() as u32;
        ReplaySnapshot {
            phase: self.phase,
            done_ms: self.done_ms,
            total_ms: self.total_ms,
            take: (self.take as u32 + 1).min(takes),
            takes,
        }
    }
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
            replay_progress: None,
            mode,
        }
    }

    /// Attach the run-wide progress a deferred replay reports on. Shared by
    /// every stream of every take in one run, since the fraction is over the
    /// whole run.
    pub fn with_progress(mut self, progress: Arc<Mutex<ReplayProgress>>) -> Self {
        self.replay_progress = Some(progress);
        self
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
    // Where the capture slot is in its stop sequence. See [`StopState`].
    pub stop: StopState,
}

impl LiveCapture {
    /// The refusal a caller trying to take the capture slot must return, or
    /// `None` when the slot is takeable. The invariant it protects: a new
    /// capture can never clear `chunk_log` before the previous stop has
    /// snapshotted it.
    pub fn stop_in_progress(&self) -> Option<&'static str> {
        match self.stop {
            StopState::None => None,
            StopState::Draining | StopState::Finishing => Some(STOP_IN_PROGRESS),
        }
    }

    /// Move the slot into its stop sequence and return the note it holds.
    /// `note_id` deliberately stays set — the tail of the transcript keeps
    /// appending live through the drain — so `Draining` is what refuses a new
    /// capture until the post-stop chain releases the slot. Both the live stop
    /// and the import completion path go through here.
    pub fn begin_stop(&mut self) -> Result<String, &'static str> {
        if let Some(msg) = self.stop_in_progress() {
            return Err(msg);
        }
        let note_id = self.note_id.clone().ok_or("not recording")?;
        self.stop = StopState::Draining;
        Ok(note_id)
    }
}

/// Where the single capture slot is in its stop sequence.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum StopState {
    /// No stop underway: the slot is either free or holding a live capture.
    #[default]
    None,
    /// Stop was pressed and the in-flight transcribes are still landing.
    /// `note_id` stays set through this window, so the tail of the transcript
    /// appends and emits exactly as it did while recording.
    Draining,
    /// The `chunk_log` snapshot has been taken and the post-stop chain owns
    /// `note.transcript` — it rewrites the whole string with speaker labels,
    /// so a late append would land on top of that.
    Finishing,
}

/// Refusal for `recording_start` / `import_audio` while a stop is in progress.
/// Phrased for the toast the frontend already shows.
pub const STOP_IN_PROGRESS: &str = "Finishing the previous recording — try again in a moment.";

/// Should a chunk that just finished transcribing append to the note live?
/// True while the capture runs and through the stop drain; false once the
/// post-stop chain has snapshotted `chunk_log`, and false for a chunk whose
/// note is no longer the one in the slot (the user started a new capture).
pub fn appends_live(active_note: Option<&str>, stop: StopState, note_id: &str) -> bool {
    active_note == Some(note_id) && stop != StopState::Finishing
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingStatus {
    pub note_id: Option<String>,
    pub phase: Phase,
    // Drain progress, present only during `Stopping` (#182): how many
    // transcribes were in flight when stop was pressed, and how many of those
    // have landed. Additive and skipped when absent, so every listener that
    // only reads `{noteId, phase}` keeps working.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub done: Option<u32>,
    // This capture ran with "Transcribe manually" on (#146), so its stop has
    // no chunks to drain and no text to diarize and lands on Idle in a few
    // hundred milliseconds. Marked on `Stopping` and `Diarizing` so the
    // recording bar can stand aside rather than flash a bar that means
    // nothing. The phases still emit: the Idle is what makes the pending take
    // visible, since the note view re-reads its sessions on every transition.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deferred: Option<bool>,
}

impl RecordingStatus {
    /// A phase with nothing attached — every emit but a stop's.
    pub fn plain(note_id: Option<&str>, phase: Phase) -> Self {
        Self {
            note_id: note_id.map(|s| s.to_string()),
            phase,
            pending: None,
            done: None,
            deferred: None,
        }
    }

    /// `Stopping` with the drain's progress on it (#182).
    pub fn stopping_progress(note_id: &str, pending: u32, done: u32) -> Self {
        Self {
            pending: Some(pending),
            done: Some(done),
            ..Self::plain(Some(note_id), Phase::Stopping)
        }
    }

    /// A stop phase of a capture that never transcribed as it ran (#146).
    /// Carries no drain counts: nothing was dispatched, so there is nothing
    /// pending to count.
    pub fn deferred_phase(note_id: &str, phase: Phase) -> Self {
        Self {
            deferred: Some(true),
            ..Self::plain(Some(note_id), phase)
        }
    }
}

/// Wall-clock cost of every step of one stop chain, in milliseconds (#182).
/// Filled as the chain runs and merged into the take's diarize diagnostics
/// JSON at the end, so a slow stop can be attributed to a step instead of
/// guessed at. `diarize_mic_ms` / `diarize_sys_ms` are per sidecar invocation
/// and absent when that stream wasn't diarized; `diagnostics_path` is the file
/// the timings are merged into, not a duration.
#[derive(Clone, Default, Serialize)]
pub struct StopTimings {
    pub sidecar_shutdown_ms: u64,
    pub reader_wait_ms: u64,
    pub drain_ms: u64,
    pub drain_pending_chunks: u32,
    pub keep_audio_ms: u64,
    pub detect_language_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diarize_mic_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diarize_sys_ms: Option<u64>,
    pub playback_assets_ms: u64,
    pub finalize_session_ms: u64,
    pub unify_ms: u64,
    pub temp_cleanup_ms: u64,
    pub total_ms: u64,
    #[serde(skip)]
    pub diagnostics_path: Option<PathBuf>,
}

impl StopTimings {
    /// One-line stderr summary of the chain, printed on every stop.
    pub fn summary(&self) -> String {
        let opt = |v: Option<u64>| v.map(|v| v.to_string()).unwrap_or_else(|| "-".into());
        format!(
            "stop timings: total={}ms sidecar={} reader={} drain={} ({} pending) keep_audio={} detect_lang={} diarize_mic={} diarize_sys={} playback={} finalize={} unify={} cleanup={}",
            self.total_ms,
            self.sidecar_shutdown_ms,
            self.reader_wait_ms,
            self.drain_ms,
            self.drain_pending_chunks,
            self.keep_audio_ms,
            self.detect_language_ms,
            opt(self.diarize_mic_ms),
            opt(self.diarize_sys_ms),
            self.playback_assets_ms,
            self.finalize_session_ms,
            self.unify_ms,
            self.temp_cleanup_ms,
        )
    }
}

/// What one take of a replay cost (#188), split at the seam the user sees: the
/// audio through the provider, then the diarize that writes the timeline,
/// rebuilds the transcript and writes the playback assets.
///
/// `audio_ms` and `streams` are the take's own shape, so cost per minute of
/// audio is derivable — the ratio between the two halves inverts against a
/// live capture's stop, which did its Whisper work during the meeting.
#[derive(Clone, Serialize)]
pub struct ReplayTakeTimings {
    /// The take's 1-based position in the note, as every other surface names it.
    pub index: u32,
    pub streams: u32,
    pub audio_ms: u64,
    pub transcribe_ms: u64,
    pub diarize_ms: u64,
}

/// Wall-clock cost of one deferred-transcription run, per take (#188).
#[derive(Clone, Default, Serialize)]
pub struct ReplayTimings {
    pub takes: Vec<ReplayTakeTimings>,
    pub total_ms: u64,
}

impl ReplayTimings {
    /// One-line stderr summary of the run, printed once however it ends.
    pub fn summary(&self) -> String {
        let takes: Vec<String> = self
            .takes
            .iter()
            .map(|t| {
                format!(
                    "[{}: streams={} audio={} transcribe={} diarize={}]",
                    t.index, t.streams, t.audio_ms, t.transcribe_ms, t.diarize_ms
                )
            })
            .collect();
        format!(
            "replay timings: total={}ms takes={} {}",
            self.total_ms,
            self.takes.len(),
            takes.join(" ")
        )
    }
}

/// Put a `timings` object into the take's diagnostics JSON, keeping every
/// field already there. `existing` is the parsed file, or `None` when the
/// chain wrote no diagnostics (no diarize model, no chunks) — in which case
/// the timings stand alone in a fresh object.
pub fn merge_timings(
    existing: Option<serde_json::Value>,
    timings: &impl Serialize,
) -> serde_json::Value {
    let mut root = match existing {
        Some(serde_json::Value::Object(map)) => serde_json::Value::Object(map),
        _ => serde_json::json!({ "created_at": chrono::Utc::now().timestamp_millis() }),
    };
    if let Some(map) = root.as_object_mut() {
        map.insert(
            "timings".to_string(),
            serde_json::to_value(timings).unwrap_or(serde_json::Value::Null),
        );
    }
    root
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
///
/// Progress rides the same event: which half of the take's work is running
/// (`phase`, #188), the audio position (`done_ms` / `total_ms`) and which take
/// of how many, so the label and the fraction are separable and the client does
/// one division. All five are absent on the brackets and on a run with nothing
/// to report, which serializes exactly as it did before they existed.
///
/// The position stays on a `diarizing` event: it is where the next take
/// resumes, and the phase — not a missing measure — is what makes the bar
/// indeterminate.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscribeStatusPayload {
    pub note_id: String,
    pub active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<ReplayPhase>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub done_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub take: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub takes: Option<u32>,
}

impl TranscribeStatusPayload {
    /// The bracket around a run: active or not, nothing measured.
    pub fn bracket(note_id: &str, active: bool) -> Self {
        Self {
            note_id: note_id.to_string(),
            active,
            phase: None,
            done_ms: None,
            total_ms: None,
            take: None,
            takes: None,
        }
    }

    pub fn progress(note_id: &str, s: ReplaySnapshot) -> Self {
        Self {
            note_id: note_id.to_string(),
            active: true,
            phase: Some(s.phase),
            done_ms: Some(s.done_ms),
            total_ms: Some(s.total_ms),
            take: Some(s.take),
            takes: Some(s.takes),
        }
    }
}

/// Per-note (re)diarize lifecycle (#187), on its own channel because
/// `recording_status` describes the live capture and nothing else. Re-diarize
/// and the cross-session unify pass are user actions on an *arbitrary* note, so
/// a recording may be running on another note throughout — an `Idle` from here
/// would blank that recording's bar, timer, stop button and tray.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiarizeStatusPayload {
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
        // Display name of the audio input device the mic tap is actually on
        // (#174). `None` from a sidecar too old to report it, or when
        // CoreAudio can't name the device — the no-audio warning falls back
        // to its device-less copy rather than inventing a name.
        //
        // Deliberately not persisted anywhere: device names are user-authored
        // and routinely contain a real person's name ("Michael's AirPods").
        // This rides a transient event to a transient warning and stops there
        // — it must not reach a diagnostics dump or anything synced.
        #[serde(default)]
        input_device: Option<String>,
    },
    // Non-fatal notice from the sidecar — e.g. it recovered mic capture after
    // an audio device change mid-recording. Surfaced to the user as a transient
    // toast (see the reader loop in commands.rs).
    Diagnostic {
        message: String,
        // Device the sidecar resumed on, sent separately so `message` stays
        // free of it: this arm logs `message` to stderr, and a device name is
        // display-only (#174). The user-facing sentence is composed in the
        // reader loop from the two parts.
        #[serde(default)]
        input_device: Option<String>,
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
    // See `SidecarEvent::Heartbeat::input_device` — display only, never
    // persisted (#174).
    pub input_device: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_chunk() -> ChunkRecord {
        ChunkRecord {
            source: ChunkSource::Mic,
            start_ms: 0,
            text: "hello".into(),
            words: Vec::new(),
            detected_language: None,
        }
    }

    #[test]
    fn a_chunk_landing_during_the_drain_still_appends() {
        // #182: the tail of the transcript arrives as it decodes. Stop-pressed
        // keeps `note_id` set precisely so this stays true through the drain.
        assert!(appends_live(Some("n1"), StopState::Draining, "n1"));
        assert!(appends_live(Some("n1"), StopState::None, "n1"));
    }

    #[test]
    fn a_chunk_landing_after_the_snapshot_does_not_append() {
        // From the snapshot on, the post-stop chain rewrites the whole
        // transcript with speaker labels — a late append would land on top.
        assert!(!appends_live(None, StopState::Finishing, "n1"));
        // Even with the id somehow still set, `Finishing` is the answer.
        assert!(!appends_live(Some("n1"), StopState::Finishing, "n1"));
        // A different capture holds the slot: not this note's business.
        assert!(!appends_live(Some("n2"), StopState::None, "n1"));
        assert!(!appends_live(None, StopState::None, "n1"));
    }

    #[test]
    fn only_a_stop_in_progress_refuses_a_new_capture() {
        let mut cap = LiveCapture::default();
        cap.note_id = Some("n1".to_string());
        assert_eq!(cap.stop_in_progress(), None, "a live slot is takeable");
        cap.stop = StopState::Draining;
        assert_eq!(cap.stop_in_progress(), Some(STOP_IN_PROGRESS));
        cap.stop = StopState::Finishing;
        assert_eq!(cap.stop_in_progress(), Some(STOP_IN_PROGRESS));
        cap.stop = StopState::None;
        assert_eq!(cap.stop_in_progress(), None);
    }

    #[test]
    fn a_refused_caller_never_reaches_the_chunk_log() {
        // The invariant #182 protects: a new capture can never clear
        // `chunk_log` before the previous stop has snapshotted it. Both
        // `recording_start` and `import_audio` ask *first* and return before
        // they reset anything, which this stands in for.
        let take_the_slot = |cap: &mut LiveCapture| -> Result<(), &'static str> {
            if let Some(msg) = cap.stop_in_progress() {
                return Err(msg);
            }
            cap.sink = Arc::new(CaptureSink::new(SinkMode::Live));
            Ok(())
        };
        let mut cap = LiveCapture::default();
        cap.note_id = Some("n1".to_string());
        cap.sink.chunk_log.lock().push(a_chunk());
        for stop in [StopState::Draining, StopState::Finishing] {
            cap.stop = stop;
            assert_eq!(take_the_slot(&mut cap), Err(STOP_IN_PROGRESS), "{stop:?} must refuse");
            assert_eq!(cap.sink.chunk_log.lock().len(), 1, "{stop:?} lost the log");
        }
        // And the stand-in really does clear it once allowed to run, so the
        // survival above is the refusal's doing.
        cap.stop = StopState::None;
        assert_eq!(take_the_slot(&mut cap), Ok(()));
        assert!(cap.sink.chunk_log.lock().is_empty());
    }

    #[test]
    fn an_import_drain_holds_the_slot_like_a_live_stop_does() {
        // `finish_import` drains too, and its child and reader are already
        // finished — so without the same lifecycle `recording_start`'s
        // self-heal reads a zombie and takes over mid-drain.
        let mut cap = LiveCapture::default();
        cap.note_id = Some("n1".to_string());
        assert_eq!(cap.begin_stop(), Ok("n1".to_string()));
        assert_eq!(cap.stop, StopState::Draining);
        assert_eq!(cap.note_id.as_deref(), Some("n1"), "the tail still appends");
        assert_eq!(cap.stop_in_progress(), Some(STOP_IN_PROGRESS));
        assert_eq!(cap.begin_stop(), Err(STOP_IN_PROGRESS), "no second stop");
    }

    #[test]
    fn an_empty_slot_has_no_stop_to_begin() {
        let mut cap = LiveCapture::default();
        assert_eq!(cap.begin_stop(), Err("not recording"));
        assert_eq!(cap.stop, StopState::None);
    }

    #[test]
    fn stopping_carries_its_progress_and_no_other_phase_does() {
        let json = serde_json::to_string(&RecordingStatus::stopping_progress("n1", 3, 1)).unwrap();
        assert_eq!(json, r#"{"noteId":"n1","phase":"stopping","pending":3,"done":1}"#);

        // Every other emit goes through `emit_status`, which passes None for
        // both — the fields must vanish, so a listener reading `{noteId,
        // phase}` gets a payload with nothing else in it.
        assert_eq!(
            serde_json::to_string(&RecordingStatus::plain(None, Phase::Idle)).unwrap(),
            r#"{"noteId":null,"phase":"idle"}"#
        );
    }

    #[test]
    fn a_deferred_stop_is_marked_and_counts_nothing() {
        // #146: a capture that never transcribed as it ran has no chunks to
        // drain, so the drain's denominator must not be on the payload at all
        // — a bar drawn from it would appear and vanish in ~300ms.
        assert_eq!(
            serde_json::to_string(&RecordingStatus::deferred_phase("n1", Phase::Stopping)).unwrap(),
            r#"{"noteId":"n1","phase":"stopping","deferred":true}"#
        );
        assert_eq!(
            serde_json::to_string(&RecordingStatus::deferred_phase("n1", Phase::Diarizing))
                .unwrap(),
            r#"{"noteId":"n1","phase":"diarizing","deferred":true}"#
        );
        // And absent for a live one, whose full-bar zero-pending case is real.
        assert_eq!(
            serde_json::to_string(&RecordingStatus::stopping_progress("n1", 0, 0)).unwrap(),
            r#"{"noteId":"n1","phase":"stopping","pending":0,"done":0}"#
        );
    }

    #[test]
    fn a_transcribe_bracket_serializes_as_it_did_before_progress_existed() {
        assert_eq!(
            serde_json::to_string(&TranscribeStatusPayload::bracket("n1", true)).unwrap(),
            r#"{"noteId":"n1","active":true}"#
        );
        assert_eq!(
            serde_json::to_string(&TranscribeStatusPayload::bracket("n1", false)).unwrap(),
            r#"{"noteId":"n1","active":false}"#
        );
    }

    #[test]
    fn a_transcribe_progress_event_carries_its_phase_position_and_take_counter() {
        // Pinned whole, field by field: this is the only place the TS mirror in
        // `ipc.ts` can be checked against what serde actually emits.
        let snapshot = ReplaySnapshot {
            phase: ReplayPhase::Transcribing,
            done_ms: 45_000,
            total_ms: 180_000,
            take: 2,
            takes: 3,
        };
        assert_eq!(
            serde_json::to_string(&TranscribeStatusPayload::progress("n1", snapshot)).unwrap(),
            r#"{"noteId":"n1","active":true,"phase":"transcribing","doneMs":45000,"totalMs":180000,"take":2,"takes":3}"#
        );
        // The diarize half keeps the position — it is where the next take
        // resumes — and the phase, not a missing measure, is what makes the
        // bar indeterminate (#188).
        assert_eq!(
            serde_json::to_string(&TranscribeStatusPayload::progress(
                "n1",
                ReplaySnapshot { phase: ReplayPhase::Diarizing, ..snapshot }
            ))
            .unwrap(),
            r#"{"noteId":"n1","active":true,"phase":"diarizing","doneMs":45000,"totalMs":180000,"take":2,"takes":3}"#
        );
    }

    fn one_stream(duration_ms: u64) -> TakeWork {
        TakeWork { streams: 1, duration_ms }
    }

    fn transcribing(done_ms: u64, total_ms: u64, take: u32, takes: u32) -> ReplaySnapshot {
        ReplaySnapshot { phase: ReplayPhase::Transcribing, done_ms, total_ms, take, takes }
    }

    #[test]
    fn a_single_stream_take_is_worth_its_own_duration() {
        let mut p = ReplayProgress::new(vec![one_stream(60_000)]);
        assert_eq!(p.snapshot(), transcribing(0, 60_000, 1, 1));
        p.note_chunk(15_000);
        assert_eq!(p.snapshot().done_ms, 15_000);
        p.finish_stream();
        assert_eq!(p.snapshot().done_ms, 60_000, "a finished stream is fully done");
    }

    #[test]
    fn a_two_stream_take_costs_twice_its_duration() {
        // Each retained stream is its own pass over the same audio.
        let mut p = ReplayProgress::new(vec![TakeWork { streams: 2, duration_ms: 60_000 }]);
        assert_eq!(p.snapshot().total_ms, 120_000);
        p.note_chunk(30_000);
        assert_eq!(p.snapshot().done_ms, 30_000);
        // The mic→sys handover: the second pass starts back at 0 in its own
        // stream, and the fraction must not follow it there.
        p.finish_stream();
        assert_eq!(p.snapshot().done_ms, 60_000);
        p.note_chunk(0);
        assert_eq!(p.snapshot().done_ms, 60_000);
        p.note_chunk(20_000);
        assert_eq!(p.snapshot().done_ms, 80_000);
        p.finish_stream();
        assert_eq!(p.snapshot().done_ms, 120_000);
    }

    #[test]
    fn several_takes_sum_and_number_themselves() {
        let mut p = ReplayProgress::new(vec![
            one_stream(60_000),
            TakeWork { streams: 2, duration_ms: 30_000 },
            one_stream(10_000),
        ]);
        assert_eq!(p.snapshot(), transcribing(0, 130_000, 1, 3));
        p.begin_take(0);
        p.finish_stream();
        p.begin_take(1);
        assert_eq!(p.snapshot(), transcribing(60_000, 130_000, 2, 3));
        p.note_chunk(10_000);
        assert_eq!(p.snapshot().done_ms, 70_000);
        p.finish_stream();
        p.finish_stream();
        p.begin_take(2);
        assert_eq!(p.snapshot(), transcribing(120_000, 130_000, 3, 3));
        p.finish_stream();
        assert_eq!(p.snapshot().done_ms, 130_000);
    }

    #[test]
    fn a_take_boundary_is_exact_even_if_a_stream_never_reported() {
        // `begin_take` snaps to everything the takes before it were worth, so a
        // replay that ended without a `finish_stream` can't leave the fraction
        // permanently short.
        let mut p = ReplayProgress::new(vec![one_stream(60_000), one_stream(60_000)]);
        p.begin_take(1);
        assert_eq!(p.snapshot().done_ms, 60_000);
    }

    #[test]
    fn progress_never_goes_backwards() {
        let mut p = ReplayProgress::new(vec![one_stream(60_000)]);
        p.note_chunk(30_000);
        // Chunks land as their transcribes return, which is not guaranteed to
        // be in order, and a duplicate is harmless.
        p.note_chunk(12_000);
        assert_eq!(p.snapshot().done_ms, 30_000);
        p.note_chunk(30_000);
        assert_eq!(p.snapshot().done_ms, 30_000);
        p.begin_take(0);
        assert_eq!(p.snapshot().done_ms, 30_000, "re-entering a take keeps its position");
    }

    #[test]
    fn a_chunk_can_claim_no_more_than_its_own_stream() {
        // A manifest duration is best-effort and can be 0 or short; a chunk
        // beyond it must not eat the next take's share, and nothing may exceed
        // the run's total.
        let mut p = ReplayProgress::new(vec![one_stream(10_000), one_stream(10_000)]);
        p.note_chunk(999_000);
        assert_eq!(p.snapshot().done_ms, 10_000);
        p.begin_take(1);
        p.note_chunk(999_000);
        assert_eq!(p.snapshot().done_ms, 20_000);
    }

    #[test]
    fn a_take_of_unknown_length_advances_only_when_its_stream_ends() {
        let mut p = ReplayProgress::new(vec![one_stream(0), one_stream(60_000)]);
        assert_eq!(p.snapshot().total_ms, 60_000);
        p.note_chunk(5_000);
        assert_eq!(p.snapshot().done_ms, 0, "no share to spend");
        p.finish_stream();
        assert_eq!(p.snapshot().done_ms, 0);
        p.begin_take(1);
        p.note_chunk(30_000);
        assert_eq!(p.snapshot().done_ms, 30_000);
    }

    /// Every snapshot one run emits, in order. Mirrors `transcribe_takes`'
    /// loop — per take: `begin_take`, one chunk per stream, `finish_stream`
    /// per stream, then `begin_diarize` — so the sequence the client sees can
    /// be asserted without an `AppHandle`. Change both together.
    fn run_snapshots(plan: Vec<TakeWork>) -> Vec<ReplaySnapshot> {
        let mut p = ReplayProgress::new(plan.clone());
        let mut out = vec![p.snapshot()];
        for (i, take) in plan.iter().enumerate() {
            p.begin_take(i);
            out.push(p.snapshot());
            for _ in 0..take.streams {
                p.note_chunk(take.duration_ms / 2);
                out.push(p.snapshot());
                p.finish_stream();
                out.push(p.snapshot());
            }
            p.begin_diarize();
            out.push(p.snapshot());
        }
        out
    }

    #[test]
    fn a_multi_take_run_alternates_transcribing_and_diarizing() {
        let seq = run_snapshots(vec![
            TakeWork { streams: 2, duration_ms: 60_000 },
            one_stream(30_000),
        ]);
        // One diarize per take, each after that take's own replay, and the run
        // never ends on a transcribing phase.
        let mut steps: Vec<(ReplayPhase, u32)> = Vec::new();
        for s in &seq {
            if steps.last() != Some(&(s.phase, s.take)) {
                steps.push((s.phase, s.take));
            }
        }
        assert_eq!(
            steps,
            vec![
                (ReplayPhase::Transcribing, 1),
                (ReplayPhase::Diarizing, 1),
                (ReplayPhase::Transcribing, 2),
                (ReplayPhase::Diarizing, 2),
            ]
        );
    }

    #[test]
    fn the_fraction_never_decreases_across_a_diarize_boundary() {
        // The diarize half reports the position the replay reached, and the
        // next take resumes from it — a bar that dropped back to the previous
        // take's start would read as work being redone (#188).
        let seq = run_snapshots(vec![
            TakeWork { streams: 2, duration_ms: 60_000 },
            one_stream(30_000),
            TakeWork { streams: 2, duration_ms: 10_000 },
        ]);
        for pair in seq.windows(2) {
            assert!(
                pair[1].done_ms >= pair[0].done_ms,
                "{} then {}",
                pair[0].done_ms,
                pair[1].done_ms
            );
        }
        assert_eq!(seq.last().unwrap().done_ms, 170_000, "the whole run is spent");
    }

    #[test]
    fn a_replay_run_reports_both_halves_of_every_take() {
        let t = ReplayTimings {
            takes: vec![
                ReplayTakeTimings {
                    index: 1,
                    streams: 2,
                    audio_ms: 1_054_000,
                    transcribe_ms: 175_000,
                    diarize_ms: 108_000,
                },
                ReplayTakeTimings {
                    index: 2,
                    streams: 1,
                    audio_ms: 60_000,
                    transcribe_ms: 9_000,
                    diarize_ms: 4_000,
                },
            ],
            total_ms: 296_000,
        };
        assert_eq!(
            t.summary(),
            "replay timings: total=296000ms takes=2 \
             [1: streams=2 audio=1054000 transcribe=175000 diarize=108000] \
             [2: streams=1 audio=60000 transcribe=9000 diarize=4000]"
        );
        // The file is the same shape as a stop's, so `merge_timings` is shared.
        let merged = merge_timings(None, &t);
        let takes = merged["timings"]["takes"].as_array().unwrap();
        assert_eq!(takes.len(), 2);
        for key in ["index", "streams", "audio_ms", "transcribe_ms", "diarize_ms"] {
            assert!(takes[0][key].is_u64(), "{key} must be an integer");
        }
        assert!(merged["timings"]["total_ms"].is_u64());
    }

    #[test]
    fn timings_merge_into_the_diagnostics_json_without_displacing_it() {
        let t = StopTimings {
            sidecar_shutdown_ms: 120,
            reader_wait_ms: 4,
            drain_ms: 9_000,
            drain_pending_chunks: 3,
            keep_audio_ms: 40,
            detect_language_ms: 1,
            diarize_mic_ms: Some(21_000),
            diarize_sys_ms: None,
            playback_assets_ms: 300,
            finalize_session_ms: 12,
            unify_ms: 0,
            temp_cleanup_ms: 7,
            total_ms: 30_484,
            diagnostics_path: Some(PathBuf::from("/tmp/community1-mic.json")),
        };
        let existing = serde_json::json!({ "engine": "community1", "mic_segments": [] });
        let merged = merge_timings(Some(existing), &t);
        assert_eq!(merged["engine"], "community1");
        let timings = &merged["timings"];
        for key in [
            "sidecar_shutdown_ms",
            "reader_wait_ms",
            "drain_ms",
            "drain_pending_chunks",
            "keep_audio_ms",
            "detect_language_ms",
            "playback_assets_ms",
            "finalize_session_ms",
            "unify_ms",
            "temp_cleanup_ms",
            "total_ms",
        ] {
            assert!(timings[key].is_u64(), "{key} must be an integer millisecond count");
        }
        assert_eq!(timings["diarize_mic_ms"], 21_000);
        // A stream that wasn't diarized says nothing rather than zero.
        assert!(timings.get("diarize_sys_ms").is_none());
        // The merge target is bookkeeping for the writer, not part of the dump.
        assert!(timings.get("diagnostics_path").is_none());

        // No diarize diagnostics were written (no model, no chunks): the
        // timings still land, in an object of their own.
        let alone = merge_timings(None, &t);
        assert!(alone["timings"]["total_ms"].is_u64());
    }

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
            SidecarEvent::Diagnostic { message, input_device } => {
                assert_eq!(message, "device changed");
                assert_eq!(input_device, None);
            }
            _ => panic!("expected Diagnostic variant"),
        }
    }

    #[test]
    fn a_recovery_diagnostic_keeps_its_device_out_of_the_message() {
        // #174: the device name must not be interpolated into `message`, which
        // the reader loop logs to stderr verbatim. It arrives as its own field
        // and the user-facing sentence is composed from the two.
        let json = r#"{"event":"diagnostic","message":"Audio input device changed; microphone capture resumed.","input_device":"Michael's AirPods Pro"}"#;
        match serde_json::from_str::<SidecarEvent>(json).unwrap() {
            SidecarEvent::Diagnostic { message, input_device } => {
                assert!(!message.contains("AirPods"), "device leaked into the logged message");
                assert_eq!(input_device.as_deref(), Some("Michael's AirPods Pro"));
            }
            _ => panic!("expected Diagnostic variant"),
        }
    }

    #[test]
    fn heartbeat_carries_the_input_device_name() {
        // #174: the heartbeat names the input device the mic tap is actually
        // on, so the no-audio warning can say *which* device it isn't
        // hearing instead of "check your microphone".
        let json = r#"{"event":"heartbeat","mic_frames":1,"sys_frames":2,"chunks":0,"mic_peak":0.0,"sys_peak":0.1,"input_device":"AirPods Pro"}"#;
        match serde_json::from_str::<SidecarEvent>(json).unwrap() {
            SidecarEvent::Heartbeat { input_device, .. } => {
                assert_eq!(input_device.as_deref(), Some("AirPods Pro"));
            }
            _ => panic!("expected Heartbeat variant"),
        }
    }

    #[test]
    fn heartbeat_without_an_input_device_still_deserializes() {
        // The sidecar binary is SHA-stamp cached and bundled separately from
        // the Rust build, so a newer app genuinely can meet an older sidecar
        // that emits no `input_device`. That must degrade to "unknown
        // device" (the warning falls back to its old copy), never to a
        // dropped heartbeat — the heartbeat also carries the level meters
        // and the chunk counter that reveal this whole class of fault.
        let json = r#"{"event":"heartbeat","mic_frames":1,"sys_frames":2,"chunks":0,"mic_peak":0.0,"sys_peak":0.1}"#;
        match serde_json::from_str::<SidecarEvent>(json).unwrap() {
            SidecarEvent::Heartbeat { input_device, mic_frames, .. } => {
                assert_eq!(input_device, None);
                assert_eq!(mic_frames, 1);
            }
            _ => panic!("expected Heartbeat variant"),
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
