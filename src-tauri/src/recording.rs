use parking_lot::Mutex;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::process::Child;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

pub type Inflight = Arc<Mutex<Vec<JoinHandle<()>>>>;

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
    Chunk { path: String },
    Error { message: String },
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
