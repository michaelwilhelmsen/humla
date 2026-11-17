use crate::db;
use crate::diarize;
use crate::local_whisper;
use crate::sessions;
use crate::recording::{CaptureSink, ChunkRecord, ChunkSource, DiagnosticPayload, ErrorPayload, Inflight, Phase, RecordingStatus, ReplayProgress, SidecarEvent, SinkMode, StartClaim, Step, StepReport};
use crate::AppState;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::Semaphore;

// Command handlers are split into submodules under `commands/`. Each holds a
// cohesive group of #[tauri::command] fns and is re-exported with `pub use`
// so the `commands::<name>` paths in lib.rs's generate_handler! stay unchanged;
// a module that holds no commands isn't re-exported.
// Shared helpers + constants (err, DEFAULT_*, emit_*, key/config readers) stay
// in this root module and are reached from submodules via `super::` / `crate::`.
mod api_keys;
mod assets;
mod chat;
mod clients;
mod copilot;
mod deferred;
mod diarization;
// `pub(crate)` so `crate::menubar` can resolve the active workspace when it
// creates a note for a headless recording.
pub(crate) mod cloud;
pub(crate) mod cloud_auth;
mod export;
// Live cloud-sync worker glue. Behind the `cloud` feature; `pub` so `run()` can
// reach `cloud_worker::install`. Not a #[tauri::command] group, so no glob
// re-export below.
#[cfg(feature = "cloud")]
pub mod cloud_worker;
mod folders;
mod import;
mod local_llm;
mod mcp;
mod menubar;
mod models;
pub(crate) mod notes;
mod permissions;
mod playback;
mod settings;
mod summary;
mod summary_prompts;
mod telemetry;
mod timeline;
mod title;
mod transcribe;
mod transcript;
mod transcription_config;
mod unify;
mod webhook;
pub use api_keys::*;
pub use assets::*;
pub use chat::*;
pub use clients::*;
pub use copilot::*;
pub use deferred::*;
pub use diarization::*;
pub use cloud::*;
pub use cloud_auth::*;
pub use export::*;
pub use folders::*;
pub use import::*;
pub use local_llm::*;
pub use mcp::*;
pub use menubar::*;
pub use models::*;
pub use notes::*;
pub use permissions::*;
pub use settings::*;
pub use summary::*;
pub use telemetry::*;
pub use timeline::*;
pub use title::*;
pub use summary_prompts::*;
pub use transcription_config::*;
pub use webhook::*;

// Shared helpers that moved into submodules but are still called by the
// recording / diarize / summary code remaining in this root module.
use api_keys::read_provider_api_key;
use models::local_model_path;
use playback::write_playback_assets;
use transcription_config::read_transcribe_config;
use transcribe::{record_detected_language, transcribe_chunk};
use transcript::{
    absorb_text_continuation_chains, bridge_short_interjections, dedup_mic_against_sys,
    estimated_speech_ms, LabelledPiece,
};
use diarization::{diarize_and_apply, offset_speaker_label};
use unify::unify_note_speakers;

pub(crate) const DEFAULT_LANGUAGE: &str = "no";
// The diarization engine when `diarize_model` is unset. Only a fresh install
// has no row, since `db::migrate_diarize_engine` writes one for every other.
// Mirrored by `DEFAULT_DIARIZE_ENGINE` in `src/lib/diarizeEngine.ts`.
const DEFAULT_DIARIZE_MODEL: &str = "nemotron3";

// Matches the sidecar's own default. Stored as a string and parsed at use.
const DEFAULT_COMMUNITY1_THRESHOLD: &str = "0.5";

// Silent-chunk gate. RMS below this is dropped before transcription so
// Whisper / gpt-4o-transcribe don't hallucinate confident text on silence.
// Was 0.008 originally — too aggressive for users sitting >40 cm from a
// MacBook mic at default gain (their soft speech lands at ~0.007). 0.005
// still rejects pure silence (~0.0001), room tone (~0.001), and mic hiss
// (~0.003) while admitting normal soft speech. Tunable via the
// silence_rms_threshold setting for noisy environments.
const DEFAULT_SILENCE_RMS_THRESHOLD: f32 = 0.005;

// `keep_audio` defaults off. Its value is read through
// `sessions::retain_audio` (which owns the default) via `keep_audio_enabled`;
// off means recordings live in the temp dir for the post-stop pipeline and are
// deleted at the end, with no playback.wav and no source copies left behind.

// Bounded backlog for the import reader loop. An `--import` replay emits chunks
// at full disk speed — far faster than realtime — so without a bound the reader
// would spawn a transcribe task per chunk and a multi-hour file would pile up
// hundreds of parked tasks doing eager pre-gate work (silence RMS read, config
// lookups) while they queue behind `transcribe_gate`. The reader acquires a
// permit before spawning each chunk's task and the task releases it on
// completion, capping the parked backlog. Transcription quality is unchanged —
// the tasks still serialise on `transcribe_gate` exactly as live recording does.
const IMPORT_BACKLOG_PERMITS: usize = 16;

const DEFAULT_SUMMARY_MODEL: &str = "gpt-5.4-mini";
/// The cloud model an automatic title call falls back to when the configured
/// summary model reasons (#90) — which the default above does, so this is the
/// model the default configuration actually titles with.
///
/// Pinned rather than derived: a reasoning model would spend thousands of
/// invisible reasoning tokens on five words, in a call the user never asked
/// for. It has to be a model every OpenAI key can reach, since a failure here
/// is silent by design and would present as titling simply never happening.
pub(crate) const TITLE_FALLBACK_MODEL: &str = "gpt-4o-mini";
// Ollama's default port + OpenAI-compat path. Any user running LM Studio,
// llama-server, or vLLM will override this in Settings.
const DEFAULT_LOCAL_LLM_BASE_URL: &str = "http://localhost:11434/v1";
fn err<E: std::fmt::Display>(e: E) -> String { e.to_string() }

/// A channel one chain's progress goes on.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Channel {
    /// The global `recording_status`.
    RecordingStatus,
    /// The per-note `diarize_status`.
    DiarizeStatus,
    /// The per-note `transcribe_status`.
    TranscribeStatus,
    /// None.
    None,
}

/// Where one chain reports — both the bracket saying it is in flight and the
/// named steps inside it.
///
/// Passed in the way [`StopClock`] is: the diarize pass and the unify pass are
/// shared by the live stop, the user-pressed Re-diarize and a deferred replay,
/// so a callee must never choose the channel. Bracket and steps live on one
/// type because they are one answer — held apart, a caller can bracket on one
/// channel and name steps on another.
///
/// `recording_status` describes the live capture and nothing else (#187): it is
/// a single global slot that also drives the tray icon and its Start/Stop
/// items, so a pass the user can start on an *arbitrary* note must stay off it —
/// its `Idle` would blank a recording running on a different note.
#[derive(Clone)]
pub(crate) enum ChainProgress {
    /// The post-stop chain of the capture that just finished, on the global
    /// phase it already holds.
    PostStop { note_id: String },
    /// The user pressed Re-diarize: bracketed per-note, with no measure.
    Rediarize { note_id: String },
    /// A deferred replay, on the per-note channel where the run's audio
    /// position and take counter already ride. Unbracketed — the command
    /// brackets the whole run.
    Replay {
        note_id: String,
        progress: Arc<parking_lot::Mutex<ReplayProgress>>,
    },
    /// Nothing. A deferred stop dispatched no chunks and lands on idle in a
    /// few hundred milliseconds, so a bar drawn for it appears and vanishes
    /// without having measured anything.
    Silent,
}

impl ChainProgress {
    /// The one channel this chain reports on, bracket and steps alike.
    #[cfg(test)]
    pub(crate) fn channel(&self) -> Channel {
        match self {
            Self::PostStop { .. } => Channel::RecordingStatus,
            Self::Rediarize { .. } => Channel::DiarizeStatus,
            Self::Replay { .. } => Channel::TranscribeStatus,
            Self::Silent => Channel::None,
        }
    }

    /// The pass is in flight.
    fn started(&self, app: &AppHandle) {
        match self {
            Self::PostStop { note_id } => emit_status(app, Some(note_id), Phase::Diarizing),
            Self::Rediarize { note_id } => emit_diarize_status(app, note_id, true),
            Self::Replay { .. } | Self::Silent => {}
        }
    }

    /// Clear it again, on every exit path.
    fn finished(&self, app: &AppHandle) {
        match self {
            Self::PostStop { .. } => emit_status(app, None, Phase::Idle),
            Self::Rediarize { note_id } => emit_diarize_status(app, note_id, false),
            Self::Replay { .. } | Self::Silent => {}
        }
    }

    fn report(&self, app: &AppHandle, r: StepReport) {
        match self {
            Self::PostStop { note_id } => {
                let _ = app.emit(
                    "recording_status",
                    RecordingStatus::chain_step(note_id, r.step, r.counter),
                );
            }
            Self::Replay { note_id, progress } => {
                let snapshot = {
                    let mut p = progress.lock();
                    p.begin_step(r.step, r.counter);
                    p.snapshot()
                };
                emit_transcribe_progress(app, note_id, snapshot);
            }
            Self::Rediarize { .. } | Self::Silent => {}
        }
    }

    fn step(&self, app: &AppHandle, step: Step) {
        self.report(app, StepReport::plain(step));
    }
}

/// Names one step over the per-source streams a pass actually has.
struct StreamSteps<'a> {
    app: &'a AppHandle,
    progress: &'a ChainProgress,
    reports: std::vec::IntoIter<StepReport>,
}

impl<'a> StreamSteps<'a> {
    /// `present` is the streams in the order the caller walks them. The count
    /// comes from what is there, never from a literal — see
    /// [`crate::recording::stream_reports`].
    fn new(
        app: &'a AppHandle,
        progress: &'a ChainProgress,
        step: Step,
        present: &[bool],
    ) -> Self {
        let streams = present.iter().filter(|p| **p).count() as u32;
        Self {
            app,
            progress,
            reports: crate::recording::stream_reports(step, streams).into_iter(),
        }
    }

    /// The next present stream is starting. Runs out rather than counting past
    /// the streams it was given.
    fn begin(&mut self) {
        if let Some(r) = self.reports.next() {
            self.progress.report(self.app, r);
        }
    }
}

// ---- Speaker diarization model management ---------------------------------

/// The diarization engine selected in Settings.
pub(crate) fn selected_diarize_engine(state: &State<AppState>) -> diarize::Engine {
    let conn = state.db.lock();
    let id = db::get_setting(&conn, "diarize_model")
        .ok()
        .flatten()
        .unwrap_or_else(|| DEFAULT_DIARIZE_MODEL.to_string());
    diarize::Engine::from_setting(&id)
}

/// Snapshot of the session-derived data the post-stop chain needs.
/// Captured in `recording_stop` *after* the reader thread finishes (so
/// chunk_log and the full-WAV paths are stable) and moved into the
/// spawned task. This decouples the post-stop chain from
/// `state.recording`, so a new recording starting before diarization
/// finishes can't make the in-flight task see the next session's
/// chunks or WAV paths.
#[derive(Clone)]
struct PostStopSnapshot {
    mic_wav: Option<PathBuf>,
    sys_wav: Option<PathBuf>,
    chunks: Vec<ChunkRecord>,
    transcript_at_start: String,
    // Persisted-session identity for this capture (allocated at
    // recording_start). The post-stop chain writes this capture's assets
    // into `recordings/<note_id>/<session_id>/` and appends a manifest entry.
    session_id: String,
    session_started_at: String,
    /// This capture ran with "Transcribe manually" on (#146): its audio is
    /// retained and its chunks were never transcribed, so the post-stop chain
    /// records the take as untranscribed and skips every step that needs text.
    deferred: bool,
    /// How long the sidecar said this capture ran. Only consulted when the
    /// take has no timeline to measure instead — see `finalize_session`.
    captured_duration_ms: u64,
    /// How the capture's two streams lined up, when the sidecar reported it.
    /// `None` for an import, and from a sidecar that predates the event.
    capture_timing: Option<crate::recording::CaptureTiming>,
}

/// Copy the temp-dir full WAVs to a permanent location keyed by
/// note_id when the user has opted into audio retention. Called from
/// the post-stop chain *before* diarize_and_apply consumes and deletes
/// the temp files via cleanup_full_wav. Best-effort: individual copy
/// failures log and proceed.
/// Resolve (and create) the write target for this capture's session assets:
/// `recordings/<note_id>/<session_id>/`. `None` when the app data dir is
/// unavailable.
async fn session_write_dir(app: &AppHandle, note_id: &str, session_id: &str) -> Option<PathBuf> {
    let app_dir = app.path().app_data_dir().ok()?;
    let recordings = sessions::recordings_dir(&app_dir, note_id);
    // Resolve through the shared write resolver so a legacy note's assets land
    // flat (where the read path reads them), not in an orphan `__legacy__`
    // subdir. See `sessions::session_write_dir`.
    let target = sessions::session_write_dir(&recordings, session_id);
    if let Err(e) = tokio::fs::create_dir_all(&target).await {
        eprintln!("sessions: mkdir {}: {e}", target.display());
        return None;
    }
    Some(target)
}

