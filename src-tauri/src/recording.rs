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

/// Per-chunk metadata captured during recording. The diarization step needs
/// to align speaker segments (timestamps relative to the full recording)
/// against chunk-level transcripts; this log holds the link between
/// "chunk N's text" and "chunk N started at start_ms in the full audio".
#[derive(Clone, Debug)]
pub struct ChunkRecord {
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
    // Rolling context window of the last ~150 committed words. Fed to
    // Whisper's `initial_prompt` for every chunk so decoding stays anchored
    // to the conversation rather than treating each chunk as a cold start.
    pub trail: Arc<Mutex<TranscriptTrail>>,
    // Per-chunk metadata. Retained for diagnostic / future hybrid use;
    // current live-diarization path doesn't read it.
    pub chunk_log: Arc<Mutex<Vec<ChunkRecord>>>,
    // Path to the full-recording WAV file. Currently unused (live
    // diarization classifies per-chunk), but kept around in case we ever
    // want a stop-time correction pass.
    pub full_wav_path: Arc<Mutex<Option<PathBuf>>>,
    // Maps FluidAudio's stable speaker_id (e.g. "1", "2") to a 1-indexed
    // display number assigned in first-encounter order. Cleared on
    // recording_start. Used by the live-diarization path to render
    // "Speaker 1: " / "Speaker 2: " prefixes consistently across chunks.
    pub speaker_display: Arc<Mutex<std::collections::HashMap<String, u32>>>,
    // The speaker_id of the last committed chunk. Used to decide whether
    // a new chunk continues the current speaker (just append) or starts
    // a new turn (newline + "Speaker N: " prefix). None at start.
    pub last_speaker: Arc<Mutex<Option<String>>>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingStatus {
    pub note_id: Option<String>,
    pub phase: Phase,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Idle,
    Starting,
    Recording,
    Paused,
    Stopping,
    Diarizing,
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
pub struct ErrorPayload {
    pub note_id: Option<String>,
    pub message: String,
}

#[derive(serde::Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum SidecarEvent {
    Chunk {
        path: String,
        // Time (in milliseconds) at which this chunk's audio starts relative
        // to the beginning of the recording. Defaults to 0 for older sidecar
        // builds that didn't emit this — the diarization step will treat
        // start-less chunks as if they all start at 0, which collapses the
        // alignment to "best effort".
        #[serde(default)]
        start_ms: u64,
    },
    FullRecording {
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
}
