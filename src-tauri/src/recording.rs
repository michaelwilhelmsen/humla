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
}

#[derive(Default)]
pub struct RecordingSession {
    pub note_id: Option<String>,
    pub child: Option<Child>,
    pub temp_dir: Option<PathBuf>,
    pub stop_tx: Option<mpsc::Sender<()>>,
    // Handles for in-flight transcribe tasks. Drained on stop so the
    // transcript is fully written before we flip to Idle.
    pub inflight: Inflight,
    // Handle for the stdout reader task that spawns transcribes. Awaiting
    // it guarantees no further pushes to `inflight` are coming.
    pub reader: Option<JoinHandle<()>>,
    // Per-source rolling context windows of the last ~150 committed words.
    // Fed to Whisper's `initial_prompt` for every chunk so decoding stays
    // anchored to its own stream rather than mixing the user's side with
    // the remote side's vocabulary, which would harm proper-noun spelling
    // and pull each Whisper invocation toward the wrong language.
    pub mic_trail: Arc<Mutex<TranscriptTrail>>,
    pub sys_trail: Arc<Mutex<TranscriptTrail>>,
    // Per-chunk metadata. Read by the offline diarization pass on
    // recording_stop to align FluidAudio's speaker segments back to the
    // chunks the user saw stream in.
    pub chunk_log: Arc<Mutex<Vec<ChunkRecord>>>,
    // Paths to the per-source full-recording WAV files. Consumed by the
    // offline diarization pass on stop, then deleted alongside the temp dir.
    // Either may be `None` if its source produced no audio (mic permission
    // denied, no system audio active for the whole recording, etc).
    pub mic_full_wav_path: Arc<Mutex<Option<PathBuf>>>,
    pub sys_full_wav_path: Arc<Mutex<Option<PathBuf>>>,
    // Snapshot of the note's transcript at recording_start. Used by the
    // offline diarization step to prepend prior content to this session's
    // diarized output, so resuming a recording adds to the transcript
    // instead of clobbering it. Empty string means "fresh recording, no
    // prior content."
    pub transcript_at_start: Arc<Mutex<String>>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingStatus {
    pub note_id: Option<String>,
    pub phase: Phase,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Idle,
    Starting,
    Recording,
    Paused,
    Stopping,
    Diarizing,
    Retranscribing,
    Polishing,
    Summarizing,
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
}