/// Which streams the sidecar actually wrote a full recording for. Used in
/// place of [`session_streams`] for a take captured with "Transcribe manually"
/// on (#146), which has no chunks to read the answer off.
fn retained_streams(snapshot: &PostStopSnapshot) -> Vec<String> {
    let mut out = Vec::new();
    if snapshot.mic_wav.is_some() {
        out.push("mic".to_string());
    }
    if snapshot.sys_wav.is_some() {
        out.push("sys".to_string());
    }
    out
}

/// Which streams produced chunks in this capture (`["mic"]`, `["sys"]`, or
/// `["mic","sys"]`). Stored on the manifest entry for the carousel tooltip.
fn session_streams(chunks: &[ChunkRecord]) -> Vec<String> {
    let mut out = Vec::new();
    if chunks.iter().any(|c| c.source == ChunkSource::Mic) {
        out.push("mic".to_string());
    }
    if chunks.iter().any(|c| c.source == ChunkSource::Sys) {
        out.push("sys".to_string());
    }
    out
}

/// Ensure the note's storage is session-shaped before a new take is written,
/// and report how many sessions already existed. Migrates a pre-feature flat
/// note into a session subdir on its second recording so the first take
/// survives (best-effort). Runs the blocking FS work off the async runtime.
async fn prepare_sessions_for_new_take(app: &AppHandle, note_id: &str) -> usize {
    let Ok(app_dir) = app.path().app_data_dir() else {
        return 0;
    };
    let note_id = note_id.to_string();
    // Serialize the manifest read-modify-write against the cloud pull worker
    // (both rewrite sessions.json). Guard is Send + held across spawn_blocking.
    let lock = app.state::<AppState>().manifest_lock.clone();
    let _manifest_guard = lock.lock().await;
    tokio::task::spawn_blocking(move || {
        let recordings = sessions::recordings_dir(&app_dir, &note_id);
        // Only a flat, manifest-less note migrates; brand-new notes and
        // already-migrated notes are no-ops.
        let legacy_id = uuid::Uuid::new_v4().to_string();
        if let Err(e) = sessions::migrate_flat_if_needed(&recordings, &legacy_id) {
            eprintln!("sessions: migrate flat {}: {e}", recordings.display());
        }
        sessions::existing_session_count(&recordings)
    })
    .await
    .unwrap_or(0)
}

/// Append this finished take to the note's `sessions.json` manifest. Duration
/// is read back from the timeline `diarize_and_apply` just wrote (max end_ms)
/// so it reflects the actual content; 0 when the timeline is missing/empty.
/// Best-effort — a missing manifest entry only hides the take from the
/// carousel, it never loses the audio.
async fn finalize_session(
    app: &AppHandle,
    note_id: &str,
    session_id: &str,
    started_at: &str,
    streams: Vec<String>,
    // What the sidecar said this capture ran for, used only when the take has
    // no timeline to measure instead (#146). A "Transcribe manually" take
    // finalises before anything has been transcribed, so the timeline is
    // absent and this is its only source of a length — and it has to be
    // settled now, because the sync engine treats a session's `duration_ms` as
    // immutable and derives its last-write-wins key from `started_at` on that
    // basis (`cloud-sync`'s `push_session`).
    captured_duration_ms: u64,
    // Whether this take's audio has already been through the provider (#146).
    // `No` only for a capture made with "Transcribe manually" on, which the
    // note's Transcribe action picks up later.
    transcribed: sessions::Transcribed,
) {
    let Ok(app_dir) = app.path().app_data_dir() else {
        return;
    };
    let recordings = sessions::recordings_dir(&app_dir, note_id);
    // The timeline wins where there is one: it measures content, and trailing
    // silence isn't worth showing as part of a take's length.
    let duration_ms = match timeline_duration_ms(&sessions::session_write_dir(&recordings, session_id)) {
        0 => captured_duration_ms,
        from_timeline => from_timeline,
    };
    let session_id = session_id.to_string();
    let started_at = started_at.to_string();
    // Serialize the append (read-modify-write of sessions.json) against a
    // concurrent cloud pull reconcile so neither loses the other's session.
    let lock = app.state::<AppState>().manifest_lock.clone();
    let _manifest_guard = lock.lock().await;
    let res = tokio::task::spawn_blocking(move || {
        sessions::append_session(
            &recordings,
            &session_id,
            &started_at,
            duration_ms,
            streams,
            transcribed,
        )
    })
    .await;
    if let Err(e) = res.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.to_string())) {
        eprintln!("sessions: finalize {note_id}: {e}");
    }
}

/// Drop a take that transcribed nothing, when it also kept no audio: remove its
/// session dir so it never reaches the manifest, the carousel or the cloud.
/// Returns true when the take was discarded, false when it's worth keeping
/// (audio present) and the caller should finalize it as normal.
///
/// The legacy flat layout is never touched — a legacy session id resolves to the
/// note's whole recordings dir, and deleting that would take every take with it.
async fn discard_empty_take(app: &AppHandle, note_id: &str, session_id: &str) -> bool {
    if session_id == sessions::LEGACY_SESSION_ID {
        return false;
    }
    let Ok(app_dir) = app.path().app_data_dir() else {
        return false;
    };
    let recordings = sessions::recordings_dir(&app_dir, note_id);
    let dir = sessions::session_dir(&recordings, session_id);
    if sessions::session_has_audio(&dir) {
        return false; // nothing transcribed, but there's something to listen to
    }
    if let Err(e) = tokio::fs::remove_dir_all(&dir).await {
        // Absent is the common case (nothing was ever written for this take).
        if e.kind() != std::io::ErrorKind::NotFound {
            eprintln!("sessions: discard empty take {session_id}: {e}");
        }
    }
    eprintln!("sessions: discarded empty take {session_id} (no transcript, no audio)");
    true
}

/// Highest `end_ms` across a session dir's `timeline.jsonl`, i.e. the take's
/// wall-clock length. 0 when the file is absent or empty.
fn timeline_duration_ms(session_dir: &std::path::Path) -> u64 {
    let path = session_dir.join("timeline.jsonl");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return 0;
    };
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|v| v.get("end_ms").and_then(|e| e.as_u64()))
        .max()
        .unwrap_or(0)
}

fn raw_setting(app: &AppHandle, key: &str) -> Option<String> {
    let state: State<AppState> = app.state();
    let conn = state.db.lock();
    db::get_setting(&conn, key).ok().flatten()
}

/// Read the device's `keep_audio` decision (#24). The single gate consulted by
/// every path that would write, upload or download a WAV.
pub(crate) fn keep_audio_enabled(app: &AppHandle) -> bool {
    sessions::retain_audio(raw_setting(app, "keep_audio").as_deref())
}

/// Whether a capture starting now should keep the Mac from idle sleep (#180).
/// Read once at `recording_start` and handed to the sidecar as a launch flag;
/// flipping the setting mid-recording leaves the running capture alone.
pub(crate) fn keep_awake_enabled(app: &AppHandle) -> bool {
    sessions::keep_awake(raw_setting(app, "keep_awake").as_deref())
}

/// Whether a recording starting now should defer transcription (#146). Reads
/// both halves of the decision — see [`sessions::defer_transcription`] for why
/// `keep_audio` gates it.
pub(crate) fn deferred_transcription_enabled(app: &AppHandle) -> bool {
    let manual = raw_setting(app, "transcribe_manually");
    let keep = raw_setting(app, "keep_audio");
    sessions::defer_transcription(manual.as_deref(), sessions::retain_audio(keep.as_deref()))
}

async fn maybe_keep_audio(
    app: &AppHandle,
    note_id: &str,
    snapshot: &PostStopSnapshot,
    progress: &ChainProgress,
) {
    // No exceptions above the setting — not even #16's "second take force-
    // retains the sources so #17 can unify them". See `sessions::retain_audio`.
    if !keep_audio_enabled(app) {
        // Chunk timings are text and stay: re-diarize needs them as its
        // alignment anchor, and they cost nothing to keep. Without the WAVs
        // they can't resurrect audio, only re-label a transcript.
        if let Some(target) = session_write_dir(app, note_id, &snapshot.session_id).await {
            write_chunks_json(&target, &snapshot.chunks).await;
        }
        return;
    }
    let mic_wav = snapshot.mic_wav.clone();
    let sys_wav = snapshot.sys_wav.clone();
    let Some(target) = session_write_dir(app, note_id, &snapshot.session_id).await else {
        eprintln!("keep_audio: session dir unavailable");
        return;
    };
    // A stream at a time, named as it goes: each copy is tens of megabytes.
    let mut copy_steps = StreamSteps::new(
        app,
        progress,
        Step::SavingAudio,
        &[mic_wav.is_some(), sys_wav.is_some()],
    );
    if let Some(src) = mic_wav {
        copy_steps.begin();
        if let Err(e) = tokio::fs::copy(&src, target.join("mic.wav")).await {
            eprintln!("keep_audio: copy mic: {e}");
        }
    }
    if let Some(src) = sys_wav {
        copy_steps.begin();
        if let Err(e) = tokio::fs::copy(&src, target.join("sys.wav")).await {
            eprintln!("keep_audio: copy sys: {e}");
        }
    }
    // Persist chunk timings alongside the audio so re-diarize survives a
    // failed/skipped diagnostic write. Read back via `read_chunks_for_note`.
    write_chunks_json(&target, &snapshot.chunks).await;
}

/// Serialize the recording's chunk log to `<target>/chunks.json` in the
/// same shape `parse_chunks_json` reads. Best-effort: write failures log
/// and proceed (the dump is opportunistic — losing it costs re-diarize
/// for that note but doesn't break anything live).
async fn write_chunks_json(target: &std::path::Path, chunks: &[ChunkRecord]) {
    let payload: Vec<serde_json::Value> = chunks
        .iter()
        .map(|c| {
            let source = match c.source {
                ChunkSource::Mic => "mic",
                ChunkSource::Sys => "sys",
            };
            let words: Vec<_> = c
                .words
                .iter()
                .map(|w| {
                    serde_json::json!({
                        "text": w.text,
                        "start_ms": w.start_ms,
                        "end_ms": w.end_ms,
                    })
                })
                .collect();
            serde_json::json!({
                "source": source,
                "start_ms": c.start_ms,
                "text": c.text,
                "words": words,
            })
        })
        .collect();
    let doc = serde_json::json!({ "chunks": payload });
    let body = match serde_json::to_string_pretty(&doc) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("chunks_json: serialize: {e}");
            return;
        }
    };
    let path = target.join("chunks.json");
    if let Err(e) = tokio::fs::write(&path, body).await {
        eprintln!("chunks_json: write {}: {e}", path.display());
    }
}

#[tauri::command]
pub fn recording_pause(state: State<AppState>) -> Result<(), String> {
    let s = state.recording.lock();
    let child = s.child.as_ref().ok_or("not recording")?;
    let pid = child.id().ok_or("no pid")? as i32;
    #[cfg(unix)]
    unsafe {
        if libc::kill(pid, libc::SIGUSR1) != 0 {
            return Err(format!("kill: {}", std::io::Error::last_os_error()));
        }
    }
    Ok(())
}

#[tauri::command]
pub fn recording_resume(state: State<AppState>) -> Result<(), String> {
    let s = state.recording.lock();
    let child = s.child.as_ref().ok_or("not recording")?;
    let pid = child.id().ok_or("no pid")? as i32;
    #[cfg(unix)]
    unsafe {
        if libc::kill(pid, libc::SIGUSR2) != 0 {
            return Err(format!("kill: {}", std::io::Error::last_os_error()));
        }
    }
    Ok(())
}

#[tauri::command]
pub fn recording_state(state: State<AppState>) -> Result<&'static str, String> {
    let s = state.recording.lock();
    // A stop holds the slot until its chain lands on Idle (#182), and `note_id`
    // stays set through the drain — so the stop state, not the id, is what says
    // whether a capture can start.
    if s.stop_in_progress().is_some() {
        return Ok("stopping");
    }
    Ok(if s.note_id.is_some() { "recording" } else { "idle" })
}

/// Stop a sidecar left behind by a session that never went through
/// `recording_stop`: SIGTERM, a short grace, then kill.
async fn stop_stale_child(mut c: tokio::process::Child) {
    if let Some(pid) = c.id() {
        #[cfg(unix)]
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
    }
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), c.wait()).await;
    let _ = c.kill().await;
}

#[tauri::command]
pub async fn recording_start(
    app: AppHandle,
    state: State<'_, AppState>,
    note_id: String,
) -> Result<(), String> {
    // Claimed before any of the checks below, which can take seconds: a second
    // start inside that gap must find the slot taken, or its sidecar would
    // replace this one's. A stop in progress refuses too (#182), since the
    // self-heal reads a taken `child` as a dead one and would clear
    // `chunk_log` before that stop had snapshotted it.
    let (claim, stale_child) = StartClaim::take(&state.recording, "already recording")?;
    if let Some(c) = stale_child {
        stop_stale_child(c).await;
    }

    // A deferred transcription on *this* note is the one thing that can't
    // overlap a recording of it (#146): it rebuilds `note.transcript` from the
    // note's timelines while the capture appends to it live, and the post-stop
    // rewrite would then combine against a snapshot the replay had already
    // replaced. A replay on any *other* note is deliberately no obstacle —
    // that is why it never takes the capture slot.
    if state.transcribing.lock().contains(&note_id) {
        let msg = "This note is being transcribed — wait for that to finish before recording it again.".to_string();
        emit_error(&app, Some(&note_id), &msg);
        return Err(msg);
    }

    // Pre-check the configured provider's prerequisites (API key / local model
    // present) and fire the local-Whisper prewarm. Shared with `import_audio`.
    //
    // Still checked when transcription is deferred (#146): the provider that
    // will run later is the one configured now, and finding out at Transcribe
    // time that no model is downloaded would mean the meeting was captured
    // against a pipeline that was never going to work.
    let provider_cfg = ensure_provider_ready(&app, &state, &note_id).await?;

    // "Transcribe manually" (#146), decided once per recording: flipping the
    // setting mid-meeting must not change a capture already running, since half
    // a transcript is worse than either regime.
    //
    // Only for on-device Whisper. What the setting buys is a Mac that isn't
    // running Metal inference for the length of a meeting; a cloud provider
    // dispatches over the network and costs the machine nothing, so deferring
    // there would trade a live transcript for no saving at all. Resolved
    // through the note's own language, so a per-language override that routes
    // this note to Deepgram keeps its live transcript while a Norwegian note on
    // local Whisper defers.
    let sink_mode = if matches!(provider_cfg, crate::stt::ProviderConfig::Local(_))
        && deferred_transcription_enabled(&app)
    {
        SinkMode::Deferred
    } else {
        SinkMode::Live
    };

    // Pre-check microphone permission — without it we can't capture anything useful.
    if let Ok(p) = capture_permissions(&app).await {
        if p.microphone != "granted" {
            let msg = "Microphone permission required. Open Settings → Permissions to grant.".to_string();
            emit_error(&app, Some(&note_id), &msg);
            return Err(msg);
        }
        if p.screen != "granted" {
            emit_error(&app, Some(&note_id),
                "Screen Recording not granted — only your microphone will be captured. Grant in Settings → Permissions and restart for the full meeting transcript.");
        }
    }

    // Shared-note recording lock. For a workspace note, claim it before we
    // start capturing so two teammates can't record the same note at once —
    // their transcripts would otherwise clobber each other under last-write-wins
    // sync. Server-arbitrated via a unique index; Personal notes and an
    // unreachable cloud return `Skipped` and record unlocked.
    let claimed_lock_id = match cloud::claim_recording_lock(&app, &note_id).await {
        cloud::LockClaim::Held(who) => {
            let msg = format!(
                "{who} is recording this note — only one person can record a shared note at a time."
            );
            emit_error(&app, Some(&note_id), &msg);
            return Err(msg);
        }
        cloud::LockClaim::Granted(id) => Some(id),
        cloud::LockClaim::Skipped => None,
    };

    emit_status(&app, Some(&note_id), Phase::Starting);

    let temp_dir = std::env::temp_dir().join(format!("notes-app-{}", note_id));
    std::fs::create_dir_all(&temp_dir).map_err(err)?;

    let sidecar_path = sidecar_path(&app)?;
    let mut cmd = Command::new(&sidecar_path);
    cmd.arg("--out").arg(&temp_dir);
    if keep_awake_enabled(&app) {
        cmd.arg("--keep-awake");
    }
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.kill_on_drop(true);
    // Detach the child into a new session so macOS TCC doesn't tie its
    // microphone / screen-recording authorization to the parent dev binary.
    // Without this, the sidecar inherits the parent's TCC "responsible process"
    // and is silently denied even though its own binary is granted.
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                // Non-fatal: continue without detaching.
            }
            Ok(())
        });
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            // Don't leak the lock we just claimed if the sidecar won't start.
            if let Some(id) = &claimed_lock_id {
                cloud::release_recording_lock(&app, id.clone()).await;
            }
            // Back out of Phase::Starting. Every phase-driven surface latches
            // the last one it saw — the recording bar, and (since #21) the tray
            // icon and its Start/Stop items — so a start that gives up here
            // without an Idle leaves all of them claiming a live capture, with
            // no affordance to clear it short of a restart.
            emit_status(&app, None, Phase::Idle);
            return Err(format!("spawn audio-capture: {e}"));
        }
    };
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let stderr = child.stderr.take().ok_or("no stderr")?;

    // Drain stderr in the background so the pipe never fills. Only the
    // sidecar's own `humla-error:`-prefixed lines get surfaced to the
    // user as a recording_error toast — anything else is treated as
    // dev-time diagnostic noise and only mirrored to our own stderr.
    // Without this filter, the Swift side's verbose `scstream: …`
    // debug lines would each pop up as a "Recording issue" toast,
    // which is what the user reported when the SCK debug logging
    // landed.
    {
        let app_err = app.clone();
        let note_id_err = note_id.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                let trimmed = line.trim();
                if trimmed.is_empty() { continue; }
                eprintln!("audio-capture stderr: {trimmed}");
                if let Some(msg) = trimmed.strip_prefix("humla-error: ") {
                    let _ = app_err.emit("recording_error", ErrorPayload {
                        note_id: Some(note_id_err.clone()),
                        message: format!("audio-capture: {msg}"),
                    });
                }
            }
        });
    }

    // Fresh inflight list for this session so handles from a previous
    // recording can never mix in.
    let inflight: Inflight = Arc::new(parking_lot::Mutex::new(Vec::new()));
    {
        let mut s = state.recording.lock();
        s.note_id = Some(note_id.clone());
        s.child = Some(child);
        s.temp_dir = Some(temp_dir);
        s.inflight = inflight.clone();
        // Allocate this capture's persisted session identity now, so the
        // post-stop chain writes into recordings/<note_id>/<session_id>/ and
        // stamps the manifest with a real start time.
        s.session_id = Some(uuid::Uuid::new_v4().to_string());
        s.session_started_at = Some(chrono::Utc::now().to_rfc3339());
        // Wipe any context from a previous recording — proper nouns and
        // sentence fragments from a different conversation would only confuse
        // this session's decoder. Same for the speaker bookkeeping. Per-source
        // trails because the mic and system streams are separate
        // conversations — sharing a trail would pull each Whisper invocation
        // toward the other side's vocabulary and language.
        // A fresh sink per capture: proper nouns and sentence fragments from a
        // different conversation would only confuse this session's decoder,
        // and a straggler chunk from the previous capture must not land in
        // this one's log. `mode` is decided once, here — flipping the setting
        // mid-meeting changes nothing about a recording already running.
        s.sink = Arc::new(CaptureSink::new(sink_mode));
    }

    // Snapshot any existing transcript so diarize_and_apply can prepend it
    // to this session's output. Resuming a recording on a note that already
    // has transcript content adds to it; starting on a blank note produces
    // the snapshot "" and behaves like a fresh recording.
    {
        let state: State<AppState> = app.state();
        let existing = {
            let conn = state.db.lock();
            db::get_note(&conn, &note_id)
                .map(|n| n.transcript)
                .unwrap_or_default()
        };
        *state.recording.lock().transcript_at_start.lock() = existing;
    }

    let app_clone = app.clone();
    let note_id_clone = note_id.clone();
    let inflight_for_reader = inflight.clone();
    let sink_for_reader = state.recording.lock().sink.clone();
    let reader_handle = tokio::spawn(async move {
        let mut reader = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            let trimmed = line.trim();
            if trimmed.is_empty() { continue; }
            match serde_json::from_str::<SidecarEvent>(trimmed) {
                // Live capture passes `None` for the backlog — chunks arrive at
                // realtime so there's nothing to bound (see `import_audio` for
                // the semaphore-bounded variant).
                Ok(ev) => {
                    if dispatch_sidecar_event(&app_clone, &note_id_clone, ev, &inflight_for_reader, None, &sink_for_reader).await {
                        break;
                    }
                }
                Err(e) => eprintln!("bad sidecar line: {e} -- {}", redact_sidecar_line(&line)),
            }
        }
        // Reader exited (sidecar closed its pipe). If the session is still
        // marked as recording for THIS note, that means the sidecar died
        // without us asking — i.e. a crash. Clean up and notify the UI so
        // the user isn't pinned in a stale "recording" state. A stop in
        // progress is the other way this pipe closes, and `note_id` stays set
        // through its drain (#182), so the stop state is what tells the two
        // apart.
        let state: tauri::State<AppState> = app_clone.state();
        let (was_active, lock_id, lock_heartbeat) = {
            let mut s = state.recording.lock();
            if s.note_id.as_deref() == Some(&note_id_clone)
                && s.stop == crate::recording::StopState::None
            {
                s.note_id = None;
                s.child = None;
                s.temp_dir = None;
                s.reader = None;
                s.inflight = Arc::new(parking_lot::Mutex::new(Vec::new()));
                (true, s.lock_id.take(), s.lock_heartbeat.take())
            } else {
                (false, None, None)
            }
        };
        // Sidecar died without recording_stop — drop the lock so the note isn't
        // pinned until the TTL lapses (the heartbeat would otherwise keep a dead
        // recording's lock alive).
        if let Some(hb) = lock_heartbeat {
            hb.abort();
        }
        if let Some(id) = lock_id {
            let app_rel = app_clone.clone();
            tokio::spawn(async move {
                cloud::release_recording_lock(&app_rel, id).await;
            });
        }
        if was_active {
            // Through `emit_status`, not a bare emit: the sidecar dying is still
            // a phase change, and the tray has to stop claiming a live capture.
            emit_status(&app_clone, None, Phase::Idle);
            let _ = app_clone.emit("recording_error", ErrorPayload {
                note_id: Some(note_id_clone.clone()),
                message: "Recording stopped unexpectedly. Try again.".to_string(),
            });
        }
    });

    // Committed only with the reader in place: until then a start would read
    // this capture's live child with no reader as an orphan and stop it.
    {
        let mut s = state.recording.lock();
        s.reader = Some(reader_handle);
        claim.commit(&mut s);
    }

    // Attach the shared-note lock to the live session + start its heartbeat so a
    // long meeting keeps the lock fresh. Stored on the session so recording_stop
    // and the crash-recovery path can abort the heartbeat and release the lock.
    if let Some(id) = claimed_lock_id {
        let hb = cloud::spawn_lock_heartbeat(app.clone(), id.clone());
        let mut s = state.recording.lock();
        s.lock_id = Some(id);
        s.lock_heartbeat = Some(hb);
    }

    emit_status(&app, Some(&note_id), Phase::Recording);
    Ok(())
}

#[tauri::command]
pub async fn recording_stop(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let stop_pressed = std::time::Instant::now();
    let (child, note_id, temp_dir, inflight, reader, lock_id, lock_heartbeat, deferred) = {
        let mut s = state.recording.lock();
        // `note_id` deliberately stays set until the post-stop snapshot: it is
        // what lets a chunk finishing during the drain append to the note live
        // (#182). `StopState::Draining` is what makes a second capture refuse
        // in the meantime, and what tells the reader's EOF handler that this
        // shutdown was asked for rather than a sidecar crash.
        let note_id = s.begin_stop().map_err(str::to_string)?;
        let child = s.child.take();
        let temp_dir = s.temp_dir.take();
        // The reader holds a clone of this same Arc, so chunks emitted during
        // shutdown still land in the list we drain below. Swap in a fresh
        // list to keep `s` self-consistent for the next session.
        let inflight = std::mem::replace(&mut s.inflight, Arc::new(parking_lot::Mutex::new(Vec::new())));
        let reader = s.reader.take();
        let lock_id = s.lock_id.take();
        let lock_heartbeat = s.lock_heartbeat.take();
        // Read here rather than off the post-stop snapshot: the phase this
        // stop emits next has to carry it, and the snapshot is taken after the
        // drain.
        let deferred = !s.sink.transcribes_on_arrival();
        (child, note_id, temp_dir, inflight, reader, lock_id, lock_heartbeat, deferred)
    };

    // Stop heartbeating and release the shared-note lock immediately, so a
    // teammate can record next without waiting out the TTL. Best-effort: a
    // missed release just lets the lock expire on its own.
    if let Some(hb) = lock_heartbeat {
        hb.abort();
    }
    if let Some(id) = lock_id {
        let app_release = app.clone();
        tokio::spawn(async move {
            cloud::release_recording_lock(&app_release, id).await;
        });
    }

    // Everything in flight at stop-pressed is the drain's denominator; the
    // reader may still push a last chunk or two before the sidecar closes its
    // pipe, so the count is re-taken (and re-emitted) after the reader exits.
    if deferred {
        emit_deferred_phase(&app, &note_id, Phase::Stopping);
    } else {
        let pending_at_stop = inflight.lock().len() as u32;
        emit_stopping_progress(&app, &note_id, pending_at_stop, 0);
    }

    let clock = StopClock::new(stop_pressed);
    let t_sidecar = std::time::Instant::now();
    if let Some(mut child) = child {
        // Send SIGTERM so the Swift sidecar runs its shutdown handler:
        // closes the writers (emitting any final chunk + full_recording
        // events), emits `stopped`, then exits. Wait up to 8 s for a
        // graceful exit before falling back to SIGKILL. 8 s is generous
        // for a normal stop (writer.close is synchronous and fast), but
        // ScreenCaptureKit's `stopCapture()` has been observed to stall
        // for multiple seconds; the sidecar now closes writers BEFORE
        // awaiting stopCapture, but the longer grace gives an extra
        // safety margin so SIGKILL never truncates emitted-but-unread
        // chunk events.
        if let Some(pid) = child.id() {
            #[cfg(unix)]
            unsafe { libc::kill(pid as i32, libc::SIGTERM); }
        }
        let waited = tokio::time::timeout(std::time::Duration::from_secs(8), child.wait()).await;
        if waited.is_err() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
    }

    clock.record(|t| t.sidecar_shutdown_ms = ms_since(t_sidecar));

    // Wait for the stdout reader to finish first: it exits when the sidecar
    // closes the pipe, which is guaranteed now that `child.wait()` returned.
    // After this point no more transcribe handles can be pushed to inflight.
    let t_reader = std::time::Instant::now();
    if let Some(r) = reader {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), r).await;
    }
    clock.record(|t| t.reader_wait_ms = ms_since(t_reader));

    // Drain in-flight transcribe tasks BEFORE snapshotting chunk_log.
    //
    // Each transcribe pushes its ChunkRecord to `chunk_log` only after the
    // provider call (Whisper / OpenAI) returns. If we snapshot before
    // draining, an in-flight transcribe that completes during the post-stop
    // chain pushes its chunk to the live `state.recording.chunk_log` —
    // which is too late: the snapshot already handed to `diarize_and_apply`
    // is frozen. The user-visible failure is "stopped before the first
    // chunk's transcript appeared → audio for that chunk is lost", because
    // `diarize_and_apply` sees an empty chunks list and bails with "no
    // chunks captured".
    //
    // The 300 s ceiling is generous so Whisper inference that's slowed down
    // (Metal accumulating, queued behind the gate) still gets to finish.
    // Aborting = silently dropping audio, which is the bug we're avoiding.
    // The user sees Phase::Stopping during the drain, which can be a few
    // seconds in the typical case — with the count of what's left, and each
    // chunk's text landing in the note as it completes (#182).
    let pending = inflight.lock().len() as u32;
    let t_drain = std::time::Instant::now();
    if deferred {
        // Nothing was dispatched, so there is nothing to drain and no count
        // worth reporting.
        drain_inflight(&inflight).await;
    } else {
        emit_stopping_progress(&app, &note_id, pending, 0);
        drain_inflight_reporting(&inflight, |done| {
            emit_stopping_progress(&app, &note_id, pending, done);
        })
        .await;
    }
    clock.record(|t| {
        t.drain_ms = ms_since(t_drain);
        t.drain_pending_chunks = pending;
    });

    // Snapshot every piece of session-derived state the post-stop chain
    // needs, NOW — *after* the drain so chunk_log includes every chunk
    // whose transcribe finished, and before spawning the background task
    // so it can't race a new `recording_start` (which clears
    // `state.recording.chunk_log`).
    let post_stop = {
        let mut s = state.recording.lock();
        take_post_stop_snapshot(&mut s)
    };

    // Spawn the post-stop processing chain in the background:
    //   Stopping → Diarizing → Idle
    // (`temp_dir` is dropped at the end of the chain, after diarize is done
    // with the full WAVs — see `run_post_stop_chain`.)
    let app_for_post = app.clone();
    let note_for_post = note_id.clone();
    tokio::spawn(async move {
        run_post_stop_chain(
            app_for_post,
            note_for_post,
            temp_dir,
            post_stop,
            clock,
        )
        .await;
    });

    Ok(())
}

/// Await every in-flight transcribe handle, aborting stragglers past the 300 s
/// ceiling. Shared by `recording_stop` and the import completion path.
///
/// The generous ceiling lets Whisper inference that's been slowed down (Metal
/// accumulating, queued behind `transcribe_gate`) still finish — aborting =
/// silently dropping that chunk's audio, which is the bug this exists to avoid.
async fn drain_inflight(inflight: &Inflight) {
    drain_inflight_reporting(inflight, |_| {}).await
}

/// [`drain_inflight`] with a progress callback, called with the running count
/// each time a handle lands — including one that failed or was skipped. The
/// count always reaches the pending total, so the bar always fills: on the
/// timeout the abandoned handles are counted in one final call, since the
/// transcript is what it is at that point.
async fn drain_inflight_reporting(inflight: &Inflight, mut on_done: impl FnMut(u32)) {
    let pending = inflight.lock().len() as u32;
    let drain = async {
        let mut done: u32 = 0;
        loop {
            let next = inflight.lock().pop();
            match next {
                Some(h) => {
                    let _ = h.await;
                    done += 1;
                    on_done(done);
                }
                None => break,
            }
        }
    };
    let timed_out =
        tokio::time::timeout(std::time::Duration::from_secs(300), drain).await.is_err();
    if timed_out {
        let remaining: Vec<_> = inflight.lock().drain(..).collect();
        eprintln!(
            "drain_inflight: timed out, aborting {} lingering transcribe(s)",
            remaining.len()
        );
        for h in &remaining {
            h.abort();
        }
        for h in remaining {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), h).await;
        }
        on_done(pending);
    }
}

/// Freeze the live capture's session-derived state into a [`PostStopSnapshot`].
/// Must be called *after* the inflight drain so `chunk_log` includes every
/// finished chunk. `session_id` / `session_started_at` fall back to fresh
/// values if (somehow) unset — a missing id only costs this take its own
/// subdir, never a clobber. Shared by `recording_stop` and `finish_import`.
fn take_post_stop_snapshot(s: &mut crate::recording::LiveCapture) -> PostStopSnapshot {
    // The snapshot is where ownership of `note.transcript` changes hands: from
    // here the post-stop chain rewrites the whole string, so `note_id` goes
    // (which stops the live per-chunk append) and the slot moves to
    // `Finishing` (which keeps a new capture refused until the chain lands on
    // Idle).
    s.note_id = None;
    s.stop = crate::recording::StopState::Finishing;
    let mic_wav = s.sink.mic_full_wav_path.lock().clone();
    let sys_wav = s.sink.sys_full_wav_path.lock().clone();
    let chunks = s.sink.chunk_log.lock().clone();
    let deferred = !s.sink.transcribes_on_arrival();
    let captured_duration_ms = *s.sink.captured_duration_ms.lock();
    let capture_timing = s.sink.capture_timing.lock().clone();
    let transcript_at_start = s.transcript_at_start.lock().clone();
    let session_id = s
        .session_id
        .take()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let session_started_at = s
        .session_started_at
        .take()
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
    PostStopSnapshot {
        mic_wav,
        sys_wav,
        chunks,
        transcript_at_start,
        session_id,
        session_started_at,
        deferred,
        captured_duration_ms,
        capture_timing,
    }
}

/// The shared post-stop chain: Diarizing → (session bookkeeping, keep-audio,
/// diarize + label, manifest append, sync ping, temp-dir cleanup) → Idle. Runs
/// off a snapshot so it can't race a new capture claiming the slot. Driven by
/// both `recording_stop` (live) and `finish_import` (file import — the mic-only
/// diarize branch).
async fn run_post_stop_chain(
    app: AppHandle,
    note_id: String,
    temp_dir: Option<PathBuf>,
    post_stop: PostStopSnapshot,
    clock: StopClock,
) {
    // The chain ends on Idle from one place, and that place is a `Drop`: a
    // panic anywhere inside would otherwise leave the slot wedged in
    // `Finishing` (refusing recording for the rest of the session) with the
    // indicator and the tray pinned on "Identifying speakers…".
    {
        let _slot = SlotGuard { app: app.clone() };
        post_stop_chain_inner(app.clone(), note_id.clone(), temp_dir, post_stop, &clock).await;
    }
    let timings = clock.finish();
    eprintln!("{}", timings.summary());
    write_stop_timings(&app, &note_id, &timings).await;
    notes::purge_note_assets_if_gone(&app, &note_id);
}

/// Wall clock for one stop chain: when stop was pressed, plus the durations
/// collected since. Shared with `diarize_and_apply`, which fills the
/// per-source diarize rows from inside its own branches.
struct StopClock {
    started: std::time::Instant,
    timings: Arc<parking_lot::Mutex<crate::recording::StopTimings>>,
}

impl StopClock {
    fn new(started: std::time::Instant) -> Self {
        Self { started, timings: Arc::new(parking_lot::Mutex::default()) }
    }

    fn record(&self, f: impl FnOnce(&mut crate::recording::StopTimings)) {
        f(&mut self.timings.lock());
    }

    fn finish(self) -> crate::recording::StopTimings {
        let total = self.started.elapsed().as_millis() as u64;
        let mut t = self.timings.lock().clone();
        t.total_ms = total;
        t
    }
}

/// [`StopClock::record`] for the steps a caller that isn't measuring a stop
/// also drives — the diarize pass runs for re-diarize and for deferred
/// transcription too.
fn record_timing(clock: Option<&StopClock>, f: impl FnOnce(&mut crate::recording::StopTimings)) {
    if let Some(clock) = clock {
        clock.record(f);
    }
}

fn ms_since(t: std::time::Instant) -> u64 {
    t.elapsed().as_millis() as u64
}

/// Frees the capture slot and lands the phase on Idle when the post-stop chain
/// ends, however it ends. The slot is released before the event so a listener
/// that answers Idle by recording again isn't refused.
struct SlotGuard {
    app: AppHandle,
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        if let Some(state) = self.app.try_state::<AppState>() {
            state.recording.lock().stop = crate::recording::StopState::None;
        }
        emit_status(&self.app, None, Phase::Idle);
    }
}

/// Merge the chain's `timings` into the take's diarize diagnostics JSON,
/// keeping every field already in it. When no diarize diagnostics were written
/// — no model, no chunks, a deferred take — the timings land in a
/// `timings.json` beside where that file would have gone, so a stop is never
/// unmeasured. Best-effort: a write failure logs and nothing else.
async fn write_stop_timings(app: &AppHandle, note_id: &str, timings: &crate::recording::StopTimings) {
    let path = match timings.diagnostics_path.clone() {
        Some(p) => p,
        None => {
            let Ok(app_dir) = app.path().app_data_dir() else { return };
            let dir = app_dir.join("diagnostics").join(note_id);
            if let Err(e) = tokio::fs::create_dir_all(&dir).await {
                eprintln!("timings: mkdir {}: {e}", dir.display());
                return;
            }
            dir.join("timings.json")
        }
    };
    let existing = tokio::fs::read_to_string(&path)
        .await
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
    let merged = crate::recording::merge_timings(existing, timings);
    match serde_json::to_string_pretty(&merged) {
        Ok(json) => {
            if let Err(e) = tokio::fs::write(&path, json).await {
                eprintln!("timings: write {}: {e}", path.display());
            }
        }
        Err(e) => eprintln!("timings: serialize: {e}"),
    }
}

/// A capture's stream timing, at `diagnostics/<note_id>/capture-<session_id>.json`.
///
/// One file per take, because every take has its own offset — the diarize
/// dumps are keyed `<engine>-<source>` and a second take would overwrite the
/// first. Holds no `chunks`, so `read_chunks_from_diagnostic` passes over it.
/// Timestamps and HAL latency figures only: no audio, no device names (#174),
/// which is why it is written whatever `keep_audio` says. Best-effort.
async fn write_capture_timing(
    app: &AppHandle,
    note_id: &str,
    session_id: &str,
    timing: &crate::recording::CaptureTiming,
) {
    let Ok(app_dir) = app.path().app_data_dir() else { return };
    let dir = app_dir.join("diagnostics").join(note_id);
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        eprintln!("capture timing: mkdir {}: {e}", dir.display());
        return;
    }
    let path = dir.join(format!("capture-{session_id}.json"));
    let payload = serde_json::json!({
        "session_id": session_id,
        "created_at": chrono::Utc::now().timestamp_millis(),
        "alignment": timing.alignment(),
        "capture_timing": timing,
    });
    match serde_json::to_string_pretty(&payload) {
        Ok(json) => {
            if let Err(e) = tokio::fs::write(&path, json).await {
                eprintln!("capture timing: write {}: {e}", path.display());
            }
        }
        Err(e) => eprintln!("capture timing: serialize: {e}"),
    }
}

/// The chain's body. Split out so the slot release, the Idle and the timings
/// write in [`run_post_stop_chain`] cover every one of its early returns.
async fn post_stop_chain_inner(
    app: AppHandle,
    note_id: String,
    temp_dir: Option<PathBuf>,
    post_stop: PostStopSnapshot,
    clock: &StopClock,
) {
    if post_stop.deferred {
        emit_deferred_phase(&app, &note_id, Phase::Diarizing);
    } else {
        emit_status(&app, Some(&note_id), Phase::Diarizing);
    }
    // A deferred stop names nothing at all: it dispatched no chunks, has no
    // text to diarize and lands on idle in a few hundred milliseconds, so a
    // labelled bar drawn for it appears and vanishes.
    let progress = if post_stop.deferred {
        ChainProgress::Silent
    } else {
        ChainProgress::PostStop { note_id: note_id.clone() }
    };

    // First, and whatever kind of take this is: it depends on nothing below,
    // and a deferred take returns early.
    if let Some(timing) = post_stop.capture_timing.as_ref() {
        write_capture_timing(&app, &note_id, &post_stop.session_id, timing).await;
    }

    // Make the note's storage session-shaped before writing this take. For a
    // pre-feature flat note, migrate its single take into a session subdir so
    // both takes survive.
    let _ = prepare_sessions_for_new_take(&app, &note_id).await;

    // Copy full WAVs into this session's subdir FIRST when keep_audio is on.
    // diarize_and_apply cleans up the temp paths after it's done, so retention
    // has to happen before it runs. (#16's "a 2nd+ session force-retains its
    // sources" exception is gone — #24 made the setting absolute, so a
    // multi-take note recorded with keep_audio off simply can't be unified.)
    let t_keep = std::time::Instant::now();
    maybe_keep_audio(&app, &note_id, &post_stop, &progress).await;
    clock.record(|t| t.keep_audio_ms = ms_since(t_keep));

    let session_id = post_stop.session_id.clone();
    let session_started_at = post_stop.session_started_at.clone();
    let deferred = post_stop.deferred;
    let post_stop_captured_duration = post_stop.captured_duration_ms;
    // A deferred take produced no chunks by design, so `session_streams` has
    // nothing to read. Take the streams from what the sidecar actually wrote
    // instead — the carousel tooltip and the manifest want the same answer
    // whether or not anything was transcribed.
    let streams = if deferred {
        retained_streams(&post_stop)
    } else {
        session_streams(&post_stop.chunks)
    };
    let transcribed_nothing = post_stop.chunks.is_empty();

    if deferred {
        // Nothing to diarize, label, title or detect a language from — there
        // is no text yet. What this take needs is to be *playable* and to be
        // recorded as pending, so the note's Transcribe action can find it.
        // `write_playback_assets` writes the mixed WAV (keep_audio is on by
        // construction here — see `sessions::defer_transcription`) and, with an
        // empty timeline, deliberately writes no `timeline.jsonl`: a timeline
        // is a claim about text, and this take has none to account for yet.
        let t_playback = std::time::Instant::now();
        write_playback_assets(
            &app,
            &note_id,
            &session_id,
            String::new(),
            post_stop.mic_wav.as_deref(),
            post_stop.sys_wav.as_deref(),
        )
        .await;
        clock.record(|t| t.playback_assets_ms = ms_since(t_playback));
        if discard_empty_take(&app, &note_id, &session_id).await {
            // No audio either — an aborted press. Nothing worth a manifest row.
            clear_temp_dir(temp_dir, clock).await;
            return;
        }
        let t_finalize = std::time::Instant::now();
        finalize_session(
            &app,
            &note_id,
            &session_id,
            &session_started_at,
            streams,
            post_stop.captured_duration_ms,
            sessions::Transcribed::No,
        )
        .await;
        clock.record(|t| t.finalize_session_ms = ms_since(t_finalize));
        session_changed_for_sync(&app, &note_id, &session_id);
        clear_temp_dir(temp_dir, clock).await;
        // The Idle the chain lands on is also what makes this pending take
        // visible: the note view re-reads its sessions on every phase
        // transition, so the Transcribe action appears without a reload.
        return;
    }

    // Record what the provider heard before diarizing (issue #167).
    // Deliberately *not* inside diarize_and_apply: that returns early when
    // the diarize model isn't downloaded, so a user without it would never
    // get a detected language. File import inherits this for free — it
    // drives the same chain.
    let t_lang = std::time::Instant::now();
    record_detected_language(&app, &note_id, &post_stop.chunks);
    clock.record(|t| t.detect_language_ms = ms_since(t_lang));
    // Title the note NOW, concurrently with the diarize pass (#90).
    //
    // The transcript is already final and in the database: `recording_stop`
    // drains every in-flight transcribe before it snapshots, and each one wrote
    // through `db::append_transcript` on its way. What diarization adds is
    // `Speaker N:` prefixes — which a title has no use for. Waiting for it cost
    // the whole diarize pass, minutes on a long meeting, for nothing.
    //
    // After `record_detected_language`, though: that resolves `auto` to what was
    // actually spoken, and the title's language directive reads it.
    tauri::async_runtime::spawn(title::generate_note_title(app.clone(), note_id.clone()));
    if let Err(e) = diarize_and_apply(
        app.clone(),
        note_id.clone(),
        post_stop,
        Some(clock),
        &progress,
    )
    .await
    {
        eprintln!("diarize_and_apply: {e}");
        emit_error(
            &app,
            Some(&note_id),
            &format!("Diarization failed (transcript still saved): {e}"),
        );
    }

    // An aborted press — Record, then Stop a second or two later — used to
    // become a permanent take: a manifest entry, a numbered pill in the
    // carousel, and a session dir. It contributed no transcript, so the real
    // recording after it showed up as "Recording 2" and the note looked split.
    // Discard it instead.
    //
    // Gated on retained audio, not on the chunk count alone: from here, "every
    // chunk failed to transcribe" (provider outage, revoked key) is
    // indistinguishable from silence, and deleting a recording the user could
    // still play or re-diarize has no undo. Nothing transcribed AND nothing to
    // listen to is the only safe discard.
    if transcribed_nothing && discard_empty_take(&app, &note_id, &session_id).await {
        // No manifest row, no unify pass over a take that isn't there, and no
        // sync pings for a session the server should never learn about.
        clear_temp_dir(temp_dir, clock).await;
        return;
    }

    // Append this take to the manifest so it shows up in the carousel and in
    // session-aware path resolution. Duration comes from the timeline
    // diarize_and_apply just wrote (max end_ms); 0 when nothing landed.
    let t_finalize = std::time::Instant::now();
    finalize_session(
        &app,
        &note_id,
        &session_id,
        &session_started_at,
        streams,
        post_stop_captured_duration,
        sessions::Transcribed::Yes,
    )
    .await;
    clock.record(|t| t.finalize_session_ms = ms_since(t_finalize));
    // Cross-session speaker unification (#17): once the note has two or more
    // takes with retained source audio + chunk timings, re-cluster the
    // concatenated audio so one voice carries one label across takes —
    // replacing the offset-only numbers diarize_and_apply just wrote. Must
    // run after finalize_session (the manifest has to include this take) and
    // before the temp-dir cleanup is irrelevant (it reads the retained
    // session copies, not the temp WAVs). No-op for single-session notes;
    // failures keep the per-take labels.
    let t_unify = std::time::Instant::now();
    match unify_note_speakers(&app, &note_id, &progress).await {
        Ok(Some(costs)) => {
            eprintln!("unify: cross-session speaker unification applied");
            clock.record(|t| t.unify_echo_ms = costs.echo_ms);
        }
        Ok(None) => {}
        Err(e) => eprintln!("unify: failed, keeping per-take labels: {e}"),
    }
    clock.record(|t| t.unify_ms = ms_since(t_unify));
    // The pipeline wrote the transcript directly (per-chunk during capture,
    // then the labelled rewrite), bypassing the notes_* commands — so push it
    // to the cloud now. Covers every diarize_and_apply exit path.
    note_changed_for_sync(&app, &note_id);
    // Ping the session AFTER the note (lower seq → the note record drains
    // first, so the session's parent-note lookup resolves). Enqueues the
    // note_sessions metadata push; the assets upload separately (the frontend
    // fires cloud_upload_note_sessions once this chain lands on Idle).
    session_changed_for_sync(&app, &note_id, &session_id);
    // Now that every step that needs the WAVs has finished, drop the temp dir.
    // Best-effort: a leftover dir is harmless.
    clear_temp_dir(temp_dir, clock).await;
}

/// Drop the capture's temp dir, timed. Best-effort — a leftover dir is
/// harmless, and a stop that skipped the dir records a zero.
async fn clear_temp_dir(dir: Option<PathBuf>, clock: &StopClock) {
    let Some(dir) = dir else { return };
    let t = std::time::Instant::now();
    let _ = tokio::fs::remove_dir_all(dir).await;
    clock.record(|x| x.temp_cleanup_ms = ms_since(t));
}

/// Resolve the active transcription provider for `note_id`'s language and
/// verify its prerequisites are in place — a stored API key for a cloud
/// provider, or a downloaded model file for local Whisper. On a miss it emits
/// a `recording_error` toast for the note and returns `Err(msg)`. On success it
/// also fires the local-Whisper prewarm (fire-and-forget) so the first chunk
/// doesn't pay the cold-start tax. Shared by `recording_start` and
/// `import_audio`.
async fn ensure_provider_ready(
    app: &AppHandle,
    state: &State<'_, AppState>,
    note_id: &str,
) -> Result<crate::stt::ProviderConfig, String> {
    // Resolve the per-language override (if any) up front: both the prereq
    // check and the prewarm use the resolved provider — otherwise a user with a
    // Norwegian override (Local) and an English default (Deepgram) would prewarm
    // the wrong model.
    let transcribe_cfg = read_transcribe_config(state).map_err(|e| e.to_string())?;
    let language = {
        let conn = state.db.lock();
        let global = db::get_setting(&conn, "language")
            .map_err(err)?
            .unwrap_or_else(|| DEFAULT_LANGUAGE.to_string());
        let note_lang = db::get_note(&conn, note_id)
            .map(|n| n.language)
            .unwrap_or_default();
        if note_lang.trim().is_empty() { global } else { note_lang }
    };
    let provider_cfg = transcribe_cfg.resolve(&language).clone();
    // Each cloud provider has its own Keychain slot. Look up the right one so
    // the check doesn't pass when (e.g.) the user picked Deepgram but only saved
    // an OpenAI key.
    let pre_err: Option<String> = match &provider_cfg {
        crate::stt::ProviderConfig::Local(local_cfg) => {
            let p = local_model_path(app, &language, &local_cfg.model_id)
                .map_err(|e| e.to_string())?;
            if p.exists() {
                None
            } else {
                Some(
                    "Local Whisper model not downloaded. Download it in Settings → Transcription."
                        .to_string(),
                )
            }
        }
        crate::stt::ProviderConfig::Custom(custom) => {
            let conn = state.db.lock();
            if crate::custom_endpoint::base_url(&conn).is_err() {
                Some("The custom endpoint has no base URL. Add one in Settings → Transcription.".to_string())
            } else if custom.model.trim().is_empty() {
                Some(
                    "No transcription model is set for the custom endpoint. Type one in Settings → Transcription."
                        .to_string(),
                )
            } else {
                None
            }
        }
        other => {
            let provider_id = other.provider_id();
            if read_provider_api_key(state, provider_id)?.is_none() {
                let label = other.provider().label();
                Some(format!("{label} API key not set. Add one in Settings → API keys."))
            } else {
                None
            }
        }
    };
    if let Some(msg) = pre_err {
        emit_error(app, Some(note_id), &msg);
        return Err(msg);
    }

    // Race a Whisper model load against the sidecar startup so the first chunk
    // doesn't pay the cold-start tax (~1–2 s on Apple Silicon). Fire and forget.
    if let crate::stt::ProviderConfig::Local(local_cfg) = &provider_cfg {
        let model_id = local_cfg.model_id.clone();
        let use_gpu = local_cfg.use_gpu;
        if let Ok(model_path) = local_model_path(app, &language, &model_id) {
            let shared = state.whisper.clone();
            tokio::spawn(async move {
                if let Err(e) = local_whisper::prewarm(shared, model_path, use_gpu).await {
                    eprintln!("whisper prewarm: {e}");
                }
            });
        }
    }
    Ok(provider_cfg)
}

/// Handle one decoded sidecar event during a live capture, an import, or a
/// deferred replay (#146).
///
/// Chunks are transcribed on spawned tasks tracked in `inflight`, against
/// `sink` — which owns the rolling context they decode with, the log they land
/// in, and whether the note's transcript moves as they arrive. When `backlog`
/// is `Some`, a permit is acquired *before* spawning (and released when the
/// task finishes), so a full-speed replay can't pile up hundreds of parked
/// pre-gate tasks. Live recording passes `None` — realtime arrival needs no
/// bound. Returns `true` when the sidecar signalled `Stopped` and the reader
/// loop should break.
async fn dispatch_sidecar_event(
    app: &AppHandle,
    note_id: &str,
    event: SidecarEvent,
    inflight: &Inflight,
    backlog: Option<&Arc<Semaphore>>,
    sink: &Arc<CaptureSink>,
) -> bool {
    match event {
        SidecarEvent::Chunk { source, path, start_ms } => {
            let pb = PathBuf::from(path);
            // "Transcribe manually" (#146): the chunk WAV is written, and
            // nothing reads it. Delete it as it arrives rather than letting an
            // hour of them accumulate beside the full streams they duplicate —
            // the temp dir is swept at the end of the chain either way, but the
            // peak is what fills a disk mid-meeting.
            if !sink.transcribes_on_arrival() {
                let _ = tokio::fs::remove_file(&pb).await;
                return false;
            }
            let source = sink.resolve_source(source);
            let app2 = app.clone();
            let note_id2 = note_id.to_string();
            let sink2 = sink.clone();
            // Block the reader here until a permit frees when importing — this
            // is the backpressure. `acquire_owned` yields a permit we move into
            // the spawned task so it's released the instant the chunk finishes.
            let permit = match backlog {
                Some(sem) => sem.clone().acquire_owned().await.ok(),
                None => None,
            };
            // A replay reports on the chunk it has just *finished*, not the one
            // the sidecar has just handed over: the reader runs at disk speed
            // and is allowed IMPORT_BACKLOG_PERMITS ahead, which on a short
            // take is the whole of it.
            let progress = sink.replay_progress.clone();
            let h = tokio::spawn(async move {
                let _permit = permit; // held until this transcribe returns
                let outcome =
                    transcribe_chunk(app2.clone(), note_id2.clone(), source, pb, start_ms, sink2)
                        .await;
                if let Some(progress) = progress {
                    let snapshot = {
                        let mut p = progress.lock();
                        p.note_chunk(start_ms);
                        p.snapshot()
                    };
                    emit_transcribe_progress(&app2, &note_id2, snapshot);
                }
                if let Err(e) = outcome {
                    let msg = format!("Transcription failed: {e}");
                    eprintln!("{msg}");
                    let _ = app2.emit("recording_error", ErrorPayload {
                        note_id: Some(note_id2),
                        message: msg,
                    });
                }
            });
            inflight.lock().push(h);
            false
        }
        SidecarEvent::FullRecording { source, path, duration_ms } => {
            let source = sink.resolve_source(source);
            // Per stream, because the sink keeps only the longer of the two —
            // and the difference is the first thing an alignment question asks.
            eprintln!("sidecar: full_recording {} duration_ms={duration_ms}", source.as_str());
            sink.note_stream_duration(duration_ms);
            // Stash the path on this capture's sink; the diarization pass reads
            // it. Each source has its own slot so the post-stop pass can branch
            // (mic-only → diarize mic; both present → diarize both).
            *sink.full_wav_slot(source).lock() = Some(PathBuf::from(path));
            false
        }
        SidecarEvent::Error { message } => {
            eprintln!("sidecar error: {message}");
            let _ = app.emit("recording_error", ErrorPayload {
                note_id: Some(note_id.to_string()),
                message,
            });
            false
        }
        SidecarEvent::Stopped => true,
        SidecarEvent::Paused => {
            emit_status(app, Some(note_id), Phase::Paused);
            false
        }
        SidecarEvent::Resumed => {
            emit_status(app, Some(note_id), Phase::Recording);
            false
        }
        SidecarEvent::Heartbeat { mic_frames, sys_frames, chunks, mic_peak, sys_peak, input_device } => {
            let _ = app.emit("recording_diagnostic", DiagnosticPayload {
                note_id: note_id.to_string(),
                mic_frames,
                sys_frames,
                chunks,
                mic_peak,
                sys_peak,
                input_device,
            });
            false
        }
        SidecarEvent::Diagnostic { message, input_device } => {
            // Non-fatal sidecar notice (e.g. mic capture recovered after a
            // device change). Reuse the recording_error channel — it's a
            // transient auto-dismissing toast on the frontend.
            //
            // Logged BEFORE the device is folded in: the name is display-only
            // (#174) and stderr is not a display. "resumed on AirPods Pro" is
            // the fact the user needs and precisely the fact a log must not
            // keep, so the two strings deliberately differ.
            eprintln!("sidecar diagnostic: {message}");
            let _ = app.emit("recording_error", ErrorPayload {
                note_id: Some(note_id.to_string()),
                message: recovery_toast(message, input_device),
            });
            false
        }
        SidecarEvent::CaptureTiming(timing) => {
            // Diagnostics only: logged here, written beside the take's other
            // diagnostics by the post-stop chain (`write_capture_timing`).
            eprintln!("{}", timing.summary());
            *sink.capture_timing.lock() = Some(timing);
            false
        }
    }
}

/// Longest device name we will render, in code points.
///
/// MIRRORED with `MAX_DEVICE_NAME` in `src/components/RecordingBar.tsx` — the
/// warning pill and this toast must clamp a user-authored name the same way, or
/// the same device reads differently in two places. Change both.
const MAX_DEVICE_NAME: usize = 40;

/// Clamp a user-authored input-device name for display (#174).
///
/// MIRRORS `noAudioWarning`'s clamp in `src/components/RecordingBar.tsx`:
/// beyond [`MAX_DEVICE_NAME`] code points, take the first `MAX - 1`, drop a
/// trailing space so it can't be stranded before the ellipsis, and append one.
/// Rust's `chars()` and JS's `Array.from` both iterate code points, so the two
/// agree character-for-character and share their test vectors.
fn clamp_device_name(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    if chars.len() <= MAX_DEVICE_NAME {
        return name.to_string();
    }
    let head: String = chars[..MAX_DEVICE_NAME - 1].iter().collect();
    format!("{}…", head.trim_end())
}

/// Compose the user-facing device-change toast from the sidecar's device-free
/// sentence and, when it reported one, the device it resumed on (#174).
///
/// Taking the two separately is the privacy guarantee, not a comment about one:
/// the caller logs `message` to stderr and emits *this* to the UI, so a device
/// name cannot reach the log by construction. The name is clamped exactly as the
/// warning pill clamps it — see [`clamp_device_name`].
fn recovery_toast(message: String, input_device: Option<String>) -> String {
    match input_device {
        Some(device) => format!(
            "{} on {}.",
            message.trim_end_matches('.'),
            clamp_device_name(&device)
        ),
        None => message,
    }
}

/// Stand-in for a redacted value in a logged sidecar line.
const REDACTED_VALUE: &str = "\"<redacted>\"";

/// Strip the input-device name out of a raw sidecar line before it is logged
/// (#174).
///
/// The three `bad sidecar line` sites log the line verbatim, which is the right
/// diagnostic — you need to see what the sidecar actually emitted — but a
/// heartbeat carries a user-authored device name that routinely contains a real
/// person's name, and #174's constraint is that it reaches no log. The field
/// name is kept and only its value replaced: *which* fields were present is the
/// half of that diagnostic worth having, and it's the half that isn't sensitive.
///
/// Deliberately string-level, not serde: this only ever runs on a line that
/// already failed to parse, so there is no value to deserialize. Written to
/// leave a line it doesn't understand alone rather than to mangle it.
///
/// One limit, and it is inherent rather than a shortcut: a value whose inner
/// quotes are *not* escaped (`"Mike "Mic" Pro"`) is indistinguishable from a
/// string that legitimately ended at the first inner quote, so a fragment can
/// survive. No parser can do better on that input, and the sidecar cannot
/// produce it — `JSONSerialization` always escapes.
fn redact_sidecar_line(line: &str) -> String {
    const KEY: &str = "\"input_device\"";
    let Some(key_at) = line.find(KEY) else {
        return line.to_string();
    };
    let after_key = key_at + KEY.len();
    let rest = &line[after_key..];
    let Some(colon_rel) = rest.find(':') else {
        return line.to_string();
    };
    let value_rest = &rest[colon_rel + 1..];
    let trimmed = value_rest.trim_start();
    // Byte offset of the value itself. Every step lands on a char boundary: the
    // key and colon are ASCII, and `trim_start` cuts whole characters.
    let value_at = after_key + colon_rel + 1 + (value_rest.len() - trimmed.len());

    if trimmed.starts_with('"') {
        // A JSON string: find its closing quote, honouring backslash escapes so
        // a name containing `\"` doesn't end it early.
        let mut escaped = false;
        for (i, ch) in trimmed.char_indices().skip(1) {
            if escaped {
                escaped = false;
                continue;
            }
            match ch {
                '\\' => escaped = true,
                '"' => {
                    let end = value_at + i + ch.len_utf8();
                    return format!("{}{REDACTED_VALUE}{}", &line[..value_at], &line[end..]);
                }
                _ => {}
            }
        }
        // Unterminated — the rest of the line is inside the string, so drop it.
        return format!("{}{REDACTED_VALUE}", &line[..value_at]);
    }

    // Not a string (`null`, or garbage): redact up to the next delimiter.
    let end = value_at + trimmed.find([',', '}']).unwrap_or(trimmed.len());
    format!("{}{REDACTED_VALUE}{}", &line[..value_at], &line[end..])
}

/// How far the run has got, on the same per-note channel. Measured in audio
/// position (see [`crate::recording::ReplayProgress`]) because a replay's chunk
/// count isn't known until it ends. No rate limit: chunks are seconds apart.
fn emit_transcribe_progress(
    app: &AppHandle,
    note_id: &str,
    snapshot: crate::recording::ReplaySnapshot,
) {
    let _ = app.emit(
        "transcribe_status",
        crate::recording::TranscribeStatusPayload::progress(note_id, snapshot),
    );
}

/// Serialise the chunk log as a per-chunk JSONL — one entry per
/// utterance, not per speaker turn. The saved transcript still groups
/// same-label runs (for summary readability), but the playback view
/// wants finer granularity: each ~5–15 s VAD chunk is the natural
/// click-to-seek and highlight unit, so glueing them by label hides
/// internal sentence boundaries from the player and makes the active-
/// turn highlight feel sluggish on long monologues. Runs dedup first
/// so echoed mic chunks don't generate phantom entries.
fn serialize_timeline(
    chunks: &[ChunkRecord],
    split_chunk: &dyn Fn(&ChunkRecord) -> Vec<LabelledPiece>,
    label_offset: u32,
) -> String {
    let kept = dedup_mic_against_sys(chunks);
    let mut sorted: Vec<&ChunkRecord> = kept.iter().collect();
    sorted.sort_by_key(|c| {
        let source_rank = match c.source {
            ChunkSource::Mic => 0,
            ChunkSource::Sys => 1,
        };
        (c.start_ms, source_rank)
    });

    // Estimate per-piece end_ms before serialising. Three sources, in
    // priority order:
    //   1. Piece's own word timings: max(w.end_ms) — exact, used
    //      whenever the provider returned word-level data.
    //   2. Next entry's start: a piece can't outlast whatever audio
    //      came after it on the same stream. Conservative but always
    //      available.
    //   3. Word-count heuristic at ~350 ms/word (typical conversational
    //      rate), floored at 1 s. Only kicks in for the final piece
    //      on a stream when there are no words.
    //
    // The end_ms drives the player's overlap rendering: while currentTime
    // sits inside a piece's [start_ms, end_ms], that piece stays "active"
    // even after a later piece on the other source has begun. Slight
    // over-estimation is fine — the worst case is one extra row staying
    // lit a beat longer; under-estimation causes a visible skip.

    // First flatten chunks → pieces, tagging each with the source so
    // the next-same-source lookup works the same way it did per-chunk.
    struct Entry {
        source: ChunkSource,
        start_ms: u64,
        text: String,
        label: String,
        words: Vec<serde_json::Value>,
        max_word_end_abs: Option<u64>,
    }
    let mut entries: Vec<Entry> = Vec::new();
    // Parallel pieces vec used solely as input to
    // `bridge_short_interjections`, then drained back into `entries`
    // below. We can't bridge `entries` directly because the bridge's
    // sandwich rule reads `LabelledPiece.words` for duration; entries
    // hold absolute-time word JSON. Keeping a parallel `LabelledPiece`
    // sequence is the minimal way to ensure the timeline emits the
    // same speaker labels `build_labelled_transcript` does — without
    // this, the DB transcript collapses correctly but the playback
    // view (driven by timeline.jsonl) still renders per-chunk
    // speaker flicker.
    let mut bridge_pieces: Vec<LabelledPiece> = Vec::new();
    for chunk in &sorted {
        for piece in split_chunk(chunk) {
            let trimmed = piece.text.trim();
            if trimmed.is_empty() {
                continue;
            }
            // Convert word timings to stream-absolute by adding the
            // chunk's start_ms. The playback view's audio element runs
            // in the merged playback.wav timeline; word timestamps
            // inside a chunk are chunk-relative until we rebase here.
            let words_abs: Vec<serde_json::Value> = piece
                .words
                .iter()
                .map(|w| {
                    serde_json::json!({
                        "text": w.text,
                        "start_ms": chunk.start_ms.saturating_add(w.start_ms),
                        "end_ms": chunk.start_ms.saturating_add(w.end_ms),
                    })
                })
                .collect();
            // Piece start: first word's absolute start when present,
            // else the chunk's start (whole-chunk fallback piece).
            let start_ms = piece
                .words
                .first()
                .map(|w| chunk.start_ms.saturating_add(w.start_ms))
                .unwrap_or(chunk.start_ms);
            let max_word_end_abs = piece
                .words
                .iter()
                .map(|w| chunk.start_ms.saturating_add(w.end_ms))
                .max();
            entries.push(Entry {
                source: chunk.source,
                start_ms,
                text: trimmed.to_string(),
                label: piece.label.clone().unwrap_or_default(),
                words: words_abs,
                max_word_end_abs,
            });
            bridge_pieces.push(piece);
        }
    }
    bridge_short_interjections(&mut bridge_pieces);
    absorb_text_continuation_chains(&mut bridge_pieces);
    for (entry, bridged) in entries.iter_mut().zip(bridge_pieces.iter()) {
        entry.label = bridged.label.clone().unwrap_or_default();
    }

    let next_same_source_start: Vec<u64> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            entries
                .iter()
                .skip(i + 1)
                .find(|n| n.source == e.source)
                .map(|n| n.start_ms)
                .unwrap_or(0)
        })
        .collect();

    let mut out = String::new();
    for (i, entry) in entries.iter().enumerate() {
        let end_ms = if let Some(max_end) = entry.max_word_end_abs {
            max_end
        } else if next_same_source_start[i] > entry.start_ms {
            next_same_source_start[i]
        } else {
            let estimated = estimated_speech_ms(&entry.text);
            entry.start_ms.saturating_add(estimated)
        };
        let json = serde_json::json!({
            "start_ms": entry.start_ms,
            "end_ms": end_ms,
            "label": offset_speaker_label(&entry.label, label_offset),
            "text": entry.text,
            "words": entry.words,
            // Which stream the piece came from. The cross-session speaker
            // unification pass (#17) uses this to carry user renames from the
            // old timeline onto the freshly clustered one without letting a
            // mic-side rename leak onto a time-overlapping sys cluster (or
            // vice versa). Absent on timelines written by older builds —
            // readers treat that as "source unknown, match any".
            "source": match entry.source { ChunkSource::Mic => "mic", ChunkSource::Sys => "sys" },
        });
        out.push_str(&json.to_string());
        out.push('\n');
    }
    out
}

pub(crate) fn emit_status(app: &AppHandle, note_id: Option<&str>, phase: Phase) {
    emit_phase(app, RecordingStatus::plain(note_id, phase));
}

/// A stop phase of a capture that never transcribed as it ran (#146). Marked
/// rather than withheld: with no chunks to drain and no text to diarize the
/// chain lands on Idle in a few hundred milliseconds, and the Idle is what
/// makes the pending take visible. No drain counts either — there is nothing
/// pending to count.
fn emit_deferred_phase(app: &AppHandle, note_id: &str, phase: Phase) {
    emit_phase(app, RecordingStatus::deferred_phase(note_id, phase));
}

/// `Stopping` with the drain's progress attached (#182): `pending` is how many
/// transcribes were in flight, `done` how many have landed. Same channel and
/// same phase as before — the counts are additive, so a listener that reads
/// only `{noteId, phase}` sees exactly what it saw before.
fn emit_stopping_progress(app: &AppHandle, note_id: &str, pending: u32, done: u32) {
    emit_phase(app, RecordingStatus::stopping_progress(note_id, pending, done));
}

fn emit_phase(app: &AppHandle, status: RecordingStatus) {
    let phase = status.phase;
    let _ = app.emit("recording_status", status);
    // The tray's icon and its Start/Stop items are driven from here rather than
    // from each call site: this is the one funnel every phase change already
    // goes through, so the menu bar can't drift out of step with the pipeline.
    crate::menubar::on_phase(app, phase);
}

/// Per-note (re)diarize lifecycle (#187), on its own channel for the same
/// reason `transcribe_status` has one: re-diarize and unify are user actions on
/// an arbitrary note, so they must not reach `recording_status`, which describes
/// the live capture and nothing else.
fn emit_diarize_status(app: &AppHandle, note_id: &str, active: bool) {
    let _ = app.emit(
        "diarize_status",
        crate::recording::DiarizeStatusPayload {
            note_id: note_id.to_string(),
            active,
        },
    );
}

fn emit_error(app: &AppHandle, note_id: Option<&str>, message: &str) {
    let _ = app.emit("recording_error", ErrorPayload {
        note_id: note_id.map(|s| s.to_string()),
        message: message.to_string(),
    });
}

/// Tell the sync observer a note changed via a backend-driven write — recording
/// transcript, re-diarize, or a timeline edit — that bypasses the `notes_*`
/// commands (those ping the observer themselves; the manual transcript/speaker
/// edits in the UI go through `notes_update`, so they're already covered). This
/// enqueues a push so the generated transcript/summary actually replicate.
/// No-op under the open-source `NoopSync`.
pub(crate) fn note_changed_for_sync(app: &AppHandle, note_id: &str) {
    app.state::<AppState>().sync.note_upserted(note_id);
}

/// Tell the sync observer a recording *session* was created or changed (#16) —
/// a take finished recording/importing, or was re-diarized. Enqueues the
/// `note_sessions` metadata push (the binary assets are uploaded separately,
/// frontend-triggered). Without this explicit ping the session would never
/// sync, since the pipeline writes it to disk outside the `notes_*` commands.
/// No-op under the open-source `NoopSync`.
fn session_changed_for_sync(app: &AppHandle, note_id: &str, session_id: &str) {
    if session_id == sessions::LEGACY_SESSION_ID {
        return; // synthesized legacy take — synced via the notes.audio path
    }
    app.state::<AppState>().sync.session_upserted(note_id, session_id);
}

fn sidecar_path(_app: &AppHandle) -> Result<PathBuf, String> {
    // 1) Production / `tauri build`: Tauri copies external binaries next to
    //    the main executable inside the .app bundle's MacOS folder, with the
    //    triple suffix stripped. So look for ../MacOS/audio-capture first.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidates = [
                dir.join("audio-capture"),
                dir.join("audio-capture-aarch64-apple-darwin"),
                dir.join("audio-capture-x86_64-apple-darwin"),
            ];
            for c in candidates {
                if c.exists() {
                    return Ok(c);
                }
            }
        }
    }

    // 2) Dev (`tauri dev`): the binary lives under src-tauri/binaries/.
    if let Ok(cwd) = std::env::current_dir() {
        for triple in ["aarch64-apple-darwin", "x86_64-apple-darwin"] {
            let p = cwd.join(format!("src-tauri/binaries/audio-capture-{triple}"));
            if p.exists() { return Ok(p); }
            let p = cwd.join(format!("binaries/audio-capture-{triple}"));
            if p.exists() { return Ok(p); }
        }
    }

    Err("audio-capture sidecar not found".into())
}

#[cfg(test)]
mod chain_progress_tests {
    use super::*;

    fn every_chain() -> Vec<ChainProgress> {
        vec![
            ChainProgress::PostStop { note_id: "n".into() },
            ChainProgress::Rediarize { note_id: "n".into() },
            ChainProgress::Replay {
                note_id: "n".into(),
                progress: Arc::new(parking_lot::Mutex::new(ReplayProgress::new(Vec::new()))),
            },
            ChainProgress::Silent,
        ]
    }

    #[test]
    fn only_the_live_captures_chain_reports_on_recording_status() {
        // `recording_status` is a single global slot that also drives the tray,
        // so anything a user can start on an arbitrary note must stay off it —
        // its Idle would blank a recording running on a different note (#187).
        let on_global: Vec<Channel> = every_chain()
            .iter()
            .map(ChainProgress::channel)
            .filter(|c| *c == Channel::RecordingStatus)
            .collect();
        assert_eq!(on_global.len(), 1);
        assert_eq!(
            ChainProgress::PostStop { note_id: "n".into() }.channel(),
            Channel::RecordingStatus
        );
    }

    #[test]
    fn each_chain_reports_on_exactly_one_channel() {
        // One type owns the channel, so no chain can bracket on one and name
        // its steps on another.
        assert_eq!(
            ChainProgress::Rediarize { note_id: "n".into() }.channel(),
            Channel::DiarizeStatus
        );
        assert_eq!(
            ChainProgress::Replay {
                note_id: "n".into(),
                progress: Arc::new(parking_lot::Mutex::new(ReplayProgress::new(Vec::new()))),
            }
            .channel(),
            Channel::TranscribeStatus
        );
        assert_eq!(ChainProgress::Silent.channel(), Channel::None);
    }

    #[test]
    fn a_stream_step_counts_off_the_streams_it_has() {
        let counted = |present: &[bool]| -> Vec<(u32, u32)> {
            let streams = present.iter().filter(|p| **p).count() as u32;
            crate::recording::stream_reports(Step::Diarizing, streams)
                .iter()
                .filter_map(|r| r.counter.map(|c| (c.index, c.count)))
                .collect()
        };
        // Mic-only and sys-only are one unit: nothing to count, so no counter.
        assert!(counted(&[true]).is_empty());
        assert!(counted(&[true, false]).is_empty());
        assert!(counted(&[false, true]).is_empty());
        assert_eq!(counted(&[true, true]), vec![(1, 2), (2, 2)]);
        // A stream that isn't there is not counted, so a third source is one
        // more flag rather than a rewrite of every literal.
        assert_eq!(counted(&[true, true, true]), vec![(1, 3), (2, 3), (3, 3)]);
    }

    #[test]
    fn a_stream_step_never_counts_past_its_own_total() {
        let reports = crate::recording::stream_reports(Step::SavingAudio, 2);
        assert_eq!(reports.len(), 2);
        for r in &reports {
            let c = r.counter.expect("two streams carry a counter");
            assert!(c.index <= c.count, "{c:?}");
        }
        // `begin` runs out rather than emitting a third report over two
        // streams, so an extra call cannot render "3 of 2".
        let mut iter = crate::recording::stream_reports(Step::SavingAudio, 2).into_iter();
        assert!(iter.next().is_some());
        assert!(iter.next().is_some());
        assert!(iter.next().is_none());
    }
}

#[cfg(test)]
mod diarize_tests {
    use super::*;
    use super::diarization::{combine_with_snapshot, session_speaker_offset};
    use super::timeline::{comparable_words, orphaned_prefix, projection_covers, synthesize_orphan_timeline};
    use super::transcript::{build_labelled_transcript, single_piece};

    fn mic(start_ms: u64, text: &str) -> ChunkRecord {
        ChunkRecord {
            source: ChunkSource::Mic,
            start_ms,
            text: text.to_string(),
            words: Vec::new(),
            detected_language: None,
        }
    }

    fn sys(start_ms: u64, text: &str) -> ChunkRecord {
        ChunkRecord {
            source: ChunkSource::Sys,
            start_ms,
            text: text.to_string(),
            words: Vec::new(),
            detected_language: None,
        }
    }

    /// Build a sys chunk with explicit word timings. Each `(text,
    /// start_ms, end_ms)` is chunk-relative, matching how the
    /// transcribe path stores them.
    fn sys_with_words(
        start_ms: u64,
        words: Vec<(&str, u64, u64)>,
    ) -> ChunkRecord {
        let text = words.iter().map(|(t, _, _)| *t).collect::<Vec<_>>().join(" ");
        ChunkRecord {
            source: ChunkSource::Sys,
            start_ms,
            text,
            words: words
                .into_iter()
                .map(|(t, s, e)| crate::recording::ChunkWord {
                    text: t.to_string(),
                    start_ms: s,
                    end_ms: e,
                })
                .collect(),
            detected_language: None,
        }
    }

    /// Wrap a simple chunk-level labeller as a piece producer that
    /// emits one whole-chunk piece. Lets the existing tests keep their
    /// `Fn(&ChunkRecord) -> Option<String>` shape while
    /// `build_labelled_transcript` consumes the new
    /// `Fn(&ChunkRecord) -> Vec<LabelledPiece>` signature.
    fn whole_chunk_pieces<F: Fn(&ChunkRecord) -> Option<String>>(
        labeller: F,
    ) -> impl Fn(&ChunkRecord) -> Vec<LabelledPiece> {
        move |c: &ChunkRecord| single_piece(c, labeller(c))
    }

    /// Issue #169: the transcript and the timeline are written from the same
    /// post-stop pass, but only the transcript gets the prior-take snapshot
    /// prepended — `serialize_timeline` sees this session's chunks alone. So a
    /// snapshot that no *earlier session's* timeline accounts for is text that
    /// exists in `note.transcript` and in no timeline at all. The styled reader
    /// renders from the merged timelines, so that text is invisible while the
    /// edit textarea (bound to the raw string) shows it — and any later
    /// `rebuild_note_transcript` (cycle a chunk label, delete a chunk,
    /// re-diarize, unify) rewrites the transcript to the timeline projection
    /// and deletes it outright.
    ///
    /// A note reaches that state when a recording lands text but writes no
    /// session assets — the diarize-model-missing early return is the route we
    /// have evidence for — and a later take on the same note becomes the only
    /// session with a timeline.
    ///
    /// Closed by repair-on-open: `orphaned_prefix` spots the snapshot the
    /// projection doesn't cover and `synthesize_orphan_timeline` gives it a
    /// session, so the merged timeline accounts for the whole transcript.
    #[test]
    fn orphaned_snapshot_text_survives_in_the_timeline() {
        // Take 1 left this behind: live-appended text, unlabelled, because the
        // pass that would have labelled it (and written its timeline) returned
        // early when the diarize model was missing.
        let snapshot = "we kicked off by agreeing the deadline slips a week";

        // Take 2, diarized normally: its own chunks, its own timeline.
        let chunks = vec![
            sys(30, "so where did we land on the freeze."),
            mic(4_000, "pushing it to the following Friday."),
        ];
        let labeller = |c: &ChunkRecord| match c.source {
            ChunkSource::Mic => Some("You".to_string()),
            ChunkSource::Sys => Some("Speaker 1".to_string()),
        };
        let split = whole_chunk_pieces(labeller);

        let combined = combine_with_snapshot(snapshot, &build_labelled_transcript(&chunks, &split));
        let session_two = serialize_timeline(&chunks, &split, session_speaker_offset(snapshot));

        // What opening the note does: project the sessions that exist, find
        // the text the projection can't account for, and give it a session of
        // its own in front of them.
        let projection = group_values_to_transcript(&jsonl_values(&session_two));
        let orphans =
            orphaned_prefix(&combined, &projection).expect("the snapshot is orphaned today");
        let timeline = format!("{}{}", synthesize_orphan_timeline(&orphans), session_two);

        // What the reader can actually show: every text the merged timeline
        // carries. Anything in the transcript but absent here is unreachable.
        let shown: String = timeline
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| {
                serde_json::from_str::<serde_json::Value>(l)
                    .ok()?
                    .get("text")?
                    .as_str()
                    .map(|s| s.to_string())
            })
            .collect::<Vec<_>>()
            .join(" ");

        for line in combined.lines().filter(|l| !l.trim().is_empty()) {
            // Strip any "Label: " prefix — the reader draws identity as a dot
            // rather than text, so only the words themselves must be reachable.
            let words = line.split_once(": ").map_or(line, |(_, rest)| rest);
            assert!(
                shown.contains(words),
                "transcript line is in no timeline entry, so the reader cannot \
                 show it and a rebuild would delete it: {words:?}",
            );
        }

        // The rebuild paths are the other half of the bug: `rebuild_note_transcript`
        // derives the transcript from the timelines alone, so before the repair
        // cycling one speaker pill deleted the snapshot outright. After it, a
        // rebuild reproduces the same words.
        let repaired_projection = group_values_to_transcript(&jsonl_values(&timeline));
        assert_eq!(
            comparable_words(&repaired_projection),
            comparable_words(&combined),
            "a rebuild after the repair must not change the transcript's words",
        );
        // ...which is also what makes the repair a fixed point: a second open
        // finds nothing left to account for, so it adds no second session.
        assert!(
            projection_covers(&combined, &repaired_projection)
                && orphaned_prefix(&combined, &repaired_projection).is_none(),
            "repair must be idempotent",
        );
    }

    /// Parse a serialised timeline back into the values the projection and
    /// per-chunk edit paths work on. `read_timeline_values` does this from a
    /// path; tests hold the JSONL in memory.
    fn jsonl_values(jsonl: &str) -> Vec<serde_json::Value> {
        jsonl
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    // ---- Orphaned transcript text (#169) --------------------------------

    /// The diarize-skipped path still has to leave a timeline behind, or the
    /// text it live-appended becomes the next orphan. No model means nothing
    /// can be said about who spoke — so the label is empty, and *only* the
    /// label: word timings describe when each word landed, which diarization
    /// has no bearing on, so they are kept and the note keeps per-word
    /// highlighting.
    #[test]
    fn diarize_skipped_timeline_carries_text_and_timings_with_no_label() {
        let chunks = vec![
            sys_with_words(0, vec![("where", 0, 400), ("now", 400, 900)]),
            sys_with_words(4_000, vec![("friday", 0, 500)]),
        ];
        let split = |c: &ChunkRecord| single_piece(c, None);
        let values = jsonl_values(&serialize_timeline(&chunks, &split, 0));
        assert_eq!(values.len(), 2);
        for v in &values {
            assert_eq!(v["label"], "");
        }
        assert_eq!(values[0]["text"], "where now");
        assert_eq!(values[1]["text"], "friday");
        // The provider's word timings survive a missing diarize model, rebased
        // to stream-absolute as usual...
        assert_eq!(values[0]["words"][1]["text"], "now");
        assert_eq!(values[0]["words"][1]["start_ms"], 400);
        assert_eq!(values[1]["words"][0]["start_ms"], 4_000);
        // ...which also gives the entry the tighter bounds `serialize_timeline`
        // derives from words rather than the coarse chunk fallback.
        assert_eq!(values[0]["end_ms"], 900);
        // And it projects back to the text the live append had already saved.
        assert_eq!(group_values_to_transcript(&values), "where now friday");
    }

    // ---- Per-session (#16) label offset + timeline plumbing -------------

    #[test]
    fn session_streams_reflects_present_sources() {
        assert_eq!(session_streams(&[mic(0, "a")]), vec!["mic"]);
        assert_eq!(session_streams(&[sys(0, "a")]), vec!["sys"]);
        assert_eq!(
            session_streams(&[mic(0, "a"), sys(100, "b")]),
            vec!["mic", "sys"]
        );
        assert!(session_streams(&[]).is_empty());
    }

    #[test]
    fn serialize_timeline_applies_label_offset() {
        // A resumed take's timeline `Speaker N` labels get bumped past the
        // prior take's, matching the DB transcript combine_with_snapshot
        // produces. Two same-speaker sys chunks (terminated so the
        // continuation absorber leaves them be).
        let chunks = vec![sys(0, "alpha beta gamma."), sys(5_000, "delta epsilon zeta.")];
        let labeller = |c: &ChunkRecord| single_piece(c, Some("Speaker 1".to_string()));
        let out = serialize_timeline(&chunks, &labeller, 2);
        let labels: Vec<String> = out
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                serde_json::from_str::<serde_json::Value>(l)
                    .unwrap()
                    .get("label")
                    .and_then(|v| v.as_str())
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert!(!labels.is_empty());
        assert!(labels.iter().all(|l| l == "Speaker 3"), "labels = {labels:?}");
    }

    #[test]
    fn serialize_timeline_leaves_you_and_zero_offset_untouched() {
        // "You" is never offset; offset 0 is a no-op.
        let chunks = vec![mic(0, "just me talking.")];
        let labeller = |c: &ChunkRecord| single_piece(c, Some("You".to_string()));
        let out = serialize_timeline(&chunks, &labeller, 4);
        assert!(out.contains("\"label\":\"You\""));
    }
}

#[cfg(test)]
mod import_tests {
    use super::*;

    // #146: a take captured with "Transcribe manually" on has no chunks, so
    // `session_streams` (which reads them) would record it as having captured
    // nothing — leaving the carousel tooltip blank on the one kind of take the
    // user is most likely to inspect before pressing Transcribe.
    #[test]
    fn a_deferred_takes_streams_come_from_the_wavs_not_the_chunks() {
        let snapshot = PostStopSnapshot {
            mic_wav: Some(PathBuf::from("/tmp/mic-full.wav")),
            sys_wav: Some(PathBuf::from("/tmp/sys-full.wav")),
            chunks: vec![],
            transcript_at_start: String::new(),
            session_id: "s1".into(),
            session_started_at: String::new(),
            deferred: true,
            captured_duration_ms: 0,
            capture_timing: None,
        };
        assert_eq!(retained_streams(&snapshot), vec!["mic", "sys"]);
        assert!(session_streams(&snapshot.chunks).is_empty());

        let mic_only = PostStopSnapshot {
            sys_wav: None,
            ..snapshot
        };
        assert_eq!(retained_streams(&mic_only), vec!["mic"]);
    }

    #[tokio::test]
    async fn import_backlog_semaphore_bounds_concurrency() {
        // The import reader acquires a permit before spawning each chunk's
        // transcribe task; with IMPORT_BACKLOG_PERMITS outstanding the next
        // acquire must block. This pins the invariant the bounded-backlog
        // backpressure relies on (a full-speed replay can't pile up unboundedly).
        let sem = Arc::new(Semaphore::new(IMPORT_BACKLOG_PERMITS));
        let mut held = Vec::new();
        for _ in 0..IMPORT_BACKLOG_PERMITS {
            held.push(sem.clone().acquire_owned().await.unwrap());
        }
        assert!(
            sem.clone().try_acquire_owned().is_err(),
            "backlog should be saturated at IMPORT_BACKLOG_PERMITS"
        );
        // Releasing one permit (a chunk finished) frees a slot again.
        held.pop();
        assert!(
            sem.try_acquire_owned().is_ok(),
            "a freed permit should be acquirable"
        );
    }
}

#[cfg(test)]
mod device_name_tests {
    use super::*;

    #[test]
    fn a_long_device_name_is_clamped() {
        // #174: device names are user-authored, so their length is not ours to
        // trust. MIRRORS the TS rule in `noAudioWarning` (RecordingBar.tsx) —
        // >40 code points becomes the first 39, trailing space trimmed, plus an
        // ellipsis. This vector is pinned identically on the TS side; change
        // both or they drift.
        let name = format!("{} tail", "b".repeat(38));
        assert_eq!(clamp_device_name(&name), format!("{}…", "b".repeat(38)));
    }

    #[test]
    fn clamping_counts_code_points_not_bytes() {
        // The granularity is what makes the mirror exact: Rust `chars()` and JS
        // `Array.from` both walk code points, so an emoji at the boundary is
        // kept whole on both sides. Counting bytes here would split it and emit
        // invalid UTF-8; counting UTF-16 units on the TS side would emit U+FFFD.
        // Same vector is pinned in RecordingBar.test.tsx.
        let name = format!("{}🎧🎧🎧", "a".repeat(38));
        assert_eq!(clamp_device_name(&name), format!("{}🎧…", "a".repeat(38)));
    }

    #[test]
    fn a_recovery_toast_splices_the_device_before_the_full_stop() {
        // The sidecar's sentence ends in a full stop; the device goes inside it
        // rather than trailing after as a second fragment.
        assert_eq!(
            recovery_toast(
                "Audio input device changed; microphone capture resumed.".into(),
                Some("AirPods Pro".into()),
            ),
            "Audio input device changed; microphone capture resumed on AirPods Pro.",
        );
    }

    #[test]
    fn a_recovery_toast_without_a_device_is_the_message_unchanged() {
        let msg = "Audio input device changed; microphone capture resumed.";
        assert_eq!(recovery_toast(msg.into(), None), msg);
    }

    #[test]
    fn a_recovery_toast_clamps_the_device_it_splices() {
        // The toast and the warning pill clamp the same name the same way; an
        // unclamped toast was the gap this closes.
        let long = format!("{}🎧🎧🎧", "a".repeat(38));
        let toast = recovery_toast("Resumed.".into(), Some(long));
        assert!(toast.contains(&format!("{}🎧…", "a".repeat(38))), "not clamped: {toast}");
    }

    #[test]
    fn a_malformed_line_is_logged_without_the_device_name() {
        // #174's constraint says the name must not reach a log. The three
        // "bad sidecar line" sites log the RAW line, so a heartbeat that fails
        // to parse would put the name in stderr — the one channel the rest of
        // this feature is careful to keep it out of. Keep the field, drop the
        // value: which fields were present is the diagnostic worth having.
        let line = r#"{"event":"heartbeat","mic_peak":0.0,"input_device":"Michael's AirPods Pro"}"#;
        let out = redact_sidecar_line(line);
        assert!(!out.contains("Michael"), "device name survived: {out}");
        assert!(!out.contains("AirPods"), "device name survived: {out}");
        assert!(out.contains("input_device"), "field name should survive: {out}");
        assert!(out.contains("mic_peak"), "rest of the line should survive: {out}");
    }

    #[test]
    fn a_line_with_no_device_is_left_alone() {
        let line = r#"{"event":"heartbeat","mic_peak":0.5,"chunks":3}"#;
        assert_eq!(redact_sidecar_line(line), line);
    }

    #[test]
    fn an_escaped_quote_inside_the_name_does_not_end_it_early() {
        // A device named `Mike "Mic" Pro` arrives with its inner quotes escaped,
        // as `JSONSerialization` emits them. Ending the value at the first inner
        // quote would leave the rest of the name in the log.
        let line = r#"{"input_device":"Mike \"Mic\" Pro","chunks":1}"#;
        let out = redact_sidecar_line(line);
        assert!(!out.contains("Mic"), "name fragment survived: {out}");
        assert!(out.contains(r#""chunks":1"#), "tail should survive: {out}");
    }

    #[test]
    fn an_unterminated_name_takes_the_rest_of_the_line_with_it() {
        // Truncated mid-write: everything after the opening quote is inside the
        // string, so none of it can be shown.
        let out = redact_sidecar_line(r#"{"input_device":"Michael's AirP"#);
        assert!(!out.contains("Michael"), "name survived: {out}");
        assert!(out.contains("input_device"), "field should survive: {out}");
    }

    #[test]
    fn a_non_string_device_value_is_still_redacted() {
        let out = redact_sidecar_line(r#"{"input_device":null,"chunks":2}"#);
        assert!(!out.contains("null"), "value survived: {out}");
        assert!(out.contains(r#""chunks":2"#), "tail should survive: {out}");
    }

    #[test]
    fn a_short_device_name_is_returned_verbatim() {
        assert_eq!(clamp_device_name("MacBook Pro-mikrofon"), "MacBook Pro-mikrofon");
        assert_eq!(clamp_device_name(""), "");
    }
}
