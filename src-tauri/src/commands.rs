use crate::db;
use crate::diarize;
use crate::local_whisper;
use crate::sessions;
use crate::wav;
use crate::recording::{CaptureSink, ChunkRecord, ChunkSource, DiagnosticPayload, ErrorPayload, Inflight, Phase, RecordingStatus, ReplayProgress, ReplayTakeTimings, ReplayTimings, SidecarEvent, SinkMode, StartClaim, Step, StepReport, TakeWork, TranscriptPayload};
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
mod webhook;
pub use api_keys::*;
pub use assets::*;
pub use chat::*;
pub use clients::*;
pub use copilot::*;
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
use transcribe::{drop_incidental_stream_hallucinations, record_detected_language, transcribe_chunk};
use transcript::{
    absorb_text_continuation_chains, bridge_short_interjections, build_labelled_transcript,
    build_pieces_unbridged, dedup_mic_against_sys, estimated_speech_ms, single_piece,
    split_by_labels, split_by_segments, LabelledPiece,
};
use timeline::{commit_rebuilt_transcript, group_values_to_transcript, read_timeline_values, rebuild_note_transcript};

pub(crate) const DEFAULT_LANGUAGE: &str = "no";
// The diarization engine when `diarize_model` is unset. Only a fresh install
// has no row, since `db::migrate_diarize_engine` writes one for every other.
// Mirrored by `DEFAULT_DIARIZE_ENGINE` in `src/lib/diarizeEngine.ts`.
const DEFAULT_DIARIZE_MODEL: &str = "nemotron3";

// Matches the sidecar's own default. Stored as a string and parsed at use.
const DEFAULT_COMMUNITY1_THRESHOLD: &str = "0.5";

const DIARIZE_MODEL_MISSING: &str = "Diarize model isn't downloaded. Download it in Settings → Transcription → Speaker labels, then try again.";
const DIARIZE_MODEL_MISSING_UNLABELLED: &str = "Speaker diarization model isn't downloaded — transcript saved without speaker labels. Download it from Settings → Transcription → Speaker labels.";
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

/// Re-run diarization on a note's saved audio with the current settings,
/// then rebuild and write back the labelled transcript. Lets the user
/// iterate on diarize_model + thresholds without re-recording.
///
/// Requires both `keep_audio` to have been on at recording time (to
/// have the WAV files) and at least one prior diagnostic dump (to read
/// the original chunk timings — those are the alignment anchor and
/// can't be reconstructed from the transcript text alone).
#[tauri::command]
pub async fn rediarize_note(app: AppHandle, note_id: String) -> Result<(), String> {
    let result = rediarize_note_inner(app.clone(), note_id.clone()).await;
    notes::purge_note_assets_if_gone(&app, &note_id);
    result
}

/// The command's body, split out so the purge check in [`rediarize_note`] covers
/// every one of its returns.
async fn rediarize_note_inner(app: AppHandle, note_id: String) -> Result<(), String> {
    let app_dir = app.path().app_data_dir().map_err(err)?;
    // Re-diarize operates on the most recent session's retained audio. Legacy
    // flat notes resolve to the flat dir; multi-session notes re-diarize the
    // latest take (earlier takes keep their existing labels — full
    // cross-session unification is issue #17).
    let recordings = sessions::recordings_dir(&app_dir, &note_id);

    // Multi-session notes: "Re-diarize" means the cross-session unify pass
    // (#17) — fresh clustering over every take's concatenated audio so one
    // voice carries one label across takes. Falls back to the latest-take
    // re-diarize below when fewer than two sessions have retained source
    // audio + chunk timings (legacy notes, takes recorded before
    // auto-retention). Single-session notes never enter this branch and
    // keep today's behaviour exactly.
    if sessions::resolve_sessions(&recordings).len() >= 2 {
        if !note_diarize_settings(&app, &note_id).await.choice.downloaded {
            return Err(DIARIZE_MODEL_MISSING.to_string());
        }
        // On the per-note channel, never the global phase: a recording may be
        // running on another note throughout (#187).
        let progress = ChainProgress::Rediarize { note_id: note_id.clone() };
        progress.started(&app);
        match unify_note_speakers(&app, &note_id, &progress).await {
            Ok(Some(_)) => {
                progress.finished(&app);
                return Ok(());
            }
            Ok(None) => {
                // Not enough retained per-session audio to unify — fall
                // through to the latest-take re-diarize.
                progress.finished(&app);
            }
            Err(e) => {
                progress.finished(&app);
                return Err(format!("Speaker unification failed: {e}"));
            }
        }
    }

    let audio_dir = sessions::latest_session_dir(&recordings);
    let session_id = sessions::resolve_sessions(&recordings)
        .last()
        .map(|(e, _)| e.id.clone())
        .unwrap_or_else(|| sessions::LEGACY_SESSION_ID.to_string());
    let mic_path = audio_dir.join("mic.wav");
    let sys_path = audio_dir.join("sys.wav");
    let mic_wav = if mic_path.exists() { Some(mic_path) } else { None };
    let sys_wav = if sys_path.exists() { Some(sys_path) } else { None };
    if mic_wav.is_none() && sys_wav.is_none() {
        return Err(
            "No saved audio for this note. Turn on Keep recorded audio in Settings → Recording before recording, then try again on a new recording."
                .to_string(),
        );
    }

    let chunks = read_chunks_for_note(&app, &note_id).map_err(err)?;
    if chunks.is_empty() {
        return Err(
            "No saved chunk timings for this note. Re-diarize needs them to realign speaker labels against the transcript, and they're only written for recordings made with Keep recorded audio on (Settings → Recording). Make a new recording with it on, then try again."
                .to_string(),
        );
    }

    let settings = note_diarize_settings(&app, &note_id).await;
    if !settings.choice.downloaded {
        return Err(DIARIZE_MODEL_MISSING.to_string());
    }

    let progress = ChainProgress::Rediarize { note_id: note_id.clone() };
    rediarize_apply_to_chunks(
        app,
        note_id,
        session_id,
        mic_wav,
        sys_wav,
        chunks,
        settings,
        LabelFallback::Abort,
        &progress,
    )
    .await
    .map(|_| ())
    .map_err(err)
}

/// Parse chunk records from a single JSON file matching the shape
/// `{ "chunks": [{source, start_ms, text, words?}, ...] }`. Used for both
/// the standalone `chunks.json` written next to retained audio and the
/// per-engine diagnostic dumps under `diagnostics/<note_id>/`.
fn parse_chunks_json(path: &std::path::Path) -> anyhow::Result<Vec<ChunkRecord>> {
    let data = std::fs::read_to_string(path)?;
    let v: serde_json::Value = serde_json::from_str(&data)?;
    let Some(arr) = v.get("chunks").and_then(|c| c.as_array()) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(arr.len());
    for c in arr {
        let source = match c.get("source").and_then(|s| s.as_str()) {
            Some("mic") => ChunkSource::Mic,
            Some("sys") => ChunkSource::Sys,
            _ => continue,
        };
        let start_ms = c.get("start_ms").and_then(|s| s.as_u64()).unwrap_or(0);
        let text = c
            .get("text")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        // Word timings are present on dumps written by builds that
        // include word-level diarize splitting; older dumps stored
        // chunks without them. Empty `words` makes `split_by_segments`
        // fall back to whole-chunk labelling (one label per chunk via
        // start_ms), matching pre-split behaviour for those notes.
        let words: Vec<crate::recording::ChunkWord> = c
            .get("words")
            .and_then(|w| w.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|w| {
                        let text = w.get("text").and_then(|s| s.as_str())?.to_string();
                        let ws = w.get("start_ms").and_then(|s| s.as_u64())?;
                        let we = w.get("end_ms").and_then(|s| s.as_u64())?;
                        Some(crate::recording::ChunkWord { text, start_ms: ws, end_ms: we })
                    })
                    .collect()
            })
            .unwrap_or_default();
        // Replayed from a saved diagnostic dump — those predate the field
        // and don't carry a detection.
        out.push(ChunkRecord { source, start_ms, text, words, detected_language: None });
    }
    Ok(out)
}

/// Read chunk records from any of the saved diagnostic JSONs for a
/// note. All dumps for the same note hold the same chunk timings (they
/// come from the original recording session and don't depend on engine
/// or threshold), so we just take the first JSON we find.
fn read_chunks_from_diagnostic(diag_dir: &std::path::Path) -> anyhow::Result<Vec<ChunkRecord>> {
    if !diag_dir.exists() {
        return Ok(Vec::new());
    }
    for entry in std::fs::read_dir(diag_dir)?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let chunks = parse_chunks_json(&path)?;
        if !chunks.is_empty() {
            return Ok(chunks);
        }
    }
    Ok(Vec::new())
}

/// Resolve chunk records for a note. Prefers the standalone
/// `recordings/<note_id>/chunks.json` (written at recording-stop time
/// alongside retained audio — survives a failed diarize), falls back to
/// any diagnostic dump for backward compat with notes recorded before
/// chunks.json existed.
fn read_chunks_for_note(app: &AppHandle, note_id: &str) -> anyhow::Result<Vec<ChunkRecord>> {
    let app_dir = app.path().app_data_dir()?;
    let recordings = sessions::recordings_dir(&app_dir, note_id);
    let chunks_path = sessions::latest_session_dir(&recordings).join("chunks.json");
    if chunks_path.exists() {
        let chunks = parse_chunks_json(&chunks_path)?;
        if !chunks.is_empty() {
            return Ok(chunks);
        }
    }
    let diag_dir = app_dir.join("diagnostics").join(note_id);
    read_chunks_from_diagnostic(&diag_dir)
}

/// What a (re)diarize pass does when speaker labels can't be produced — the
/// model isn't downloaded, the sidecar failed, or clustering returned nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LabelFallback {
    /// Abort and leave the note exactly as it was. Right for the user-pressed
    /// **Re-diarize**: the text is already correct and only the labels were in
    /// question, so a failure should change nothing.
    Abort,
    /// Write the turns with no `Speaker N:` prefix and carry on. Required for
    /// deferred transcription (#146), where this pass is what puts the text
    /// into the note at all — under ADR-0004 a take that transcribed something
    /// always writes a session, labels or not, so the words stay visible in
    /// the reader instead of being thrown away with the labels.
    Unlabelled,
}

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

/// What the named steps inside one take's (re)diarize cost, in ms.
struct TakeStepCosts {
    /// `None` when the take had no system stream beside its mic.
    echo_ms: Option<u64>,
    playback_ms: u64,
}

/// Mirror of [`diarize_and_apply`]'s branching over caller-supplied paths and
/// chunks instead of recording-session state. No snapshot — the transcript is
/// rebuilt from the chunk timings rather than appended to an in-flight session.
async fn rediarize_apply_to_chunks(
    app: AppHandle,
    note_id: String,
    session_id: String,
    mic_wav: Option<PathBuf>,
    sys_wav: Option<PathBuf>,
    chunks: Vec<ChunkRecord>,
    settings: NoteDiarizeSettings,
    fallback: LabelFallback,
    progress: &ChainProgress,
) -> anyhow::Result<TakeStepCosts> {
    let NoteDiarizeSettings { choice, thresholds, expected_speakers } = settings;
    // Before deciding the capture mode — a hallucinated chunk on an otherwise
    // silent stream would misclassify the whole recording. Also cleans up notes
    // recorded before the transcribe-time guard existed, which is the point of
    // running it here rather than trusting the chunk log.
    let chunks = drop_incidental_stream_hallucinations(chunks);
    if chunks.is_empty() {
        progress.finished(&app);
        return Err(anyhow::anyhow!("no usable chunks after dropping collapsed ones"));
    }
    let mic_chunks_present = chunks.iter().any(|c| c.source == ChunkSource::Mic);
    let sys_chunks_present = chunks.iter().any(|c| c.source == ChunkSource::Sys);

    progress.started(&app);

    type Splitter = dyn Fn(&ChunkRecord) -> Vec<LabelledPiece> + Send;
    struct DiarizeStage {
        splitter: Box<Splitter>,
        // Cloned so the diagnostic dump can resolve per-word speaker IDs
        // after the originals have been moved into the closure. Kept per
        // source: a hybrid recording diarizes both streams, and a word's
        // speaker id only means anything against its own stream's segments.
        mic_segments_for_dump: Vec<diarize::Segment>,
        sys_segments_for_dump: Vec<diarize::Segment>,
        source_tag: &'static str,
        echo: Option<crate::echo::EchoReport>,
        echo_ms: Option<u64>,
    }
    // Under `Unlabelled`, a diarize that can't run is not a failure — the text
    // still has to reach the note (#146 / ADR-0004), just without `Speaker N:`
    // prefixes. Word timings are kept: diarization decides *who* spoke, not
    // when each word landed, so its absence says nothing about the timings the
    // provider returned.
    let unlabelled_stage = |why: &str| -> DiarizeStage {
        eprintln!("diarize: {why}; writing the timeline without speaker labels");
        DiarizeStage {
            splitter: Box::new(|c: &ChunkRecord| single_piece(c, None)),
            mic_segments_for_dump: Vec::new(),
            sys_segments_for_dump: Vec::new(),
            source_tag: "unlabelled",
            echo: None,
            echo_ms: None,
        }
    };
    // The model being absent is the ordinary case of that, and worth catching
    // before the per-stream branches so neither has to. Under `Abort` the
    // caller has already refused to start without one.
    let model_missing = fallback == LabelFallback::Unlabelled && !choice.downloaded;
    if model_missing {
        emit_error(&app, Some(&note_id), DIARIZE_MODEL_MISSING_UNLABELLED);
    }
    let mut engine = choice.engine;

    let stage: DiarizeStage = if model_missing {
        unlabelled_stage("model not downloaded")
    } else {
        match (mic_chunks_present, sys_chunks_present) {
        (true, false) => {
            let mut diarize_steps =
                StreamSteps::new(&app, progress, Step::Diarizing, &[mic_wav.is_some()]);
            let outcome: Result<Vec<diarize::Segment>, String> = match mic_wav.clone() {
                None => Err("mic chunks present but no saved mic.wav".to_string()),
                Some(wav) => {
                    diarize_steps.begin();
                    match diarize_and_maybe_clean(&app, &note_id, &wav, expected_speakers, &mut engine, thresholds)
                        .await
                    {
                        Err(e) => Err(format!("mic diarize failed: {e}")),
                        Ok(segs) if segs.is_empty() => {
                            // No splitter to build → write the bare diagnostic
                            // (no pieces) so the user can still inspect
                            // "diarize ran but found nothing".
                            write_diagnostics_json(
                                &app, &note_id, engine, "mic", &segs, &[], &chunks, &thresholds,
                                None, None,
                            )
                            .await;
                            Err("diarize returned no segments".to_string())
                        }
                        Ok(segs) => Ok(segs),
                    }
                }
            };
            match outcome {
                Err(why) if fallback == LabelFallback::Unlabelled => unlabelled_stage(&why),
                Err(why) => {
                    progress.finished(&app);
                    return Err(anyhow::anyhow!(why));
                }
                Ok(segments) => {
                    let segments_for_dump = segments.clone();
                    let display_map = build_display_map(&chunks, &segments, ChunkSource::Mic);
                    DiarizeStage {
                        splitter: Box::new(move |c: &ChunkRecord| {
                            split_by_segments(c, &segments, &display_map)
                        }),
                        mic_segments_for_dump: segments_for_dump,
                        sys_segments_for_dump: Vec::new(),
                        source_tag: "mic",
                        echo: None,
                        echo_ms: None,
                    }
                }
            }
        }
        (false, true) => {
            let mut diarize_steps =
                StreamSteps::new(&app, progress, Step::Diarizing, &[sys_wav.is_some()]);
            let outcome: Result<Vec<diarize::Segment>, String> = match sys_wav.clone() {
                None => Err("sys chunks present but no saved sys.wav".to_string()),
                Some(wav) => {
                    diarize_steps.begin();
                    match diarize_and_maybe_clean(&app, &note_id, &wav, expected_speakers, &mut engine, thresholds)
                        .await
                    {
                        Err(e) => Err(format!("sys diarize failed: {e}")),
                        Ok(segs) if segs.is_empty() => {
                            write_diagnostics_json(
                                &app, &note_id, engine, "sys", &[], &segs, &chunks, &thresholds,
                                None, None,
                            )
                            .await;
                            Err("diarize returned no segments".to_string())
                        }
                        Ok(segs) => Ok(segs),
                    }
                }
            };
            match outcome {
                Err(why) if fallback == LabelFallback::Unlabelled => unlabelled_stage(&why),
                Err(why) => {
                    progress.finished(&app);
                    return Err(anyhow::anyhow!(why));
                }
                Ok(segments) => {
                    let segments_for_dump = segments.clone();
                    let display_map = build_display_map(&chunks, &segments, ChunkSource::Sys);
                    DiarizeStage {
                        splitter: Box::new(move |c: &ChunkRecord| {
                            split_by_segments(c, &segments, &display_map)
                        }),
                        mic_segments_for_dump: Vec::new(),
                        sys_segments_for_dump: segments_for_dump,
                        source_tag: "sys",
                        echo: None,
                        echo_ms: None,
                    }
                }
            }
        }
        (true, true) => {
            // Hybrid: diarize both streams and number them off one counter.
            // `You` is kept only when the mic resolves to a single voice — see
            // `build_hybrid_labels`. System stream first, mic second with the
            // remaining head-count: see `hybrid_sys_hint` / `mic_hint_after_sys`
            // for why the hint is worth more on the mic, and the echo pass
            // first. Mirrors `diarize_and_apply`'s hybrid branch; change both
            // together.
            let mut echo = EchoPass::run(mic_wav.as_deref(), sys_wav.as_deref(), || {
                progress.step(&app, Step::RemovingEcho)
            })
            .await;
            let mut diarize_steps = StreamSteps::new(
                &app,
                progress,
                Step::Diarizing,
                &[sys_wav.is_some(), mic_wav.is_some()],
            );
            let sys_speaker_hint = hybrid_sys_hint(expected_speakers, &chunks);
            let sys_segments = if let Some(p) = sys_wav.as_ref() {
                diarize_steps.begin();
                diarize_and_maybe_clean(&app, &note_id, p, sys_speaker_hint, &mut engine, thresholds)
                    .await
                    .unwrap_or_else(|e| {
                        eprintln!("rediarize: sys diarize failed ({e}), sys falls back to one label");
                        Vec::new()
                    })
            } else {
                Vec::new()
            };
            let mic_speaker_hint = mic_hint_after_sys(expected_speakers, &sys_segments);
            eprintln!(
                "rediarize hybrid: sys hint {sys_speaker_hint:?} → {} sys voice(s); mic hint {mic_speaker_hint:?}",
                distinct_speaker_count(&sys_segments)
            );
            let mic_segments = if let Some(p) = mic_wav.as_ref() {
                diarize_steps.begin();
                diarize_and_maybe_clean(&app, &note_id, echo.mic_input(p), mic_speaker_hint, &mut engine, thresholds)
                    .await
                    .unwrap_or_else(|e| {
                        eprintln!("rediarize: mic diarize failed ({e}), mic falls back to You");
                        Vec::new()
                    })
            } else {
                eprintln!("rediarize hybrid: no saved mic.wav, mic falls back to You");
                Vec::new()
            };
            echo.release_cancelled_mic();
            eprintln!(
                "rediarize hybrid: mic resolved to {} voice(s)",
                distinct_speaker_count(&mic_segments)
            );
            let (kept_mic, echo_report) = echo.check(&mic_segments);
            let mic_segments_for_dump = mic_segments;
            let mic_segments = kept_mic;
            let sys_segments_for_dump = sys_segments.clone();
            let labels = build_hybrid_labels(&chunks, &mic_segments, &sys_segments);
            let sys_fallback = format!("Speaker {}", labels.next_free);
            let HybridLabels { mic: mic_labels, sys: sys_labels, .. } = labels;
            let splitter: Box<Splitter> = Box::new(move |c: &ChunkRecord| match c.source {
                ChunkSource::Mic if mic_segments.is_empty() => {
                    single_piece(c, Some("You".to_string()))
                }
                ChunkSource::Mic => split_by_labels(c, &mic_segments, &mic_labels),
                ChunkSource::Sys if sys_segments.is_empty() => {
                    single_piece(c, Some(sys_fallback.clone()))
                }
                ChunkSource::Sys => split_by_labels(c, &sys_segments, &sys_labels),
            });
            DiarizeStage {
                splitter,
                mic_segments_for_dump,
                sys_segments_for_dump,
                source_tag: "hybrid",
                echo: echo_report,
                echo_ms: echo.ms,
            }
        }
        (false, false) => {
            progress.finished(&app);
            return Err(anyhow::anyhow!("no chunks recorded for either source"));
        }
        }
    };

    // Capture the labelled-piece sequence at both the pre- and post-
    // bridge stages so the diagnostic can show what the sandwich rule
    // did (or, more interestingly, didn't do) for this run.
    let pieces_pre = build_pieces_unbridged(&chunks, stage.splitter.as_ref());
    let mut pieces_post = pieces_pre.clone();
    bridge_short_interjections(&mut pieces_post);
    absorb_text_continuation_chains(&mut pieces_post);
    write_diagnostics_json(
        &app,
        &note_id,
        engine,
        stage.source_tag,
        &stage.mic_segments_for_dump,
        &stage.sys_segments_for_dump,
        &chunks,
        &thresholds,
        Some((&pieces_pre, &pieces_post)),
        stage.echo.as_ref(),
    )
    .await;

    let echo_ms = stage.echo_ms;
    let split_chunk = stage.splitter;
    let new_transcript = build_labelled_transcript(&chunks, split_chunk.as_ref());
    if new_transcript.trim().is_empty() {
        progress.finished(&app);
        return Err(anyhow::anyhow!("re-diarize produced empty transcript"));
    }

    // Offset this session's speaker numbers past any in the prior sessions,
    // so re-diarizing the latest take of a multi-session note doesn't collide
    // its "Speaker 1" with an earlier take's. For a single-session / legacy
    // note this is 0 and the behaviour is unchanged.
    let label_offset = prior_sessions_speaker_offset(&app, &note_id, &session_id);

    // Refresh the playback timeline so highlighting reflects the new labels.
    // The mixed playback.wav is rebuilt too — cheap, idempotent. Write it
    // FIRST, then rebuild the full DB transcript from every session's timeline
    // (so earlier takes are preserved), matching the per-chunk edit path.
    let timeline = serialize_timeline(&chunks, split_chunk.as_ref(), label_offset);
    progress.step(&app, Step::WritingPlayback);
    let t_playback = std::time::Instant::now();
    write_playback_assets(
        &app,
        &note_id,
        &session_id,
        timeline,
        mic_wav.as_deref(),
        sys_wav.as_deref(),
    )
    .await;
    let playback_ms = ms_since(t_playback);

    // Through the shared commit, so re-diarize inherits its refusal to trade a
    // non-empty transcript for an empty projection rather than restating it.
    let full_transcript = match rebuild_note_transcript(&app, &note_id) {
        Ok(t) => t,
        Err(e) => {
            progress.finished(&app);
            return Err(anyhow::anyhow!(e));
        }
    };
    if let Err(e) = commit_rebuilt_transcript(&app, &note_id, full_transcript) {
        progress.finished(&app);
        return Err(anyhow::anyhow!(e));
    }
    // Re-diarize rewrote this session's timeline.jsonl — re-push the metadata
    // (idempotent) so the record exists; the frontend re-uploads the timeline
    // asset after the command returns.
    session_changed_for_sync(&app, &note_id, &session_id);

    progress.finished(&app);
    Ok(TakeStepCosts { echo_ms, playback_ms })
}

/// Speaker-number offset for a session being (re)built in isolation: the
/// highest `Speaker N` across every *earlier* session's timeline. Mirrors
/// what `combine_with_snapshot` would have produced from the prior takes.
fn prior_sessions_speaker_offset(app: &AppHandle, note_id: &str, session_id: &str) -> u32 {
    let Ok(app_dir) = app.path().app_data_dir() else {
        return 0;
    };
    let recordings = sessions::recordings_dir(&app_dir, note_id);
    let mut acc = String::new();
    for (entry, dir) in sessions::resolve_sessions(&recordings) {
        if entry.id == session_id {
            break;
        }
        let values = read_timeline_values(&dir.join("timeline.jsonl"));
        let part = group_values_to_transcript(&values);
        if !part.trim().is_empty() {
            if !acc.is_empty() {
                acc.push('\n');
            }
            acc.push_str(&part);
        }
    }
    max_speaker_number(&acc)
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

/// How one note is diarized.
#[derive(Clone, Copy)]
struct NoteDiarizeSettings {
    /// Chosen from the selected engine, the note's speaker count and which
    /// models are downloaded (`diarize::choose_engine`), for the whole note.
    choice: diarize::EngineChoice,
    thresholds: diarize::Thresholds,
    expected_speakers: Option<i64>,
}

async fn note_diarize_settings(app: &AppHandle, note_id: &str) -> NoteDiarizeSettings {
    let (selected, thresholds, expected_speakers) = {
        let state: State<AppState> = app.state();
        let selected = selected_diarize_engine(&state);
        let thresholds = read_diarize_thresholds(&state);
        let conn = state.db.lock();
        let hint = db::get_note(&conn, note_id)
            .ok()
            .and_then(|n| n.expected_speakers)
            .filter(|n| *n > 0);
        (selected, thresholds, hint)
    };
    NoteDiarizeSettings {
        choice: diarize::choose_engine(app, selected, expected_speakers).await,
        thresholds,
        expected_speakers,
    }
}

/// Whether to run `diarize::clean_segments` over the raw sidecar output
/// before walking word-level alignment against it. Defaults to true.
/// Toggle off via `update settings set value='false' where key='diarize_clean_segments'`
/// to A/B against the raw output for the same recording.
fn should_clean_diarize_segments(app: &AppHandle) -> bool {
    let state: State<AppState> = app.state();
    let conn = state.db.lock();
    db::get_setting(&conn, "diarize_clean_segments")
        .ok()
        .flatten()
        .map(|s| s != "false")
        .unwrap_or(true)
}

/// Runs the diarize sidecar, then `diarize::clean_segments` over its output
/// unless `diarize_clean_segments` is off. A failed run is retried with the
/// other engine when that one is downloaded; `engine` is the pass's, and
/// becomes the fallback once the retry succeeds, so the pass's remaining
/// streams run on it too.
async fn diarize_and_maybe_clean(
    app: &AppHandle,
    note_id: &str,
    audio_path: &std::path::Path,
    num_speakers: Option<i64>,
    engine: &mut diarize::Engine,
    thresholds: diarize::Thresholds,
) -> anyhow::Result<Vec<diarize::Segment>> {
    let raw = match diarize::diarize_file(app, audio_path, num_speakers, *engine, thresholds).await {
        Ok(raw) => raw,
        Err(e) => {
            let failed = *engine;
            let Some(other) = diarize::retry_engine(app, failed).await else {
                return Err(e);
            };
            eprintln!("diarize: {} failed ({e}), retrying with {}", failed.arg(), other.arg());
            match diarize::diarize_file(app, audio_path, num_speakers, other, thresholds).await {
                Ok(raw) => {
                    diarize::demote(failed);
                    *engine = other;
                    emit_error(
                        app,
                        Some(note_id),
                        &diarize::fell_back_message(failed, other, &e.to_string()),
                    );
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Err(e) = diarize::recheck(&app, failed).await {
                            let message = diarize::recheck_failed_message(failed, &e.to_string());
                            emit_error(&app, None, &message);
                        }
                    });
                    raw
                }
                Err(retry) => {
                    // Both failing is the audio's doing, not an engine's.
                    eprintln!("diarize: {} failed too ({retry})", other.arg());
                    return Err(e);
                }
            }
        }
    };
    if !should_clean_diarize_segments(app) {
        return Ok(raw);
    }
    let raw_count = raw.len();
    let cleaned = diarize::clean_segments(raw);
    eprintln!(
        "diarize_clean: {raw_count} segments → {} after cleaning",
        cleaned.len()
    );
    Ok(cleaned)
}

/// Serialize a sequence of `LabelledPiece`s for the diagnostic dump.
/// Includes the speaker label, the joined text, a word count, the
/// acoustic span (last word's end - first word's start, chunk-relative),
/// and the individual word texts so a reader can correlate against the
/// `chunks` array without having to re-derive the alignment.
fn pieces_to_json(pieces: &[LabelledPiece]) -> Vec<serde_json::Value> {
    pieces
        .iter()
        .map(|p| {
            let words_text: Vec<&str> = p.words.iter().map(|w| w.text.as_str()).collect();
            let first = p.words.first().map(|w| w.start_ms).unwrap_or(0);
            let last = p.words.last().map(|w| w.end_ms).unwrap_or(0);
            let span_ms = last.saturating_sub(first);
            serde_json::json!({
                "label": p.label,
                "text": p.text,
                "word_count": p.text.split_whitespace().count(),
                "span_ms": span_ms,
                "words": words_text,
            })
        })
        .collect()
}

/// Write a JSON snapshot of one diarize run for inspection. Lands at
/// <app_data>/diagnostics/<note_id>/<engine>-<source>.json, overwritten
/// each time the same combination runs (e.g. switching engines or
/// re-running with different thresholds). Includes the segments, the
/// chunk timings they were aligned against, and the threshold values
/// used — enough for the user to eyeball where the engine placed
/// shifts and decide whether the threshold is too aggressive or too
/// loose. Best-effort: a write failure logs and proceeds, never breaks
/// the diarize pipeline.
///
/// `pieces` is the labelled-piece sequence the transcript emitter walks,
/// captured before and after `bridge_short_interjections` runs. When present
/// the dump also includes a `bridge_changes` diff so flicker-absorption
/// decisions (and non-decisions) are inspectable.
///
/// `mic_segments` are the mic's voices as diarized, echo included; `echo` says
/// which of them the check dropped before they were numbered.
async fn write_diagnostics_json(
    app: &AppHandle,
    note_id: &str,
    engine: diarize::Engine,
    source: &str,
    mic_segments: &[diarize::Segment],
    sys_segments: &[diarize::Segment],
    chunks: &[ChunkRecord],
    thresholds: &diarize::Thresholds,
    pieces: Option<(&[LabelledPiece], &[LabelledPiece])>,
    echo: Option<&crate::echo::EchoReport>,
) -> Option<PathBuf> {
    let Ok(app_dir) = app.path().app_data_dir() else {
        eprintln!("diagnostics: app_data_dir unavailable, skipping write");
        return None;
    };
    let dir = app_dir.join("diagnostics").join(note_id);
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        eprintln!("diagnostics: mkdir {}: {e}", dir.display());
        return None;
    }
    let engine_arg = engine.arg();
    let path = dir.join(format!("{engine_arg}-{source}.json"));

    let chunk_payload: Vec<_> = chunks
        .iter()
        .map(|c| {
            // Persist word timings (chunk-relative) so re-diarize can
            // do word-level speaker splitting later — without them the
            // re-diarize path falls back to one label per chunk and a
            // 15s chunk that contains a back-and-forth gets only one
            // speaker. Empty when the original transcribe provider
            // didn't return word data (current OpenAI API).
            //
            // `speaker_id` is the raw diarizer output for each word's
            // absolute-time midpoint — same lookup `split_by_segments`
            // uses. Surfacing it lets us audit whether word-level
            // assignment is actually happening, or whether the chunk
            // fell through to the no-words fallback path.
            //
            // Resolved against the word's *own* stream: a hybrid recording
            // diarizes mic and sys separately, and both segment lists are
            // stream-relative, so looking a mic word up in sys segments
            // would invent an id that never labelled anything.
            let segments = match c.source {
                ChunkSource::Mic => mic_segments,
                ChunkSource::Sys => sys_segments,
            };
            let words: Vec<_> = c
                .words
                .iter()
                .map(|w| {
                    let mid =
                        w.start_ms.saturating_add(w.end_ms.saturating_sub(w.start_ms) / 2);
                    let abs = c.start_ms.saturating_add(mid);
                    let speaker_id = assign_speaker(abs, segments).map(|s| s.to_string());
                    serde_json::json!({
                        "text": w.text,
                        "start_ms": w.start_ms,
                        "end_ms": w.end_ms,
                        "speaker_id": speaker_id,
                    })
                })
                .collect();
            serde_json::json!({
                "source": match c.source { ChunkSource::Mic => "mic", ChunkSource::Sys => "sys" },
                "start_ms": c.start_ms,
                "text": c.text,
                "words": words,
            })
        })
        .collect();

    // The post-split piece sequence is what `build_labelled_transcript`
    // emits as `Speaker N: ...` lines. Capturing both the pre- and post-
    // bridge versions, plus the diff, makes flicker-absorption decisions
    // (or non-decisions) inspectable: every entry in `bridge_changes`
    // is a piece the sandwich rule moved; every short A-B-A pattern that
    // ISN'T in `bridge_changes` is a piece the bridge declined to move,
    // and the reason is decidable from word count + duration + neighbours.
    let pieces_pre_json = pieces.map(|(pre, _)| pieces_to_json(pre));
    let pieces_post_json = pieces.map(|(_, post)| pieces_to_json(post));
    let bridge_changes = match pieces {
        Some((pre, post)) if pre.len() == post.len() => pre
            .iter()
            .zip(post.iter())
            .enumerate()
            .filter_map(|(i, (a, b))| {
                if a.label != b.label {
                    Some(serde_json::json!({
                        "piece_idx": i,
                        "from": a.label,
                        "to": b.label,
                    }))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };

    let payload = serde_json::json!({
        "engine": engine_arg,
        "source": source,
        "thresholds": {
            "community1_clustering": thresholds.community1_clustering,
        },
        "mic_segments": mic_segments,
        "sys_segments": sys_segments,
        "chunks": chunk_payload,
        "pieces_before_bridge": pieces_pre_json,
        "pieces_after_bridge": pieces_post_json,
        "bridge_changes": bridge_changes,
        "echo": echo,
        "created_at": chrono::Utc::now().timestamp_millis(),
    });

    let json = match serde_json::to_string_pretty(&payload) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("diagnostics: serialize: {e}");
            return None;
        }
    };
    if let Err(e) = tokio::fs::write(&path, json).await {
        eprintln!("diagnostics: write {}: {e}", path.display());
        return None;
    }
    Some(path)
}

/// One diarize sidecar invocation, with its wall clock recorded against the
/// stream it ran on (#182). A hybrid capture invokes the sidecar twice, and the
/// two costs are what makes the sequential pass legible.
async fn timed_diarize(
    app: &AppHandle,
    note_id: &str,
    wav: &std::path::Path,
    num_speakers: Option<i64>,
    engine: &mut diarize::Engine,
    thresholds: diarize::Thresholds,
    clock: Option<&StopClock>,
    source: ChunkSource,
) -> anyhow::Result<Vec<diarize::Segment>> {
    let t = std::time::Instant::now();
    let out = diarize_and_maybe_clean(app, note_id, wav, num_speakers, engine, thresholds).await;
    record_timing(clock, |x| {
        let ms = ms_since(t);
        match source {
            ChunkSource::Mic => x.diarize_mic_ms = Some(ms),
            ChunkSource::Sys => x.diarize_sys_ms = Some(ms),
        }
    });
    out
}

/// One take's echo pass as a chain runs it: named as its own step and
/// timed, over the take's two full streams, before its mic is diarized. Holds
/// nothing for a take that lacks either stream — there is no reference to take
/// the echo out with.
struct EchoPass {
    analysis: Option<crate::echo::TakeAnalysis>,
    /// The mic with the echo cancelled out, for the mic diarize to read.
    cancelled: Option<CancelledMicWav>,
    /// What the step cost, when it ran.
    ms: Option<u64>,
}

impl EchoPass {
    /// `announce` names the step, and runs only when the pass does.
    async fn run(
        mic: Option<&std::path::Path>,
        sys: Option<&std::path::Path>,
        announce: impl FnOnce(),
    ) -> Self {
        let (Some(mic), Some(sys)) = (mic, sys) else {
            return Self { analysis: None, cancelled: None, ms: None };
        };
        announce();
        let t = std::time::Instant::now();
        let mut analysis = analyze_take_echo(mic, sys).await;
        let cancelled = match analysis.as_mut().and_then(crate::echo::TakeAnalysis::take_cleaned) {
            Some(cleaned) => match CancelledMicWav::write(&cleaned).await {
                Ok(wav) => Some(wav),
                Err(e) => {
                    eprintln!("echo: can't write the cancelled mic ({e}); diarizing the mic as captured");
                    None
                }
            },
            None => None,
        };
        Self { analysis, cancelled, ms: Some(ms_since(t)) }
    }

    /// What the mic diarize reads: the cancelled mic, when there is one.
    fn mic_input<'a>(&'a self, captured: &'a std::path::Path) -> &'a std::path::Path {
        self.cancelled.as_ref().map_or(captured, |wav| wav.path())
    }

    /// The mic diarize has read the cancelled mic, so nothing keeps it.
    fn release_cancelled_mic(&mut self) {
        self.cancelled = None;
    }

    /// The mic's voices without the ones that read as echo, and what the
    /// diarize dump records of the pass.
    fn check(
        &self,
        mic_segments: &[diarize::Segment],
    ) -> (Vec<diarize::Segment>, Option<crate::echo::EchoReport>) {
        let Some(analysis) = &self.analysis else {
            return (mic_segments.to_vec(), None);
        };
        let (kept, report) = analysis.check(mic_segments.to_vec());
        if let Some(check) = &report.check {
            log_echo_check(check);
        }
        (kept, Some(report))
    }
}

/// A cancelled mic written for the diarize sidecar to read. It lives in the
/// system temp dir and goes when dropped: a WAV under `recordings/` or
/// `diagnostics/` would sit outside `keep_audio` and "Delete stored audio".
struct CancelledMicWav(PathBuf);

impl CancelledMicWav {
    async fn write(samples: &[i16]) -> anyhow::Result<Self> {
        // Held before the write, so a half-written file is removed too.
        let wav = Self(std::env::temp_dir().join(format!("humla-echo-{}.wav", uuid::Uuid::new_v4())));
        wav::write_i16_mono_16k(&wav.0, samples).await?;
        Ok(wav)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for CancelledMicWav {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Reads a take's two full streams and analyses the echo between them, off the
/// async runtime. `None` when either can't be read: the mic is then diarized as
/// it was captured.
async fn analyze_take_echo(
    mic: &std::path::Path,
    sys: &std::path::Path,
) -> Option<crate::echo::TakeAnalysis> {
    let (mic_samples, sys_samples) =
        match tokio::try_join!(wav::read_i16_mono_16k(mic), wav::read_i16_mono_16k(sys)) {
            Ok(both) => both,
            Err(e) => {
                eprintln!("echo: can't read the take's streams ({e}); diarizing the mic as captured");
                return None;
            }
        };
    let analysis = match tokio::task::spawn_blocking(move || {
        crate::echo::analyze_take(mic_samples, sys_samples)
    })
    .await
    {
        Ok(a) => a,
        Err(e) => {
            eprintln!("echo: analysis failed ({e}); diarizing the mic as captured");
            return None;
        }
    };
    let lag = &analysis.lag;
    match (lag.median_ms, analysis.echo.is_some()) {
        (Some(ms), true) => eprintln!(
            "echo: the mic hears sys {ms:+.1} ms late in {} of {} windows, {} step(s)",
            lag.valid,
            lag.windows,
            lag.steps.len()
        ),
        _ => eprintln!("echo: none in {} windows; the mic is left as it is", lag.windows),
    }
    Some(analysis)
}

fn log_echo_check(check: &crate::echo::VoiceCheck) {
    let voices: Vec<String> = check
        .voices
        .iter()
        .map(|v| {
            format!(
                "{} {:.0}s {}{}",
                v.speaker_id,
                v.seconds,
                v.ratio.map(|r| format!("{r:.2}")).unwrap_or_else(|| "-".into()),
                if v.dropped { " dropped" } else { "" }
            )
        })
        .collect();
    let note = if check.skipped {
        " — too little sys silence to judge"
    } else if check.kept_all {
        " — every voice read as echo, so all were kept"
    } else {
        ""
    };
    eprintln!(
        "echo: sys silent {:.1}s; {}{note}",
        check.sys_silent_s,
        voices.join(" · ")
    );
}

/// [`write_diagnostics_json`] for a stop chain: the file it wrote is remembered
/// on the clock, so the chain's timings are merged into that same file instead
/// of a sibling.
async fn write_diagnostics_timed(
    app: &AppHandle,
    note_id: &str,
    engine: diarize::Engine,
    source: &str,
    mic_segments: &[diarize::Segment],
    sys_segments: &[diarize::Segment],
    chunks: &[ChunkRecord],
    thresholds: &diarize::Thresholds,
    clock: Option<&StopClock>,
    echo: Option<&crate::echo::EchoReport>,
) {
    let path = write_diagnostics_json(
        app, note_id, engine, source, mic_segments, sys_segments, chunks, thresholds, None, echo,
    )
    .await;
    if let Some(path) = path {
        record_timing(clock, |x| x.diagnostics_path = Some(path));
    }
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

/// Read the user-tunable diarizer thresholds from settings. A missing value
/// falls back to its DEFAULT_* constant, the number the settings UI shows. An
/// unparseable one drops to None, the sidecar's default, rather than silently
/// becoming ours.
fn read_diarize_thresholds(state: &State<AppState>) -> diarize::Thresholds {
    let conn = state.db.lock();
    let community1_clustering = db::get_setting(&conn, "community1_threshold")
        .ok()
        .flatten()
        .or_else(|| Some(DEFAULT_COMMUNITY1_THRESHOLD.to_string()))
        .and_then(|s| s.parse::<f64>().ok());
    diarize::Thresholds { community1_clustering }
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

/// A replay run's timings, at `diagnostics/<note_id>/replay-timings.json`.
///
/// Its own file rather than a merge into the take's diarize diagnostics, which
/// [`write_stop_timings`] does: that file is keyed `<engine>-<source>`, so every
/// take of a multi-take run would write the same one and only the last would
/// survive. Best-effort — a write failure logs and nothing else.
async fn write_replay_timings(app: &AppHandle, note_id: &str, timings: &ReplayTimings) {
    let Ok(app_dir) = app.path().app_data_dir() else { return };
    let dir = app_dir.join("diagnostics").join(note_id);
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        eprintln!("replay timings: mkdir {}: {e}", dir.display());
        return;
    }
    let path = dir.join("replay-timings.json");
    let existing = tokio::fs::read_to_string(&path)
        .await
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
    let merged = crate::recording::merge_timings(existing, timings);
    match serde_json::to_string_pretty(&merged) {
        Ok(json) => {
            if let Err(e) = tokio::fs::write(&path, json).await {
                eprintln!("replay timings: write {}: {e}", path.display());
            }
        }
        Err(e) => eprintln!("replay timings: serialize: {e}"),
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

// ---------------------------------------------------------------------------
// Deferred transcription (#146)
//
// "Transcribe manually" trades live transcription for a quiet Mac: the meeting
// is captured and its per-source streams retained, and nothing reaches a
// provider until the user presses Transcribe. That press replays each pending
// take's retained audio through the *same* decode → VAD chunk → transcribe →
// diarize pipeline a file import uses, one take at a time, oldest first.
//
// Three things make it not-a-recording, and each is load-bearing:
//   * It never touches the single live-capture slot, so a replay of an hour of
//     audio doesn't stop the user recording their next meeting.
//   * It reports through its own per-note `transcribe_status` channel rather
//     than `recording_status`, so it can't blank the bar of a recording running
//     on another note.
//   * Its chunks never stream into `note.transcript`. The text arrives once,
//     from the timeline rebuild — so a replay that dies half-way leaves no text
//     behind that no timeline accounts for (ADR-0004).
// ---------------------------------------------------------------------------

/// Run transcription over `note_id`'s takes, oldest first (#146). Mirrors
/// `summarize_note`'s shape: a per-note status event brackets the work, and the
/// note it belongs to is the only surface that changes.
///
/// `scope` picks which takes, and the two surfaces that call it are different
/// actions rather than one with a flag: the toolbar's **Transcribe** finishes
/// what a "Transcribe manually" capture left pending, while the Transcript
/// panel's **Re-transcribe** replaces text the note already has — for a
/// recording that came back off the wrong language or the wrong model.
#[tauri::command]
pub async fn transcribe_note(
    app: AppHandle,
    state: State<'_, AppState>,
    note_id: String,
    scope: sessions::TranscribeScope,
) -> Result<(), String> {
    // Refuse only for *this* note: its post-stop chain is about to rewrite the
    // same transcript from the same timelines, and two writers would race for
    // it. A recording on a different note is explicitly fine — not blocking
    // that is the whole reason this path avoids the capture slot.
    {
        let s = state.recording.lock();
        if s.note_id.as_deref() == Some(&note_id) {
            return Err("This note is recording — stop the recording first.".into());
        }
    }
    if !state.transcribing.lock().insert(note_id.clone()) {
        return Err("Already transcribing this note.".into());
    }
    emit_transcribe_status(&app, &note_id, true);
    let result = transcribe_takes(&app, &note_id, scope).await;
    notes::purge_note_assets_if_gone(&app, &note_id);
    state.transcribing.lock().remove(&note_id);
    emit_transcribe_status(&app, &note_id, false);
    // No `emit_error` here: the command's rejection is what the caller toasts,
    // and emitting as well would show the same message twice.
    result
}

/// Per-note deferred-transcription lifecycle, on its own channel for the same
/// reason `summary_status` has one: a replay on note A must not touch note B's
/// state, and neither may reach `recording_status`.
fn emit_transcribe_status(app: &AppHandle, note_id: &str, active: bool) {
    let _ = app.emit(
        "transcribe_status",
        crate::recording::TranscribeStatusPayload::bracket(note_id, active),
    );
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

async fn transcribe_takes(
    app: &AppHandle,
    note_id: &str,
    scope: sessions::TranscribeScope,
) -> Result<(), String> {
    let app_dir = app.path().app_data_dir().map_err(err)?;
    let recordings = sessions::recordings_dir(&app_dir, note_id);
    let takes = sessions::takes_to_transcribe(&recordings, scope);
    if takes.is_empty() {
        // Nothing to do — a stale button, or a second press that lost the race.
        // Not an error: there is no bad state to report.
        return Ok(());
    }

    let state: State<AppState> = app.state();
    // Re-transcribing replaces text the note already has, and the rewrite path
    // (`db::set_transcript`) keeps no history of its own — so a run that comes
    // back worse than what it replaced, or that lands in the wrong language a
    // second time, would have nothing to go back to. One snapshot before the
    // first take makes the whole run undoable through the note's existing
    // revision list. `Pending` needs none: it only adds takes that had no text.
    if scope == sessions::TranscribeScope::All {
        let conn = state.db.lock();
        // Loudly best-effort: this call IS the undo for a destructive run, so a
        // silent failure would mean the user's corrected turns are replaced
        // with nothing to go back to. Not fatal — refusing to transcribe
        // because the history table wouldn't take a row would be worse — but
        // never invisible.
        if let Err(e) = db::snapshot_revision(&conn, note_id) {
            eprintln!("transcribe: could not snapshot a revision for {note_id} before a re-transcribe; the run will not be undoable: {e}");
        }
    }
    // The provider that runs now is the one configured now, which may not be
    // the one that was configured when the audio was captured. Check before
    // replaying an hour of it.
    ensure_provider_ready(app, &state, note_id).await?;

    // The run's shape, settled before a single chunk decodes: each take's
    // retained streams are replayed one after the other, so a take that kept
    // both costs twice its duration. Every stream of every take reports into
    // this one handle, since the fraction is over the whole run.
    let progress = Arc::new(parking_lot::Mutex::new(ReplayProgress::new(
        takes
            .iter()
            .map(|(entry, dir)| TakeWork {
                streams: sessions::replayable_stream_count(dir),
                duration_ms: entry.duration_ms,
            })
            .collect(),
    )));
    emit_transcribe_progress(app, note_id, progress.lock().snapshot());
    // On its own per-note channel, never the global phase: a recording may be
    // running on another note throughout. The shared diarize and unify passes
    // take this rather than choosing.
    let chain = ChainProgress::Replay {
        note_id: note_id.to_string(),
        progress: progress.clone(),
    };

    let mut transcribed_chunks: Vec<ChunkRecord> = Vec::new();
    let mut failures: Vec<String> = Vec::new();
    // Set when a take fails hard enough to abandon the rest of the run. Held
    // rather than returned so the tail below still runs for the takes that did
    // land — see the loop.
    let mut fatal: Option<String> = None;
    // What the run cost, per take and per half. Filled as the loop runs and
    // reported once below, so a take abandoned mid-run is still measured.
    let run_started = std::time::Instant::now();
    let mut timings = ReplayTimings::default();
    for (take_index, (entry, dir)) in takes.into_iter().enumerate() {
        // Emitted, not just recorded: this is where the step goes back to
        // transcribing after the previous take's diarize.
        let snapshot = {
            let mut p = progress.lock();
            p.begin_take(take_index);
            p.snapshot()
        };
        emit_transcribe_progress(app, note_id, snapshot);
        let t_transcribe = std::time::Instant::now();
        // At least one of these exists — `takes_to_transcribe` filtered on it,
        // so a take whose audio was swept away by "Delete stored audio" never
        // reaches here and never produced a button either.
        let mic = dir.join("mic.wav");
        let sys = dir.join("sys.wav");
        let mic_wav = mic.exists().then_some(mic);
        let sys_wav = sys.exists().then_some(sys);

        // One sink for the whole take: its two streams share a chunk log (the
        // diarize pass has to see both to tell an in-person meeting from a
        // call) and keep their own rolling context. Replays run one after the
        // other — inference serialises on `transcribe_gate` regardless, and
        // sequential keeps the machine's load where the user put it.
        let sink = CaptureSink::new(SinkMode::Replay {
            source: ChunkSource::Mic,
        })
        .with_progress(progress.clone());
        if let Some(path) = mic_wav.as_deref() {
            let stream = Arc::new(sink.for_stream(ChunkSource::Mic));
            replay_retained_stream(app, note_id, path, &stream).await?;
            progress.lock().finish_stream();
        }
        if let Some(path) = sys_wav.as_deref() {
            let stream = Arc::new(sink.for_stream(ChunkSource::Sys));
            replay_retained_stream(app, note_id, path, &stream).await?;
            progress.lock().finish_stream();
        }
        emit_transcribe_progress(app, note_id, progress.lock().snapshot());
        let mut take_timings = ReplayTakeTimings {
            index: entry.index,
            streams: mic_wav.is_some() as u32 + sys_wav.is_some() as u32,
            audio_ms: entry.duration_ms,
            transcribe_ms: ms_since(t_transcribe),
            diarize_ms: 0,
            echo_ms: None,
            playback_ms: 0,
        };
        let chunks = sink.chunk_log.lock().clone();
        if chunks.is_empty() {
            timings.takes.push(take_timings);
            // Deliberately NOT marked transcribed. From here, silence and "every
            // chunk failed" (provider outage, revoked key) are indistinguishable,
            // and the second has to stay retryable — the audio is still on disk,
            // so the action stays available for this take.
            failures.push(format!(
                "Recording {} produced no text. Its audio is still saved — try again.",
                entry.index
            ));
            continue;
        }
        // chunks.json beside the audio, so a later re-diarize has its anchor.
        if let Some(target) = session_write_dir(app, note_id, &entry.id).await {
            write_chunks_json(&target, &chunks).await;
        }
        // Through the re-diarize path, not the post-stop one: it writes this
        // take's timeline with a speaker-number offset taken from the takes
        // *before* it and then rebuilds `note.transcript` from every session's
        // timeline. That places a newly transcribed take at its own position in
        // the note even when a later take was already transcribed — which
        // `combine_with_snapshot`, appending to the end, could not.
        // A take that fails here stops the RUN, but must not discard what the
        // takes before it already committed: each has written its
        // `timeline.jsonl` and rebuilt the note's transcript from it, so the
        // checkpoints and the asset push below still have to happen. Recorded
        // and broken out of rather than `?`-ed straight to the caller.
        let t_diarize = std::time::Instant::now();
        // Chosen per take, so a take after one that demoted an engine skips it.
        let settings = note_diarize_settings(app, note_id).await;
        let diarized = rediarize_apply_to_chunks(
            app.clone(),
            note_id.to_string(),
            entry.id.clone(),
            mic_wav.clone(),
            sys_wav.clone(),
            chunks.clone(),
            settings,
            LabelFallback::Unlabelled,
            &chain,
        )
        .await;
        take_timings.diarize_ms = ms_since(t_diarize);
        if let Ok(costs) = &diarized {
            take_timings.echo_ms = costs.echo_ms;
            take_timings.playback_ms = costs.playback_ms;
        }
        timings.takes.push(take_timings);
        if let Err(e) = diarized {
            fatal = Some(format!("Recording {} failed to transcribe: {e}", entry.index));
            break;
        }
        record_transcribed_take(app, note_id, &entry.id).await;
        session_changed_for_sync(app, note_id, &entry.id);
        transcribed_chunks.extend(chunks);
    }

    if transcribed_chunks.is_empty() {
        // Nothing landed, so there is nothing to checkpoint or push. With no
        // failures either, every take had lost its audio — nothing was
        // attempted, so there is nothing to report. Measured on the way out
        // all the same.
        timings.total_ms = ms_since(run_started);
        eprintln!("{}", timings.summary());
        write_replay_timings(app, note_id, &timings).await;
        failures.extend(fatal);
        return if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join(" "))
        };
    }
    // A partial run: some takes landed, some didn't. Those are notices rather
    // than the command's outcome, since the note did gain text.
    for msg in &failures {
        emit_error(app, Some(note_id), msg);
    }

    // Content-settled checkpoints the live path runs post-stop, in the same
    // order and for the same reasons: resolve `auto` to what was actually
    // spoken (#167) before anything reads the note's language, refresh
    // retrieval chunks now that the note has searchable text (#47), embed them
    // off the request path (#48), and give a still-unnamed note a title (#90).
    record_detected_language(app, note_id, &transcribed_chunks);
    {
        let conn = state.db.lock();
        chat::reindex_note_content(&conn, note_id);
    }
    tauri::async_runtime::spawn(chat::embed_note_bg(app.clone(), note_id.to_string()));
    tauri::async_runtime::spawn(title::generate_note_title(
        app.clone(),
        note_id.to_string(),
    ));
    // Cross-session speaker unification (#17), exactly as the post-stop chain
    // runs it — otherwise a note's takes would carry one label per voice or
    // per take depending on which regime recorded them, which is not a
    // difference the setting is supposed to make. No-op for single-take notes;
    // a failure keeps the per-take labels.
    let t_unify = std::time::Instant::now();
    match unify_note_speakers(app, note_id, &chain).await {
        Ok(Some(costs)) => {
            eprintln!("unify: cross-session speaker unification applied");
            timings.unify_echo_ms = costs.echo_ms;
        }
        Ok(None) => {}
        Err(e) => eprintln!("unify: failed, keeping per-take labels: {e}"),
    }
    timings.unify_ms = ms_since(t_unify);

    // The run's own line, once however it ends.
    timings.total_ms = ms_since(run_started);
    eprintln!("{}", timings.summary());
    write_replay_timings(app, note_id, &timings).await;

    // Push the rewritten `timeline.jsonl` of every take this run touched.
    //
    // `session_changed_for_sync` above only enqueues the *metadata* record;
    // the binary assets are a separate channel (see the per-session asset sync
    // comment in `commands::cloud`), and for a finished recording the frontend
    // fires it off the `recording_status` idle. This path deliberately never
    // emits one — so without this, a teammate gets the note's text and never
    // its speaker labels or word timings until repair-on-open happens to catch
    // it. From the backend rather than the view, so no other caller of
    // `transcribe_note` can skip it.
    //
    // Spawned: it waits for the sync worker to have pushed the records it
    // needs, which can take longer than the user should watch a spinner for.
    // A no-op for Personal notes.
    #[cfg(feature = "cloud")]
    {
        let app = app.clone();
        let note_id = note_id.to_string();
        tauri::async_runtime::spawn(async move {
            if let Err(e) = cloud::upload_note_sessions(&app, &note_id, false).await {
                eprintln!("transcribe: session asset upload for {note_id}: {e}");
            }
        });
    }
    // Only now, with everything the successful takes earned committed and
    // pushed, does a mid-run failure become the run's outcome.
    if let Some(msg) = fatal {
        return Err(msg);
    }
    Ok(())
}

/// Record one take as transcribed. Best-effort with a loud log.
///
/// Deliberately does NOT touch `duration_ms`, though this is the moment the
/// take first has a timeline to measure. The sync engine treats a session's
/// scalar metadata as immutable and derives its last-write-wins key from
/// `started_at` on exactly that basis (`cloud-sync`'s `push_session`: "index /
/// started_at / duration / streams never change"), so a duration corrected
/// here would re-push under a byte-identical key and converge only because the
/// server happens to compare strictly. A deferred take's length comes from the
/// sidecar at capture time instead — see `finalize_session`.
///
/// The text is already committed by the time this runs, so a failure leaves the
/// note offering a Transcribe button for a take that already has its text.
/// Pressing it replays the take and rewrites the same `timeline.jsonl` — the
/// transcript is rebuilt from the timelines rather than appended to, so the
/// words don't double — but it spends the whole transcription again for no
/// change. Hence the log: worth noticing.
async fn record_transcribed_take(app: &AppHandle, note_id: &str, session_id: &str) {
    let Ok(app_dir) = app.path().app_data_dir() else {
        return;
    };
    let recordings = sessions::recordings_dir(&app_dir, note_id);
    let session_id = session_id.to_string();
    // Same lock every other manifest read-modify-write takes, so a cloud pull
    // reconciling mid-replay can't drop what we're about to write.
    let lock = app.state::<AppState>().manifest_lock.clone();
    let _manifest_guard = lock.lock().await;
    let res = tokio::task::spawn_blocking(move || {
        sessions::record_transcribed_take(&recordings, &session_id)
    })
    .await;
    match res.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.to_string())) {
        Ok(true) => {}
        Ok(false) => eprintln!("transcribe: no manifest entry to record for {note_id}"),
        Err(e) => eprintln!("transcribe: record transcribed take {note_id}: {e}"),
    }
}

/// Replay one retained source WAV through the sidecar's `--import` mode,
/// transcribing every VAD chunk it emits into `sink`. Returns once the sidecar
/// has closed its pipe and every spawned transcribe has finished, so the
/// caller's chunk log is complete.
///
/// One stream per call. The sidecar's `--import` mode writes through its mic
/// writers whatever it is fed, so which stream a chunk belongs to is the
/// caller's knowledge — carried by `SinkMode::Replay { source }`, not read off
/// the event.
async fn replay_retained_stream(
    app: &AppHandle,
    note_id: &str,
    wav: &std::path::Path,
    sink: &Arc<CaptureSink>,
) -> Result<(), String> {
    let temp_dir = std::env::temp_dir().join(format!(
        "humla-replay-{}-{}",
        note_id,
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&temp_dir).map_err(err)?;
    let sidecar = sidecar_path(app)?;
    let mut cmd = Command::new(&sidecar);
    cmd.arg("--import").arg(wav);
    cmd.arg("--out").arg(&temp_dir);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.kill_on_drop(true);
    // No `setsid` detach here, unlike the live path: that exists so macOS TCC
    // attributes microphone / screen capture to the sidecar's own binary, and a
    // replay opens neither — it reads a WAV we already own.
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("spawn audio-capture (replay): {e}"))?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let stderr = child.stderr.take().ok_or("no stderr")?;
    {
        let app_err = app.clone();
        let note_id_err = note_id.to_string();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                eprintln!("audio-capture(replay) stderr: {trimmed}");
                if let Some(msg) = trimmed.strip_prefix("humla-error: ") {
                    let _ = app_err.emit("recording_error", ErrorPayload {
                        note_id: Some(note_id_err.clone()),
                        message: format!("audio-capture: {msg}"),
                    });
                }
            }
        });
    }

    let inflight: Inflight = Arc::new(parking_lot::Mutex::new(Vec::new()));
    // Same bound as an import: a replay emits chunks at disk speed, so without
    // it the reader would park hundreds of tasks behind `transcribe_gate`.
    let backlog = Arc::new(Semaphore::new(IMPORT_BACKLOG_PERMITS));
    let mut reader = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = reader.next_line().await {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<SidecarEvent>(trimmed) {
            Ok(ev) => {
                if dispatch_sidecar_event(app, note_id, ev, &inflight, Some(&backlog), sink).await {
                    break;
                }
            }
            Err(e) => eprintln!("bad sidecar line: {e} -- {}", redact_sidecar_line(&line)),
        }
    }
    drain_inflight(&inflight).await;
    let _ = child.wait().await;
    // The replay wrote its own chunk WAVs plus a duplicate of the stream it was
    // fed; none of it outlives this call (the diarize pass reads the *retained*
    // WAV, not this copy).
    let _ = tokio::fs::remove_dir_all(&temp_dir).await;
    Ok(())
}

/// Run offline speaker diarization on the just-finished recording and
/// rewrite the transcript with proper labels. Branches on which streams
/// produced content:
///
/// - **Mic only** (in-person meeting, no system audio): diarize the mic
///   full WAV, label chunks `Speaker N:` in first-encounter order. This is
///   the original single-stream path; multiple humans sharing the same mic
///   get separated by community-1's clustering.
/// - **System only** (very rare; mic permission denied or some platform
///   weirdness): diarize the system full WAV, same `Speaker N:` labelling.
/// - **Both present** (remote/hybrid call): diarize the system stream for
///   remote-side speakers, label every mic chunk as `You:` (the user is
///   the only person on the mic side, by definition of channel
///   attribution). Skips diarizing the mic stream entirely — there's no
///   point classifying a stream where every chunk is the same person.
///
/// Resumed recordings prepend the snapshotted prior transcript and offset
/// this session's `Speaker N:` numbers past any existing ones so resumed
/// halves don't collide IDs (`You:` is a fixed label and isn't offset).
/// No-ops gracefully when the diarize model isn't downloaded, when no
/// chunks were captured, or when both streams produced nothing.
async fn diarize_and_apply(
    app: AppHandle,
    note_id: String,
    post_stop: PostStopSnapshot,
    // The stop chain's clock (#182), so each diarize sidecar invocation is
    // attributed to the stream it ran on. `None` for a caller that isn't
    // measuring a stop.
    clock: Option<&StopClock>,
    // Where this pass reports. Passed in for the same reason the clock is: the
    // pass is shared, and the channel is the caller's.
    progress: &ChainProgress,
) -> anyhow::Result<()> {
    // Per-session state arrives via `post_stop`, captured in
    // `recording_stop` once the reader thread finished writing to the
    // session. Reading from `state.recording` here would race with a new
    // recording — the user can hit ⌘R again before this task lands.
    // The DB-derived bits (engine, thresholds, expected_speakers hint)
    // still come from the live state because they're per-note settings,
    // not per-session.
    let mic_wav = post_stop.mic_wav.clone();
    let sys_wav = post_stop.sys_wav.clone();
    // Defensive: `transcribe_chunk` already drops these, so this normally
    // changes nothing. It matters for chunks that predate that guard, and it
    // has to happen before the capture-mode decision below.
    let chunks = drop_incidental_stream_hallucinations(post_stop.chunks.clone());
    let snapshot = post_stop.transcript_at_start.clone();
    let session_id = post_stop.session_id.clone();
    if chunks.is_empty() {
        eprintln!("diarize: no chunks captured, skipping");
        return Ok(());
    }
    let NoteDiarizeSettings { choice, thresholds, expected_speakers } =
        note_diarize_settings(&app, &note_id).await;
    // Under ADR-0004 a recording that lands text always writes a session, so
    // with no model downloaded the timeline is written without speaker labels,
    // and the user is told why they never arrive.
    let diarize_available = choice.downloaded;
    let mut engine = choice.engine;
    if !diarize_available {
        eprintln!("diarize: model not downloaded, saving the timeline without labels");
        emit_error(&app, Some(&note_id), DIARIZE_MODEL_MISSING_UNLABELLED);
    }

    let mic_chunks_present = chunks.iter().any(|c| c.source == ChunkSource::Mic);
    let sys_chunks_present = chunks.iter().any(|c| c.source == ChunkSource::Sys);

    if diarize_available {
        progress.started(&app);
    }

    // Decide which WAV to diarize and how to label chunks. The label
    // assignment is a per-chunk closure so the merge step doesn't need to
    // know which mode we're in.
    // The labeller closure crosses a `.await` (the cleanup_full_wav calls
    // below) inside a spawned task, so it has to be `Send`. The captured
    // segments + display map are both Send, so the bound just needs to be
    // declared on the trait object.
    type Splitter = dyn Fn(&ChunkRecord) -> Vec<LabelledPiece> + Send;
    let split_chunk: Box<Splitter> = if !diarize_available {
        // No model, so no speaker assignment to make — but the word timings
        // are kept. Diarization decides *who* spoke, not when each word
        // landed, so its absence says nothing about the timings the provider
        // returned for this audio. (ADR-0004 drops `words` when a turn's text
        // is *edited*, where the mapping describes words that are gone. That
        // isn't this case, and a timeline with real timings gives these notes
        // per-word highlighting plus the tighter turn bounds
        // `serialize_timeline` derives from words when it has them.)
        Box::new(|c: &ChunkRecord| single_piece(c, None))
    } else {
        match (mic_chunks_present, sys_chunks_present) {
        (true, false) => {
            // In-person mode: diarize the mic stream, every chunk gets a
            // numbered label from its segment. The per-note expected
            // speaker hint applies directly — every speaker is on the mic.
            //
            // Three failure modes drop us into a single-speaker fallback
            // (every mic chunk gets `Speaker 1:`) instead of returning
            // early with no labels:
            //   1. mic_full.wav missing — sidecar SIGKILL'd before close.
            //   2. diarize sidecar errored.
            //   3. diarize returned zero segments.
            // Applying the fallback rather than bailing keeps the post-stop
            // behaviour consistent with the hybrid branch and gives the user
            // visible evidence that diarize ran.
            let single_speaker_fallback = || -> Box<Splitter> {
                Box::new(|c: &ChunkRecord| single_piece(c, Some("Speaker 1".to_string())))
            };
            let mut diarize_steps =
                StreamSteps::new(&app, progress, Step::Diarizing, &[mic_wav.is_some()]);
            match mic_wav.clone() {
                None => {
                    eprintln!("diarize: mic chunks present but mic_full.wav missing, falling back to single-speaker labels");
                    emit_error(
                        &app,
                        Some(&note_id),
                        "Diarization unavailable: the recording sidecar didn't write the full audio file. All speech grouped under Speaker 1.",
                    );
                    single_speaker_fallback()
                }
                Some(wav) => {
                    diarize_steps.begin();
                    match timed_diarize(&app, &note_id, &wav, expected_speakers, &mut engine, thresholds, clock, ChunkSource::Mic).await {
                        Err(e) => {
                            eprintln!("diarize: mic diarize failed ({e}), falling back to single-speaker labels");
                            emit_error(
                                &app,
                                Some(&note_id),
                                &format!("Diarization failed ({e}); all speech grouped under Speaker 1."),
                            );
                            single_speaker_fallback()
                        }
                        Ok(segments) if segments.is_empty() => {
                            eprintln!("diarize: no segments returned for mic stream, falling back to single-speaker labels");
                            emit_error(
                                &app,
                                Some(&note_id),
                                "Diarization found no distinct speakers; all speech grouped under Speaker 1.",
                            );
                            single_speaker_fallback()
                        }
                        Ok(segments) => {
                            write_diagnostics_timed(&app, &note_id, engine, "mic", &segments, &[], &chunks, &thresholds, clock, None).await;
                            let display_map = build_display_map(&chunks, &segments, ChunkSource::Mic);
                            Box::new(move |c: &ChunkRecord| split_by_segments(c, &segments, &display_map))
                        }
                    }
                }
            }
        }
        (false, true) => {
            // Edge case: system-only recording. Same shape as mic-only;
            // same three failure modes drop to the single-speaker fallback.
            let single_speaker_fallback = || -> Box<Splitter> {
                Box::new(|c: &ChunkRecord| single_piece(c, Some("Speaker 1".to_string())))
            };
            let mut diarize_steps =
                StreamSteps::new(&app, progress, Step::Diarizing, &[sys_wav.is_some()]);
            match sys_wav.clone() {
                None => {
                    eprintln!("diarize: sys chunks present but sys_full.wav missing, falling back to single-speaker labels");
                    emit_error(
                        &app,
                        Some(&note_id),
                        "Diarization unavailable: the recording sidecar didn't write the full audio file. All speech grouped under Speaker 1.",
                    );
                    single_speaker_fallback()
                }
                Some(wav) => {
                    diarize_steps.begin();
                    match timed_diarize(&app, &note_id, &wav, expected_speakers, &mut engine, thresholds, clock, ChunkSource::Sys).await {
                        Err(e) => {
                            eprintln!("diarize: sys diarize failed ({e}), falling back to single-speaker labels");
                            emit_error(
                                &app,
                                Some(&note_id),
                                &format!("Diarization failed ({e}); all speech grouped under Speaker 1."),
                            );
                            single_speaker_fallback()
                        }
                        Ok(segments) if segments.is_empty() => {
                            eprintln!("diarize: no segments returned for sys stream, falling back to single-speaker labels");
                            emit_error(
                                &app,
                                Some(&note_id),
                                "Diarization found no distinct speakers; all speech grouped under Speaker 1.",
                            );
                            single_speaker_fallback()
                        }
                        Ok(segments) => {
                            write_diagnostics_timed(&app, &note_id, engine, "sys", &[], &segments, &chunks, &thresholds, clock, None).await;
                            let display_map = build_display_map(&chunks, &segments, ChunkSource::Sys);
                            Box::new(move |c: &ChunkRecord| split_by_segments(c, &segments, &display_map))
                        }
                    }
                }
            }
        }
        (true, true) => {
            // Both streams carried speech. Diarize *both* — see
            // `build_hybrid_labels` for why the mic can't be assumed to hold
            // a single person just because the system stream has content.
            //
            // Order matters, and so does who gets the speaker hint. The
            // per-note hint is a *total* across both streams, and only one
            // stream can be handed a derived count, because the second one's
            // share is only knowable after the first has been diarized. The
            // hint is worth far more on the mic: `withSpeakers(exactly:)` is
            // the one reliable lever we have over VBx, which otherwise "tends
            // to choose 1 on dominant-speaker conversations" (see
            // speaker-diarize/main.swift) — exactly the in-person meeting
            // where one person does most of the talking. So the system stream
            // goes first and the mic takes the remainder.
            //
            // Before either, the echo pass: the speakers' echo on the mic is
            // heard as extra voices, which the check drops before numbering.
            // Mirrors `rediarize_apply_to_chunks`' hybrid branch; change both.
            let mut echo = EchoPass::run(mic_wav.as_deref(), sys_wav.as_deref(), || {
                progress.step(&app, Step::RemovingEcho)
            })
            .await;
            record_timing(clock, |x| x.echo_ms = echo.ms);
            let mut diarize_steps = StreamSteps::new(
                &app,
                progress,
                Step::Diarizing,
                &[sys_wav.is_some(), mic_wav.is_some()],
            );
            let sys_speaker_hint = hybrid_sys_hint(expected_speakers, &chunks);
            let sys_segments = match sys_wav.clone() {
                None => {
                    eprintln!("diarize: sys chunks present but sys_full.wav missing");
                    emit_error(
                        &app,
                        Some(&note_id),
                        "Diarization unavailable for the remote side; remote speech grouped under one speaker.",
                    );
                    Vec::new()
                }
                Some(wav) => {
                    diarize_steps.begin();
                    timed_diarize(&app, &note_id, &wav, sys_speaker_hint, &mut engine, thresholds, clock, ChunkSource::Sys)
                        .await
                        .unwrap_or_else(|e| {
                            eprintln!("diarize: sys diarize failed ({e})");
                            emit_error(
                                &app,
                                Some(&note_id),
                                &format!("Diarization failed for the remote side ({e}); remote speech grouped under one speaker."),
                            );
                            Vec::new()
                        })
                }
            };
            let mic_speaker_hint = mic_hint_after_sys(expected_speakers, &sys_segments);
            let mic_segments = match mic_wav.clone() {
                None => {
                    eprintln!("diarize: hybrid but mic_full.wav missing, mic falls back to You");
                    Vec::new()
                }
                Some(wav) => {
                    diarize_steps.begin();
                    timed_diarize(&app, &note_id, echo.mic_input(&wav), mic_speaker_hint, &mut engine, thresholds, clock, ChunkSource::Mic)
                        .await
                        .unwrap_or_else(|e| {
                            eprintln!("diarize: hybrid mic diarize failed ({e}), mic falls back to You");
                            Vec::new()
                        })
                }
            };
            echo.release_cancelled_mic();
            if mic_segments.is_empty() && sys_segments.is_empty() {
                emit_error(
                    &app,
                    Some(&note_id),
                    "Diarization found no distinct speakers; speech grouped by audio source.",
                );
            }
            let (kept_mic, echo_report) = echo.check(&mic_segments);
            write_diagnostics_timed(&app, &note_id, engine, "hybrid", &mic_segments, &sys_segments, &chunks, &thresholds, clock, echo_report.as_ref()).await;
            let mic_segments = kept_mic;
            let labels = build_hybrid_labels(&chunks, &mic_segments, &sys_segments);
            // A stream whose diarize produced nothing still needs a distinct
            // label. A `None` label makes `build_labelled_transcript` glue the
            // chunk's text onto the previous line, which would silently merge
            // remote speech into the user's own turn — the bug
            // `hybrid_fallback_keeps_sys_chunks_distinct_from_mic` pins.
            // `next_free` keeps that fallback off any number the other
            // stream's real speakers already took.
            let sys_fallback = format!("Speaker {}", labels.next_free);
            let HybridLabels { mic: mic_labels, sys: sys_labels, .. } = labels;
            Box::new(move |c: &ChunkRecord| match c.source {
                ChunkSource::Mic if mic_segments.is_empty() => {
                    single_piece(c, Some("You".to_string()))
                }
                ChunkSource::Mic => split_by_labels(c, &mic_segments, &mic_labels),
                ChunkSource::Sys if sys_segments.is_empty() => {
                    single_piece(c, Some(sys_fallback.clone()))
                }
                ChunkSource::Sys => split_by_labels(c, &sys_segments, &sys_labels),
            })
        }
        (false, false) => unreachable!("chunks.is_empty() returned earlier"),
        }
    };

    let new_session = build_labelled_transcript(&chunks, split_chunk.as_ref());
    let combined = combine_with_snapshot(&snapshot, &new_session);
    // Same offset combine_with_snapshot applied to the DB transcript — bake it
    // into this session's timeline labels so the styled reader (rendered from
    // the concatenated per-session timelines) matches the saved transcript.
    let label_offset = session_speaker_offset(&snapshot);
    if combined.trim().is_empty() {
        return Ok(());
    }
    {
        let state: State<AppState> = app.state();
        let conn = state.db.lock();
        db::set_transcript(&conn, &note_id, &combined)?;
        // Content-settled checkpoint (issue #47): the diarized transcript is
        // the final transcript — refresh retrieval chunks so chat can search it.
        chat::reindex_note_content(&conn, &note_id);
    }
    // Embed the refreshed chunks off the request path (issue #48).
    tauri::async_runtime::spawn(chat::embed_note_bg(app.clone(), note_id.clone()));
    let _ = app.emit(
        "transcript_replaced",
        TranscriptPayload {
            note_id: note_id.clone(),
            text: combined,
        },
    );

    // Persist the playback bundle before we drop the temp full WAVs: the
    // per-turn timeline always, the mixed WAV only when keep_audio is on
    // (#24 — see write_playback_assets). Best-effort: failures log to
    // stderr but don't abort the post-stop chain. Compute the
    // timeline synchronously so the splitter doesn't have to be Send +
    // Sync to cross the awaits inside write_playback_assets.
    let timeline = serialize_timeline(&chunks, split_chunk.as_ref(), label_offset);
    progress.step(&app, Step::WritingPlayback);
    let t_playback = std::time::Instant::now();
    write_playback_assets(
        &app,
        &note_id,
        &session_id,
        timeline,
        mic_wav.as_deref(),
        sys_wav.as_deref(),
    )
    .await;
    record_timing(clock, |x| x.playback_assets_ms = ms_since(t_playback));

    // Free the full.wav files ahead of the temp-dir cleanup. Best-effort.
    if let Some(p) = mic_wav { diarize::cleanup_full_wav(&p).await; }
    if let Some(p) = sys_wav { diarize::cleanup_full_wav(&p).await; }
    Ok(())
}

/// Walk the chunks of a given source in time order, assigning each
/// distinct speaker_id a 1-indexed display number on first encounter.
/// When a chunk has word timings, each word's absolute midpoint is
/// looked up so a speaker that only appears mid-chunk still gets a
/// number — without this, `split_by_segments` would drop their pieces
/// to `None` (no entry in the map) and their text would silently merge
/// onto the surrounding speaker's line.
fn build_display_map(
    chunks: &[ChunkRecord],
    segments: &[diarize::Segment],
    source: ChunkSource,
) -> std::collections::HashMap<String, u32> {
    let mut map: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let record = |sid: &str, map: &mut std::collections::HashMap<String, u32>| {
        if !map.contains_key(sid) {
            let n = (map.len() as u32) + 1;
            map.insert(sid.to_string(), n);
        }
    };
    for chunk in chunks.iter().filter(|c| c.source == source) {
        if chunk.words.is_empty() {
            if let Some(sid) = assign_speaker(chunk.start_ms, segments) {
                record(sid, &mut map);
            }
            continue;
        }
        for word in &chunk.words {
            let half = word.end_ms.saturating_sub(word.start_ms) / 2;
            let mid = word.start_ms.saturating_add(half);
            let abs = chunk.start_ms.saturating_add(mid);
            if let Some(sid) = assign_speaker(abs, segments) {
                record(sid, &mut map);
            }
        }
    }
    map
}

/// Below this share of the chunks, the system stream is treated as incidental
/// rather than as a side of the conversation — a notification chime or a few
/// seconds of video during an in-person meeting. One sys chunk out of 157 is
/// 0.6%; a real call puts tens of percent on the system side.
const INCIDENTAL_STREAM_CHUNK_SHARE: f32 = 0.05;

/// Speaker hint for the system stream of a hybrid recording.
///
/// The per-note hint is a total across both streams, and the old `n - 1`
/// assumed exactly one person on the mic. That still holds for a real remote
/// call, so it's kept — but only when the system stream is actually carrying
/// one. When sys is incidental, asking the diarizer to find `n - 1` speakers in
/// what is effectively silence is worse than asking it for nothing, so it gets
/// no hint and the whole total stays available for the mic.
fn hybrid_sys_hint(expected_speakers: Option<i64>, chunks: &[ChunkRecord]) -> Option<i64> {
    if chunks.is_empty() {
        return None;
    }
    let sys = chunks.iter().filter(|c| c.source == ChunkSource::Sys).count();
    let share = sys as f32 / chunks.len() as f32;
    if share < INCIDENTAL_STREAM_CHUNK_SHARE {
        return None;
    }
    expected_speakers.map(|n| (n - 1).max(1))
}

/// Speaker hint for the mic stream, once the system stream has been diarized:
/// the note's total minus the voices the system side actually accounted for,
/// floored at 1.
///
/// This is where the hint earns its keep. `withSpeakers(exactly:)` is the only
/// reliable override of VBx's cluster-count search, which collapses to a single
/// speaker on conversations where one person dominates — so an in-person
/// meeting of three where one does most of the talking needs the count to come
/// through, or everyone merges into one label.
fn mic_hint_after_sys(
    expected_speakers: Option<i64>,
    sys_segments: &[diarize::Segment],
) -> Option<i64> {
    let sys_voices = distinct_speaker_count(sys_segments) as i64;
    expected_speakers.map(|n| (n - sys_voices).max(1))
}

/// How many distinct voices a segment list describes. Used to turn the
/// per-note "expected speakers" hint — a total across both streams — into a
/// hint for the stream that hasn't been diarized yet. Counts raw segment ids
/// rather than ids a chunk reached, because it feeds a hint rather than a
/// label; `build_hybrid_labels` does the stricter reached-only count for the
/// numbering itself.
fn distinct_speaker_count(segments: &[diarize::Segment]) -> usize {
    segments
        .iter()
        .map(|s| s.speaker_id.as_str())
        .collect::<std::collections::HashSet<_>>()
        .len()
}

/// Final display labels for a *hybrid* recording — one where both the mic
/// and the system stream produced speech.
struct HybridLabels {
    mic: std::collections::HashMap<String, String>,
    sys: std::collections::HashMap<String, String>,
    /// First speaker number not handed out. A stream whose diarize failed
    /// takes this as its fallback label, so the fallback can't collide with
    /// a real speaker found on the stream that succeeded.
    next_free: u32,
}

/// Number both streams' speakers for a hybrid recording.
///
/// This used to be channel attribution alone: every mic chunk was hard-
/// labelled `You` and only the system stream was diarized, on the assumption
/// that a two-stream recording is a remote call with exactly one person at
/// the microphone. That assumption fails in two ordinary shapes:
///
///   * An in-person meeting where something incidental plays through the
///     system output — a notification chime, a few seconds of video. A
///     *single* stray sys chunk was enough to route a whole room into the
///     remote-call branch, skip the mic diarize entirely, and collapse
///     everyone present onto one `You:` line.
///   * A genuine hybrid meeting: several people in a room, on a call with
///     remote participants. Everyone in the room shares the mic.
///
/// So both streams get diarized and both get numbered. `You` survives only
/// where it is actually earned — when the mic diarize resolves to exactly
/// one voice, that voice is the user, which is what a solo remote call looks
/// like. A stream with no segments (diarize unavailable, errored, or empty)
/// registers nothing and is left to the caller's fallback.
///
/// Numbering walks the merged chunk sequence in `(start_ms, source)` order
/// off one shared counter, so a mic speaker and a sys speaker can never land
/// on the same number. Mirrors `build_unified_display_map`'s rule for the
/// cross-session unify pass. As in `build_display_map`, only speaker ids a
/// chunk actually reaches consume a number — a segment nobody spoke over
/// doesn't burn `Speaker 2`.
fn build_hybrid_labels(
    chunks: &[ChunkRecord],
    mic_segments: &[diarize::Segment],
    sys_segments: &[diarize::Segment],
) -> HybridLabels {
    let mut mic_nums: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let mut sys_nums: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let mut next: u32 = 1;

    let mut sorted: Vec<&ChunkRecord> = chunks.iter().collect();
    sorted.sort_by_key(|c| {
        let source_rank = match c.source {
            ChunkSource::Mic => 0,
            ChunkSource::Sys => 1,
        };
        (c.start_ms, source_rank)
    });

    for chunk in sorted {
        let (segments, map) = match chunk.source {
            ChunkSource::Mic => (mic_segments, &mut mic_nums),
            ChunkSource::Sys => (sys_segments, &mut sys_nums),
        };
        if segments.is_empty() {
            continue;
        }
        let mut record = |sid: &str, next: &mut u32| {
            if !map.contains_key(sid) {
                map.insert(sid.to_string(), *next);
                *next += 1;
            }
        };
        if chunk.words.is_empty() {
            if let Some(sid) = assign_speaker(chunk.start_ms, segments) {
                record(sid, &mut next);
            }
            continue;
        }
        for word in &chunk.words {
            let half = word.end_ms.saturating_sub(word.start_ms) / 2;
            let mid = word.start_ms.saturating_add(half);
            let abs = chunk.start_ms.saturating_add(mid);
            if let Some(sid) = assign_speaker(abs, segments) {
                record(sid, &mut next);
            }
        }
    }

    finalise_stream_labels(mic_nums, sys_nums, true, next)
}

/// Shared final step for the per-take hybrid pass and the cross-session unify
/// pass: turn `speaker_id → N` maps into the display labels the transcript
/// actually carries.
///
/// When `allow_you` is set and the mic resolved to exactly one voice, that
/// voice is the user and takes `You` — the solo-remote-call shape. `You`
/// doesn't consume a number, so the sys side closes the hole by restarting at
/// 1 in assignment order. Otherwise every voice is numbered, which is what
/// keeps a room of people distinct.
fn finalise_stream_labels(
    mic_nums: std::collections::HashMap<String, u32>,
    sys_nums: std::collections::HashMap<String, u32>,
    allow_you: bool,
    next: u32,
) -> HybridLabels {
    if allow_you && mic_nums.len() == 1 {
        let mic_id = mic_nums.into_keys().next().expect("len checked == 1");
        let mut sys_ranked: Vec<(String, u32)> = sys_nums.into_iter().collect();
        sys_ranked.sort_by_key(|(_, n)| *n);
        let sys: std::collections::HashMap<String, String> = sys_ranked
            .into_iter()
            .enumerate()
            .map(|(i, (sid, _))| (sid, format!("Speaker {}", i + 1)))
            .collect();
        let next_free = sys.len() as u32 + 1;
        return HybridLabels {
            mic: std::iter::once((mic_id, "You".to_string())).collect(),
            sys,
            next_free,
        };
    }

    let to_labels = |m: std::collections::HashMap<String, u32>| {
        m.into_iter()
            .map(|(sid, n)| (sid, format!("Speaker {n}")))
            .collect()
    };
    HybridLabels {
        mic: to_labels(mic_nums),
        sys: to_labels(sys_nums),
        next_free: next,
    }
}

/// Stitch the prior transcript snapshot to a freshly diarized session.
/// When the snapshot is empty, the new text wins outright. When both have
/// content, this offsets the new session's `Speaker N:` numbers past the
/// highest one already in the snapshot (so a resume doesn't collide
/// "Speaker 1" from session 1 with a different "Speaker 1" from session 2)
/// and joins them with a newline.
fn combine_with_snapshot(snapshot: &str, new_session: &str) -> String {
    let snap_trimmed = snapshot.trim_end();
    if snap_trimmed.is_empty() {
        return new_session.to_string();
    }
    let new_trimmed = new_session.trim();
    if new_trimmed.is_empty() {
        return snap_trimmed.to_string();
    }
    let offset = max_speaker_number(snap_trimmed);
    let offset_new = if offset > 0 {
        offset_speaker_numbers(new_trimmed, offset)
    } else {
        new_trimmed.to_string()
    };
    format!("{snap_trimmed}\n{offset_new}")
}

/// Per-session speaker-number offset: the highest `Speaker N` already present
/// in the prior-transcript snapshot. This is the same value
/// `combine_with_snapshot` uses to renumber a resumed take's speakers, and it
/// gets baked into the session's own `timeline.jsonl` labels so the styled
/// reader (which renders from the concatenated timelines) shows exactly the
/// same labels as the saved transcript + chip strip.
fn session_speaker_offset(snapshot: &str) -> u32 {
    max_speaker_number(snapshot.trim_end())
}

/// Offset a *bare* speaker label (no trailing colon): `"Speaker 2"` + 3 →
/// `"Speaker 5"`. Non-numbered labels (`"You"`, `"Michael"`) and the empty
/// label pass through untouched, matching `offset_speaker_numbers`.
fn offset_speaker_label(label: &str, offset: u32) -> String {
    if offset == 0 {
        return label.to_string();
    }
    if let Some(rest) = label.strip_prefix("Speaker ") {
        if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
            if let Ok(n) = rest.parse::<u32>() {
                return format!("Speaker {}", n + offset);
            }
        }
    }
    label.to_string()
}

/// Highest N appearing in any line that starts with `Speaker N:`. Returns
/// 0 when none are found — useful for "should we offset?" checks.
fn max_speaker_number(text: &str) -> u32 {
    let mut max = 0u32;
    for line in text.lines() {
        if let Some(rest) = line.trim_start().strip_prefix("Speaker ") {
            // Read digits up to the colon; "Speaker 12: foo" → 12.
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(n) = digits.parse::<u32>() {
                if n > max {
                    max = n;
                }
            }
        }
    }
    max
}

/// Rewrite `Speaker N:` line prefixes by adding `offset` to every N. Only
/// touches the literal pattern we emit ourselves (`^Speaker \d+: `), so
/// renamed speakers ("Michael:", "Wilma:") stay untouched and free text
/// that happens to contain "Speaker 1" mid-sentence isn't rewritten.
fn offset_speaker_numbers(text: &str, offset: u32) -> String {
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.lines().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if let Some(rest) = line.strip_prefix("Speaker ") {
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            let after_digits = &rest[digits.len()..];
            if !digits.is_empty() && after_digits.starts_with(": ") {
                if let Ok(n) = digits.parse::<u32>() {
                    out.push_str(&format!("Speaker {}{}", n + offset, after_digits));
                    continue;
                }
            }
        }
        out.push_str(line);
    }
    out
}

/// Map a chunk's start time to a speaker_id by checking which segment
/// contains it; falls back to the closest segment by edge distance when
/// the chunk lands in a gap (silence between turns, or before/after the
/// segmented region). Returns None only when `segments` is empty.
fn assign_speaker(chunk_start_ms: u64, segments: &[diarize::Segment]) -> Option<&str> {
    for seg in segments {
        if chunk_start_ms >= seg.start_ms && chunk_start_ms < seg.end_ms {
            return Some(&seg.speaker_id);
        }
    }
    segments
        .iter()
        .min_by_key(|s| {
            if chunk_start_ms < s.start_ms {
                s.start_ms - chunk_start_ms
            } else {
                chunk_start_ms.saturating_sub(s.end_ms)
            }
        })
        .map(|s| s.speaker_id.as_str())
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

// ---- Cross-session speaker unification (#17) -------------------------------
//
// Each recording session is diarized on its own WAV, so clustering can't know
// that session 2's "Speaker 1" is the same voice as session 1's — the offset
// combine just renumbers past the previous max and a 2-person, 3-stop meeting
// shows up to 8 labels. The unify pass concatenates the retained per-session
// source WAVs (per stream, matching the per-source diarize passes), re-runs
// clustering over the combined audio so one voice = one cluster across takes,
// and rebuilds every session's timeline labels from the unified result. User
// renames survive: the old timeline labels are carried onto the new clusters
// by speech-time overlap (custom names beat generated `Speaker N` ones).
//
// The pass recomputes from fresh clustering every run — no stored marker —
// so re-running it is idempotent by construction: the same audio yields the
// same clusters, and re-applying the carried names reproduces the same
// timelines.

/// Which streams a session's chunks cover. Mirrors `diarize_and_apply`'s
/// per-take branch: mic-only diarizes the mic stream, sys-only the system
/// stream, hybrid labels mic "You" by channel attribution and diarizes sys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionMode {
    MicOnly,
    SysOnly,
    Hybrid,
}

/// Classify a session's chunk log. `None` when it recorded nothing.
fn session_mode(chunks: &[ChunkRecord]) -> Option<SessionMode> {
    let mic = chunks.iter().any(|c| c.source == ChunkSource::Mic);
    let sys = chunks.iter().any(|c| c.source == ChunkSource::Sys);
    match (mic, sys) {
        (true, false) => Some(SessionMode::MicOnly),
        (false, true) => Some(SessionMode::SysOnly),
        (true, true) => Some(SessionMode::Hybrid),
        (false, false) => None,
    }
}

/// Whether a session can join the concatenated unify pass, given which source
/// WAVs survive on disk: it needs every WAV whose stream will be diarized.
/// Mic-only needs `mic.wav`, sys-only needs `sys.wav`, and **hybrid needs
/// both** — its mic is diarized now rather than being labelled `You` by
/// channel attribution, and `concat_wavs` fails the whole pass on an
/// unreadable input, which would also misalign every later session's concat
/// offset. Sessions that fail this stay "frozen": their existing labels are
/// kept and the unified numbering is offset past them.
fn session_unifiable(mode: SessionMode, has_mic_wav: bool, has_sys_wav: bool) -> bool {
    match mode {
        SessionMode::MicOnly => has_mic_wav,
        SessionMode::SysOnly => has_sys_wav,
        SessionMode::Hybrid => has_mic_wav && has_sys_wav,
    }
}

/// One session's input to the unified relabel pass.
struct UnifySession {
    session_id: String,
    mode: SessionMode,
    /// Chunk log with session-local times, exactly as recorded.
    chunks: Vec<ChunkRecord>,
    /// This session's start offset (ms) inside the concatenated mic / sys
    /// WAV. Only meaningful for the stream(s) this session contributed.
    mic_offset_ms: u64,
    sys_offset_ms: u64,
    /// The labels currently on disk (timeline.jsonl before the rewrite) —
    /// the only place user renames live (there is no metadata table).
    old_spans: Vec<LabelSpan>,
}

/// One timeline entry's label + time span (+ stream when the entry was
/// written by a build that records it). Used to carry user renames from the
/// pre-unify timeline onto the freshly clustered one by time overlap.
#[derive(Clone, Debug)]
struct LabelSpan {
    start_ms: u64,
    end_ms: u64,
    label: String,
    source: Option<ChunkSource>,
}

/// Millisecond start offset of each input inside a concatenation, from real
/// sample counts (16 kHz mono → 16 samples per ms). Using actual samples
/// rather than the manifest's best-effort `durationMs` keeps the chunk-offset
/// map aligned with the diarizer's own clock over multi-session concats.
fn concat_offsets_ms(sample_counts: &[usize]) -> Vec<u64> {
    let mut out = Vec::with_capacity(sample_counts.len());
    let mut acc_samples: u64 = 0;
    for &n in sample_counts {
        out.push(acc_samples / 16);
        acc_samples += n as u64;
    }
    out
}

/// Labels the pipeline emits itself: `Speaker N` and the fixed hybrid-mic
/// `You` (plus the empty "no label" marker). Everything else is a user
/// rename. Deviation from a literal "custom = not /^Speaker \d+$/" reading
/// for "You": it's system-generated channel attribution, and treating it as
/// custom would let a remote speaker's cluster inherit "You" through
/// incidental time overlap with the user's own mic entries.
fn is_generated_label(label: &str) -> bool {
    if label.is_empty() || label == "You" {
        return true;
    }
    label
        .strip_prefix("Speaker ")
        .map(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
        .unwrap_or(false)
}

/// Overlap between two spans in ms; 0 when disjoint.
fn spans_overlap_ms(a: &LabelSpan, b: &LabelSpan) -> u64 {
    let start = a.start_ms.max(b.start_ms);
    let end = a.end_ms.min(b.end_ms);
    end.saturating_sub(start)
}

/// Parse timeline JSONL values into label spans. `source` is `None` for
/// entries written before the field existed (pre-#17 builds).
fn spans_from_values(values: &[serde_json::Value]) -> Vec<LabelSpan> {
    values
        .iter()
        .filter_map(|v| {
            let label = v.get("label")?.as_str()?.to_string();
            let start_ms = v.get("start_ms")?.as_u64()?;
            let end_ms = v.get("end_ms")?.as_u64()?;
            let source = v
                .get("source")
                .and_then(|s| s.as_str())
                .and_then(|s| match s {
                    "mic" => Some(ChunkSource::Mic),
                    "sys" => Some(ChunkSource::Sys),
                    _ => None,
                });
            Some(LabelSpan { start_ms, end_ms, label, source })
        })
        .collect()
}

/// Highest exact `Speaker N` label in a session's timeline file. Drives the
/// numbering offset that keeps unified labels from colliding with frozen
/// (non-unifiable) sessions' existing numbers.
fn max_speaker_in_timeline(path: &std::path::Path) -> u32 {
    read_timeline_values(path)
        .iter()
        .filter_map(|v| v.get("label").and_then(|s| s.as_str()).map(str::to_string))
        .filter_map(|l| l.strip_prefix("Speaker ").and_then(|r| r.parse::<u32>().ok()))
        .max()
        .unwrap_or(0)
}

/// First-encounter display numbering over the whole note. Walks sessions in
/// manifest order and chunks in the `(start_ms, source)` order the timeline
/// serialiser uses, resolving each chunk (or word midpoint) against the
/// combined-timeline segments after shifting by the session's concat offset.
/// One shared counter across both streams so a reader meets `Speaker 1`,
/// `Speaker 2`, … in reading order regardless of which stream each voice is
/// on. Hybrid sessions' mic chunks are skipped (fixed "You").
fn build_unified_display_map(
    sessions: &[UnifySession],
    mic_segments: &[diarize::Segment],
    sys_segments: &[diarize::Segment],
) -> (
    std::collections::HashMap<String, u32>,
    std::collections::HashMap<String, u32>,
    u32,
) {
    let mut mic_map: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let mut sys_map: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let mut next: u32 = 1;
    for sess in sessions {
        let mut sorted: Vec<&ChunkRecord> = sess.chunks.iter().collect();
        sorted.sort_by_key(|c| {
            let source_rank = match c.source {
                ChunkSource::Mic => 0,
                ChunkSource::Sys => 1,
            };
            (c.start_ms, source_rank)
        });
        for chunk in sorted {
            // A hybrid take's mic used to be skipped here — it was labelled
            // `You` by channel attribution and never diarized. It now gets
            // numbered like any other stream so several people sharing one
            // mic stay distinct; `finalise_stream_labels` re-applies `You`
            // when the mic really did resolve to a single voice.
            let (segments, offset, map) = match chunk.source {
                ChunkSource::Mic => (mic_segments, sess.mic_offset_ms, &mut mic_map),
                ChunkSource::Sys => (sys_segments, sess.sys_offset_ms, &mut sys_map),
            };
            if segments.is_empty() {
                continue;
            }
            if chunk.words.is_empty() {
                if let Some(sid) = assign_speaker(chunk.start_ms.saturating_add(offset), segments) {
                    if !map.contains_key(sid) {
                        map.insert(sid.to_string(), next);
                        next += 1;
                    }
                }
                continue;
            }
            for word in &chunk.words {
                let half = word.end_ms.saturating_sub(word.start_ms) / 2;
                let mid = word.start_ms.saturating_add(half);
                let abs = chunk
                    .start_ms
                    .saturating_add(mid)
                    .saturating_add(offset);
                if let Some(sid) = assign_speaker(abs, segments) {
                    if !map.contains_key(sid) {
                        map.insert(sid.to_string(), next);
                        next += 1;
                    }
                }
            }
        }
    }
    (mic_map, sys_map, next)
}

/// Result of the pure relabel pass: per-session timeline JSONL (manifest
/// order, session-local times) plus human-readable notices for custom-name
/// collisions the pass had to resolve.
struct UnifyOutcome {
    timelines: Vec<(String, String)>,
    notices: Vec<String>,
}

/// Carry user renames from the old timelines onto the freshly clustered
/// labels. For each new (generated) label, accumulate speech-time overlap
/// against old *custom* labels — matched within the same session, and within
/// the same stream when both sides know their source. Rules (issue #17):
/// a custom name beats a generated `Speaker N`; when two different custom
/// names land in one cluster, the one covering more speech time wins and a
/// notice is emitted so the user can see (and undo, via rename) the merge.
fn custom_name_map(
    new_per_session: &[Vec<LabelSpan>],
    old_per_session: &[Vec<LabelSpan>],
) -> (std::collections::HashMap<String, String>, Vec<String>) {
    let mut acc: std::collections::HashMap<String, std::collections::HashMap<String, u64>> =
        std::collections::HashMap::new();
    for (news, olds) in new_per_session.iter().zip(old_per_session) {
        for n in news {
            // New labels are always pipeline-generated at this point; an
            // empty label marks an unlabelled piece and must never gain a
            // name it didn't have.
            if n.label.is_empty() || !is_generated_label(&n.label) {
                continue;
            }
            for o in olds {
                if is_generated_label(&o.label) {
                    continue;
                }
                if let (Some(a), Some(b)) = (n.source, o.source) {
                    if a != b {
                        continue;
                    }
                }
                let t = spans_overlap_ms(n, o);
                if t > 0 {
                    *acc.entry(n.label.clone())
                        .or_default()
                        .entry(o.label.clone())
                        .or_default() += t;
                }
            }
        }
    }
    let mut map = std::collections::HashMap::new();
    let mut notices = Vec::new();
    let mut keys: Vec<&String> = acc.keys().collect();
    keys.sort();
    for k in keys {
        let mut ranked: Vec<(&String, u64)> = acc[k].iter().map(|(n, &t)| (n, t)).collect();
        // Most speech time wins; name ascending as a deterministic tiebreak.
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        let (winner, _) = ranked[0];
        map.insert(k.clone(), winner.clone());
        if ranked.len() >= 2 {
            let losers: Vec<&str> = ranked[1..].iter().map(|(n, _)| n.as_str()).collect();
            notices.push(format!(
                "Speaker unification detected \"{}\" and \"{}\" as the same voice — kept \"{}\" (more speech time). Rename the speaker to adjust.",
                winner,
                losers.join("\" and \""),
                winner
            ));
        }
    }
    (map, notices)
}

/// The pure core of the unify pass: given every unifiable session's chunks,
/// concat offsets, and old labels, plus the segments from diarizing the
/// concatenated stream(s), rebuild each session's timeline JSONL with
/// unified labels. Times stay session-local — the splitter shifts a chunk
/// into concat time only for the segment lookup, and `serialize_timeline`
/// rebases words off the original chunk. `label_offset` bumps generated
/// numbers past frozen sessions' existing ones.
fn unify_relabel(
    sessions: &[UnifySession],
    mic_segments: &[diarize::Segment],
    sys_segments: &[diarize::Segment],
    label_offset: u32,
) -> UnifyOutcome {
    let (mic_nums, sys_nums, next) =
        build_unified_display_map(sessions, mic_segments, sys_segments);
    // `You` is a channel-attribution artifact of a *remote call*: it only holds
    // when every take that captured mic was one, and the combined mic stream
    // really did resolve to a single voice (see `finalise_stream_labels`). A
    // note that also has an in-person take numbers its voices instead —
    // otherwise unifying a solo remote take with an in-person one would relabel
    // the in-person speaker `You`, which is not what that take recorded.
    let mic_sessions: Vec<&UnifySession> = sessions
        .iter()
        .filter(|s| s.mode != SessionMode::SysOnly)
        .collect();
    let allow_you = !mic_sessions.is_empty()
        && mic_sessions.iter().all(|s| s.mode == SessionMode::Hybrid);
    let HybridLabels { mic: mic_map, sys: sys_map, .. } =
        finalise_stream_labels(mic_nums, sys_nums, allow_you, next);

    let mut values_per_session: Vec<Vec<serde_json::Value>> = Vec::new();
    for sess in sessions {
        let mic_off = sess.mic_offset_ms;
        let sys_off = sess.sys_offset_ms;
        let splitter = |c: &ChunkRecord| -> Vec<LabelledPiece> {
            match c.source {
                // A mic stream with no segments (diarize unavailable for this
                // note's mic side) keeps the old channel-attributed label
                // rather than emitting `None`, which would glue the text onto
                // whatever line came before it.
                ChunkSource::Mic if mic_segments.is_empty() => {
                    single_piece(c, Some("You".to_string()))
                }
                ChunkSource::Mic => {
                    let mut shifted = c.clone();
                    shifted.start_ms = shifted.start_ms.saturating_add(mic_off);
                    split_by_labels(&shifted, mic_segments, &mic_map)
                }
                ChunkSource::Sys => {
                    let mut shifted = c.clone();
                    shifted.start_ms = shifted.start_ms.saturating_add(sys_off);
                    split_by_labels(&shifted, sys_segments, &sys_map)
                }
            }
        };
        let jsonl = serialize_timeline(&sess.chunks, &splitter, label_offset);
        values_per_session.push(
            jsonl
                .lines()
                .filter(|l| !l.trim().is_empty())
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect(),
        );
    }

    let new_spans: Vec<Vec<LabelSpan>> =
        values_per_session.iter().map(|v| spans_from_values(v)).collect();
    let old_spans: Vec<Vec<LabelSpan>> =
        sessions.iter().map(|s| s.old_spans.clone()).collect();
    let (rename_map, notices) = custom_name_map(&new_spans, &old_spans);

    let mut timelines = Vec::new();
    for (sess, mut values) in sessions.iter().zip(values_per_session) {
        for v in values.iter_mut() {
            if let Some(label) = v.get("label").and_then(|s| s.as_str()) {
                if let Some(new_label) = rename_map.get(label) {
                    v["label"] = serde_json::Value::String(new_label.clone());
                }
            }
        }
        let mut out = String::new();
        for v in &values {
            out.push_str(&v.to_string());
            out.push('\n');
        }
        timelines.push((sess.session_id.clone(), out));
    }
    UnifyOutcome { timelines, notices }
}

/// One session that qualified for the concatenated pass, with its chunk log
/// already loaded.
struct UnifyCandidate {
    entry: sessions::SessionEntry,
    dir: PathBuf,
    chunks: Vec<ChunkRecord>,
    mode: SessionMode,
}

/// Concatenate 16 kHz mono WAVs into `out`, returning each input's start
/// offset (ms) inside the result, computed from real sample counts. `None`
/// when `paths` is empty (no file written).
async fn concat_wavs(
    paths: &[PathBuf],
    out: &std::path::Path,
) -> anyhow::Result<Option<Vec<u64>>> {
    if paths.is_empty() {
        return Ok(None);
    }
    let mut combined: Vec<f32> = Vec::new();
    let mut counts = Vec::with_capacity(paths.len());
    for p in paths {
        let samples = wav::read_f32_mono_16k(p).await?;
        counts.push(samples.len());
        combined.extend_from_slice(&samples);
    }
    wav::write_pcm16_mono_16k(out, &combined).await?;
    Ok(Some(concat_offsets_ms(&counts)))
}

/// What the named steps inside one unify pass cost, in ms, beyond the pass as
/// a whole.
pub(crate) struct UnifyCosts {
    /// The echo pass over every hybrid take; `None` when there was none.
    echo_ms: Option<u64>,
}

/// A scratch dir for one unify invocation's concat WAVs. Unique per call (a
/// UUID suffix) so two overlapping unify passes for the *same* note — auto-unify
/// in the post-stop chain racing a user `rediarize_note`, or two rapid
/// Re-diarize clicks — never write to and `remove_dir_all` each other's concat
/// files (a mid-read truncation that silently produced wrong unified labels).
fn unify_scratch_dir(note_id: &str) -> PathBuf {
    std::env::temp_dir().join(format!("humla-unify-{note_id}-{}", uuid::Uuid::new_v4()))
}

/// Get (or lazily create) the per-note unify lock. Same note → same `Arc`, so
/// concurrent unify passes for it serialize; different notes get independent
/// locks and stay concurrent. The `parking_lot` guard is dropped before the
/// caller `.await`s on the returned tokio lock.
fn unify_note_lock(
    locks: &parking_lot::Mutex<std::collections::HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    note_id: &str,
) -> Arc<tokio::sync::Mutex<()>> {
    locks.lock().entry(note_id.to_string()).or_default().clone()
}

/// Run the cross-session speaker unification pass on a note: every take's
/// audio joined end to end and diarized once per stream, all takes each time,
/// so one voice keeps one label across them. The joined WAVs live in a temp
/// dir removed when the pass ends.
///
/// Returns what it cost when the pass ran and rewrote labels, and `Ok(None)`
/// when the note doesn't qualify (fewer than two sessions, fewer than two with
/// retained source audio and chunk timings, or no diarize model), in which case
/// callers keep the per-take labels. `Err` leaves the existing labels
/// untouched: the pass writes only after every diarize it needed succeeded.
pub(crate) async fn unify_note_speakers(
    app: &AppHandle,
    note_id: &str,
    // Where the pass reports. Passed in, never chosen here: this one function
    // is reached from the live stop, the user-pressed Re-diarize and a deferred
    // replay, and only the first of those owns `recording_status`.
    progress: &ChainProgress,
) -> anyhow::Result<Option<UnifyCosts>> {
    // Per-note re-entrancy guard: a second unify for this note waits for the
    // first to finish (then re-runs on its up-to-date timelines) rather than
    // racing it. Held for the whole pass; safe because unify is a leaf that
    // never re-enters itself, so this can't deadlock the post-stop chain.
    let note_lock = unify_note_lock(&app.state::<AppState>().unify_locks, note_id);
    let _unify_guard = note_lock.lock().await;

    let app_dir = app.path().app_data_dir()?;
    let recordings = sessions::recordings_dir(&app_dir, note_id);
    let resolved = sessions::resolve_sessions(&recordings);
    if resolved.len() < 2 {
        return Ok(None);
    }

    let settings = note_diarize_settings(app, note_id).await;
    if !settings.choice.downloaded {
        eprintln!("unify: diarize model not downloaded, keeping per-take labels");
        return Ok(None);
    }

    // Partition into sessions that can join the concatenated pass and
    // "frozen" ones that keep their existing labels — a take without
    // chunks.json or its diarized-stream WAV (e.g. a first take recorded
    // before auto-retention kicked in, or a keep-audio-off legacy note)
    // can't be re-clustered, so its labels stay and the unified numbering
    // is offset past them.
    let mut unifiable: Vec<UnifyCandidate> = Vec::new();
    let mut frozen_dirs: Vec<PathBuf> = Vec::new();
    for (entry, dir) in resolved {
        let chunks_path = dir.join("chunks.json");
        let chunks = if chunks_path.exists() {
            parse_chunks_json(&chunks_path).unwrap_or_default()
        } else {
            Vec::new()
        };
        let has_mic = dir.join("mic.wav").exists();
        let has_sys = dir.join("sys.wav").exists();
        match session_mode(&chunks) {
            Some(mode) if session_unifiable(mode, has_mic, has_sys) => {
                unifiable.push(UnifyCandidate { entry, dir, chunks, mode });
            }
            _ => frozen_dirs.push(dir),
        }
    }
    if unifiable.len() < 2 {
        eprintln!(
            "unify: only {} session(s) have retained audio + chunk timings (need 2+), keeping per-take labels",
            unifiable.len()
        );
        return Ok(None);
    }

    // Concat WAVs are scratch files — never inside the session dirs. Unique per
    // invocation so overlapping passes can't clobber each other's concats.
    let tmp = unify_scratch_dir(note_id);
    tokio::fs::create_dir_all(&tmp).await?;
    let result = unify_apply(app, note_id, &tmp, &unifiable, &frozen_dirs, settings, progress).await;
    let _ = tokio::fs::remove_dir_all(&tmp).await;
    result.map(Some)
}

/// The IO half of the unify pass, split out so the caller can always clean
/// up the temp dir. Concatenates + diarizes per stream, runs the pure
/// relabel, writes every unified session's timeline, and rebuilds the DB
/// transcript from all sessions (frozen ones included, untouched).
async fn unify_apply(
    app: &AppHandle,
    note_id: &str,
    tmp: &std::path::Path,
    unifiable: &[UnifyCandidate],
    frozen_dirs: &[PathBuf],
    settings: NoteDiarizeSettings,
    progress: &ChainProgress,
) -> anyhow::Result<UnifyCosts> {
    let NoteDiarizeSettings { choice, thresholds, expected_speakers } = settings;
    let mut engine = choice.engine;
    // Per-stream concatenation in manifest order. Mic concat = every session
    // that captured mic (mic-only *and* hybrid — a hybrid take's mic is
    // diarized now, not assumed to be one person); sys concat = sys-only +
    // hybrid. Matches the per-source passes diarize_and_apply runs on a single
    // take. `session_unifiable` guarantees a hybrid session has both WAVs, so
    // neither concat can hit a missing file and misalign the offsets.
    let mic_takes: Vec<&UnifyCandidate> =
        unifiable.iter().filter(|c| c.mode != SessionMode::SysOnly).collect();
    let sys_paths: Vec<PathBuf> = unifiable
        .iter()
        .filter(|c| c.mode != SessionMode::MicOnly)
        .map(|c| c.dir.join("sys.wav"))
        .collect();
    let mic_concat = tmp.join("mic-concat.wav");
    let sys_concat = tmp.join("sys-concat.wav");
    let mut echoes = cancel_each_takes_echo(app, progress, &mic_takes).await;
    let echo_ms = echoes.iter().filter_map(|e| e.ms).reduce(|a, b| a + b);
    let mic_paths: Vec<PathBuf> = mic_takes
        .iter()
        .zip(&echoes)
        .map(|(take, echo)| echo.mic_input(&take.dir.join("mic.wav")).to_path_buf())
        .collect();
    // Named a stream at a time: each stream's concat and its diarize are
    // announced together, so the label names the work actually running.
    let mut unify_steps = StreamSteps::new(
        app,
        progress,
        Step::MatchingSpeakers,
        &[!mic_paths.is_empty(), !sys_paths.is_empty()],
    );
    // Same speaker-count hint semantics as the per-take pass. With no system
    // stream in play the note's expected_speakers applies to the mic directly
    // (in-person). Once both streams exist the total can't be split a priori,
    // so the mic goes in unhinted and the sys stream is asked for whatever the
    // mic didn't account for. Only community-1 takes the hint; Nemotron 3
    // counts the combined audio itself, as it does a single take.
    if !mic_paths.is_empty() {
        unify_steps.begin();
    }
    let mic_offsets = concat_wavs(&mic_paths, &mic_concat).await?;
    echoes.iter_mut().for_each(EchoPass::release_cancelled_mic);
    let mic_hint = if sys_paths.is_empty() {
        expected_speakers
    } else {
        None
    };
    let mic_segments = match &mic_offsets {
        Some(_) => diarize_and_maybe_clean(app, note_id, &mic_concat, mic_hint, &mut engine, thresholds).await?,
        None => Vec::new(),
    };
    if mic_offsets.is_some() && mic_segments.is_empty() {
        anyhow::bail!("diarize returned no segments for the combined mic stream");
    }
    // Dropped before the sys hint counts the mic's voices, which echo must
    // not add to.
    let (mic_segments, echo_takes) = drop_joined_echo(
        &mic_takes,
        &echoes,
        mic_offsets.as_deref().unwrap_or_default(),
        &mic_segments,
    );
    if !sys_paths.is_empty() {
        unify_steps.begin();
    }
    let sys_offsets = concat_wavs(&sys_paths, &sys_concat).await?;
    let sys_hint = if mic_paths.is_empty() {
        expected_speakers
    } else {
        expected_speakers
            .map(|n| (n - distinct_speaker_count(&mic_segments).max(1) as i64).max(1))
    };
    let sys_segments = match &sys_offsets {
        Some(_) => {
            diarize_and_maybe_clean(app, note_id, &sys_concat, sys_hint, &mut engine, thresholds).await?
        }
        None => Vec::new(),
    };
    if sys_offsets.is_some() && sys_segments.is_empty() {
        anyhow::bail!("diarize returned no segments for the combined system stream");
    }
    if !echo_takes.is_empty() {
        write_unify_dump(app, note_id, engine, &echo_takes).await;
    }

    // Unified generated numbers start past any frozen session's existing
    // ones so the two label spaces never collide in the merged transcript.
    let label_offset = frozen_dirs
        .iter()
        .map(|d| max_speaker_in_timeline(&d.join("timeline.jsonl")))
        .max()
        .unwrap_or(0);

    let mut mic_iter = mic_offsets.unwrap_or_default().into_iter();
    let mut sys_iter = sys_offsets.unwrap_or_default().into_iter();
    let sessions_in: Vec<UnifySession> = unifiable
        .iter()
        .map(|c| {
            // Must mirror `mic_takes` / `sys_paths` above exactly — these
            // offsets are consumed in the same order the concat was built, so a
            // filter that disagrees shifts every later session's segment
            // lookups into the wrong take's audio.
            let mic_offset_ms = if c.mode != SessionMode::SysOnly {
                mic_iter.next().unwrap_or(0)
            } else {
                0
            };
            let sys_offset_ms = if c.mode != SessionMode::MicOnly {
                sys_iter.next().unwrap_or(0)
            } else {
                0
            };
            UnifySession {
                session_id: c.entry.id.clone(),
                mode: c.mode,
                chunks: c.chunks.clone(),
                mic_offset_ms,
                sys_offset_ms,
                old_spans: spans_from_values(&read_timeline_values(
                    &c.dir.join("timeline.jsonl"),
                )),
            }
        })
        .collect();

    let outcome = unify_relabel(&sessions_in, &mic_segments, &sys_segments, label_offset);

    for ((session_id, jsonl), cand) in outcome.timelines.iter().zip(unifiable) {
        debug_assert_eq!(session_id, &cand.entry.id);
        tokio::fs::write(cand.dir.join("timeline.jsonl"), jsonl).await?;
    }

    // Rebuild the DB transcript from every session's timeline (frozen ones
    // keep their old labels) and notify the UI + sync.
    let transcript = rebuild_note_transcript(app, note_id).map_err(|e| anyhow::anyhow!(e))?;
    commit_rebuilt_transcript(app, note_id, transcript).map_err(|e| anyhow::anyhow!(e))?;

    // Surface custom-name merges on the existing transient-toast channel
    // (recording_error doubles as the informational surface — the sidecar's
    // non-fatal Diagnostic notices already ride it).
    for notice in &outcome.notices {
        emit_error(app, Some(note_id), notice);
    }
    eprintln!(
        "unify: relabelled {} session(s) from unified clustering ({} frozen)",
        unifiable.len(),
        frozen_dirs.len()
    );
    Ok(UnifyCosts { echo_ms })
}

/// Each hybrid take's echo pass, a take at a time since every take's lag is
/// its own: one [`EchoPass`] per mic take, holding nothing for a take with no
/// system stream. `session_unifiable` has made sure a hybrid take kept both.
async fn cancel_each_takes_echo(
    app: &AppHandle,
    progress: &ChainProgress,
    mic_takes: &[&UnifyCandidate],
) -> Vec<EchoPass> {
    let hybrid = mic_takes.iter().filter(|c| c.mode == SessionMode::Hybrid).count() as u32;
    let mut nth = 0;
    let mut echoes = Vec::with_capacity(mic_takes.len());
    for take in mic_takes {
        let sys = (take.mode == SessionMode::Hybrid).then(|| take.dir.join("sys.wav"));
        let echo = EchoPass::run(Some(&take.dir.join("mic.wav")), sys.as_deref(), || {
            nth += 1;
            progress.report(app, StepReport::counted(Step::RemovingEcho, nth, hybrid));
        })
        .await;
        echoes.push(echo);
    }
    echoes
}

/// What the unify dump records of one take's echo pass.
#[derive(serde::Serialize)]
struct UnifyTakeEcho<'a> {
    session_id: &'a str,
    index: u32,
    echo: crate::echo::EchoReport,
}

/// The joined mic's voices without the ones that read as echo, each hybrid
/// take judged inside its own span of it, and what the dump records of every
/// take that had an echo pass. `offsets[i]` is where `mic_takes[i]` starts in
/// the joined stream, and `echoes[i]` is its echo pass.
fn drop_joined_echo<'a>(
    mic_takes: &[&'a UnifyCandidate],
    echoes: &[EchoPass],
    offsets: &[u64],
    mic_segments: &[diarize::Segment],
) -> (Vec<diarize::Segment>, Vec<UnifyTakeEcho<'a>>) {
    let takes: Vec<crate::echo::JoinedTake> = echoes
        .iter()
        .zip(offsets)
        .map(|(echo, &start_ms)| crate::echo::JoinedTake {
            start_ms,
            analysis: echo.analysis.as_ref(),
        })
        .collect();
    let (kept, reports) = crate::echo::check_joined(mic_segments, &takes);
    let dump = mic_takes
        .iter()
        .zip(reports)
        .filter_map(|(take, report)| {
            let echo = report?;
            if let Some(check) = &echo.check {
                log_echo_check(check);
            }
            Some(UnifyTakeEcho { session_id: &take.entry.id, index: take.entry.index, echo })
        })
        .collect();
    (kept, dump)
}

/// `diagnostics/<note_id>/<engine>-unify.json`. Holds no `chunks`, so
/// `read_chunks_from_diagnostic` passes over it. Best-effort.
async fn write_unify_dump(
    app: &AppHandle,
    note_id: &str,
    engine: diarize::Engine,
    takes: &[UnifyTakeEcho<'_>],
) {
    let Ok(app_dir) = app.path().app_data_dir() else { return };
    let dir = app_dir.join("diagnostics").join(note_id);
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        eprintln!("diagnostics: mkdir {}: {e}", dir.display());
        return;
    }
    let path = dir.join(format!("{}-unify.json", engine.arg()));
    let payload = serde_json::json!({
        "engine": engine.arg(),
        "source": "unify",
        "takes": takes,
        "created_at": chrono::Utc::now().timestamp_millis(),
    });
    match serde_json::to_string_pretty(&payload) {
        Ok(json) => {
            if let Err(e) = tokio::fs::write(&path, json).await {
                eprintln!("diagnostics: write {}: {e}", path.display());
            }
        }
        Err(e) => eprintln!("diagnostics: serialize: {e}"),
    }
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
mod unify_concurrency_tests {
    use super::*;

    #[test]
    fn scratch_dir_is_unique_per_invocation() {
        // Two unify passes for the SAME note must get distinct scratch dirs, or
        // one's remove_dir_all truncates the other's concat WAVs mid-read →
        // wrong unified labels written to every session.
        let a = unify_scratch_dir("note1");
        let b = unify_scratch_dir("note1");
        assert_ne!(a, b);
        assert!(a.file_name().unwrap().to_string_lossy().contains("note1"));
        assert!(a.starts_with(std::env::temp_dir()));
    }

    #[test]
    fn note_lock_shared_per_note_distinct_across_notes() {
        let locks = parking_lot::Mutex::new(std::collections::HashMap::new());
        let a1 = unify_note_lock(&locks, "n1");
        let a2 = unify_note_lock(&locks, "n1");
        let b = unify_note_lock(&locks, "n2");
        // Same note → same lock (so passes serialize); different note → its own.
        assert!(Arc::ptr_eq(&a1, &a2));
        assert!(!Arc::ptr_eq(&a1, &b));
    }

    #[test]
    fn note_lock_serializes_same_note() {
        // A held lock forces a second unify for the same note to wait; a
        // different note is never blocked.
        let locks = parking_lot::Mutex::new(std::collections::HashMap::new());
        let held = unify_note_lock(&locks, "n1").try_lock_owned().unwrap();
        let same = unify_note_lock(&locks, "n1");
        assert!(same.try_lock().is_err(), "second unify for the note must wait");
        assert!(
            unify_note_lock(&locks, "n2").try_lock().is_ok(),
            "a different note stays concurrent"
        );
        drop(held);
        assert!(same.try_lock().is_ok(), "lock frees once the first pass ends");
    }
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
    use super::timeline::{comparable_words, orphaned_prefix, projection_covers, synthesize_orphan_timeline};
    use crate::diarize::Segment;

    fn seg(start_ms: u64, end_ms: u64, sid: &str) -> Segment {
        Segment { start_ms, end_ms, speaker_id: sid.to_string() }
    }

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

    /// Build the same labeller `diarize_and_apply` would build in the
    /// mic-only branch: every chunk gets `Speaker N:` from its segment.
    /// Pulled into a test helper so we can exercise `build_labelled_transcript`
    /// without mocking the sidecar.
    fn mic_only_labeller(
        chunks: Vec<ChunkRecord>,
        segments: Vec<Segment>,
    ) -> String {
        let display_map = build_display_map(&chunks, &segments, ChunkSource::Mic);
        let pieces = whole_chunk_pieces(move |c: &ChunkRecord| {
            let sid = assign_speaker(c.start_ms, &segments)?;
            display_map.get(sid).map(|n| format!("Speaker {n}"))
        });
        build_labelled_transcript(&chunks, &pieces)
    }

    #[test]
    fn split_chunk_with_no_words_falls_back_to_whole_chunk() {
        // The OpenAI / older-diagnostic path: chunk has text but no
        // word timings. split_by_segments must still emit one piece
        // labelled by start_ms (matching pre-split behaviour).
        let chunks = vec![sys(0, "hello world")];
        let segs = vec![seg(0, 10_000, "A")];
        let display_map = build_display_map(&chunks, &segs, ChunkSource::Sys);
        let pieces = split_by_segments(&chunks[0], &segs, &display_map);
        assert_eq!(pieces.len(), 1);
        assert_eq!(pieces[0].label.as_deref(), Some("Speaker 1"));
        assert_eq!(pieces[0].text, "hello world");
    }

    #[test]
    fn split_chunk_keeps_single_speaker_as_one_piece() {
        // All words fall inside the same speaker segment → one piece
        // covering the whole chunk text.
        let chunk = sys_with_words(
            10_000,
            vec![("hello", 0, 500), ("there", 500, 1000)],
        );
        let segs = vec![seg(0, 30_000, "A")];
        let display_map = build_display_map(
            std::slice::from_ref(&chunk),
            &segs,
            ChunkSource::Sys,
        );
        let pieces = split_by_segments(&chunk, &segs, &display_map);
        assert_eq!(pieces.len(), 1);
        assert_eq!(pieces[0].label.as_deref(), Some("Speaker 1"));
        assert_eq!(pieces[0].text, "hello there");
    }

    #[test]
    fn split_chunk_breaks_at_speaker_boundary_inside_chunk() {
        // The reported bug: a 15s VAD chunk that opens with one voice
        // and closes with another. Words 0–500 ms fall in segment A,
        // word at 8000 ms falls in segment B. We expect two pieces.
        // Chunk starts at 10_000 ms absolute, so word abs times are
        // 10_000, 10_500, 18_000.
        let chunk = sys_with_words(
            10_000,
            vec![
                ("first", 0, 500),
                ("second", 500, 1000),
                ("third", 8000, 8500),
            ],
        );
        let segs = vec![
            seg(10_000, 15_000, "A"),
            seg(15_000, 25_000, "B"),
        ];
        let display_map = build_display_map(
            std::slice::from_ref(&chunk),
            &segs,
            ChunkSource::Sys,
        );
        let pieces = split_by_segments(&chunk, &segs, &display_map);
        assert_eq!(pieces.len(), 2);
        assert_eq!(pieces[0].label.as_deref(), Some("Speaker 1"));
        assert_eq!(pieces[0].text, "first second");
        assert_eq!(pieces[1].label.as_deref(), Some("Speaker 2"));
        assert_eq!(pieces[1].text, "third");
    }

    #[test]
    fn split_chunk_handles_back_and_forth_within_chunk() {
        // The exact pattern in the user's screenshot: same chunk
        // contains alternating speakers. Three pieces: A then B
        // then A again, mapping to Speaker 1 / Speaker 2 / Speaker 1.
        let chunk = sys_with_words(
            0,
            vec![
                ("you", 0, 200),
                ("had", 200, 400),
                ("the", 400, 600),
                ("mouth", 600, 900),
                ("I", 5000, 5100),
                ("bet", 5100, 5400),
                ("yes", 9000, 9300),
                ("I", 9300, 9400),
                ("did", 9400, 9700),
            ],
        );
        let segs = vec![
            seg(0, 4_500, "A"),
            seg(4_500, 8_500, "B"),
            seg(8_500, 12_000, "A"),
        ];
        let display_map = build_display_map(
            std::slice::from_ref(&chunk),
            &segs,
            ChunkSource::Sys,
        );
        let pieces = split_by_segments(&chunk, &segs, &display_map);
        assert_eq!(pieces.len(), 3);
        assert_eq!(pieces[0].label.as_deref(), Some("Speaker 1"));
        assert_eq!(pieces[0].text, "you had the mouth");
        assert_eq!(pieces[1].label.as_deref(), Some("Speaker 2"));
        assert_eq!(pieces[1].text, "I bet");
        assert_eq!(pieces[2].label.as_deref(), Some("Speaker 1"));
        assert_eq!(pieces[2].text, "yes I did");
    }

    #[test]
    fn bridge_end_to_end_collapses_three_lines_to_one() {
        // The user-visible payoff: a chunk that emits three pieces
        // (Speaker 1 / Speaker 2 / Speaker 1, with the middle being a
        // brief backchannel) should render as a single Speaker 1 line.
        let chunk = sys_with_words(
            0,
            vec![
                ("the", 0, 200),
                ("plan", 200, 400),
                ("is", 400, 600),
                ("yeah", 4500, 4900),
                ("simple", 6000, 6500),
                ("really", 6500, 6900),
            ],
        );
        let segs = vec![
            seg(0, 4_400, "A"),
            seg(4_400, 5_000, "B"),
            seg(5_000, 10_000, "A"),
        ];
        let chunks = vec![chunk];
        let display_map = build_display_map(&chunks, &segs, ChunkSource::Sys);
        let splitter = move |c: &ChunkRecord| split_by_segments(c, &segs, &display_map);
        assert_eq!(
            build_labelled_transcript(&chunks, &splitter),
            "Speaker 1: the plan is yeah simple really"
        );
    }

    #[test]
    fn split_chunk_into_transcript_emits_separate_lines_per_speaker() {
        // End-to-end: a single chunk containing two speakers should
        // produce two `Speaker N:` lines in the rendered transcript,
        // not one. This is the user-visible behaviour the whole change
        // exists for. Sentence terminators on each turn keep the
        // post-build heuristic merge from collapsing the boundary —
        // real Whisper output emits these too.
        let chunks = vec![sys_with_words(
            0,
            vec![
                ("hello", 0, 500),
                ("there.", 500, 1000),
                ("Hi", 5000, 5300),
                ("back.", 5300, 5700),
            ],
        )];
        let segs = vec![seg(0, 4_500, "A"), seg(4_500, 10_000, "B")];
        let display_map = build_display_map(&chunks, &segs, ChunkSource::Sys);
        let splitter = move |c: &ChunkRecord| split_by_segments(c, &segs, &display_map);
        assert_eq!(
            build_labelled_transcript(&chunks, &splitter),
            "Speaker 1: hello there.\nSpeaker 2: Hi back."
        );
    }

    #[test]
    fn assign_speaker_inside_segment() {
        let segs = vec![seg(0, 5000, "A"), seg(5000, 10000, "B")];
        assert_eq!(assign_speaker(2500, &segs), Some("A"));
        assert_eq!(assign_speaker(5000, &segs), Some("B"));
        assert_eq!(assign_speaker(9999, &segs), Some("B"));
    }

    #[test]
    fn assign_speaker_in_gap_uses_closest() {
        // Gap from 5000-7000. Chunk at 5500 is closer to A (gap edge 5000)
        // than to B (gap edge 7000), so falls back to A.
        let segs = vec![seg(0, 5000, "A"), seg(7000, 10000, "B")];
        assert_eq!(assign_speaker(5500, &segs), Some("A"));
        assert_eq!(assign_speaker(6800, &segs), Some("B"));
    }

    #[test]
    fn assign_speaker_before_first_segment() {
        let segs = vec![seg(2000, 5000, "A")];
        assert_eq!(assign_speaker(500, &segs), Some("A"));
    }

    #[test]
    fn assign_speaker_empty_segments() {
        let segs: Vec<Segment> = vec![];
        assert_eq!(assign_speaker(1000, &segs), None);
    }

    #[test]
    fn build_transcript_empty_chunks() {
        assert_eq!(mic_only_labeller(vec![], vec![seg(0, 1000, "A")]), "");
    }

    #[test]
    fn build_transcript_single_speaker_runs() {
        // Three chunks all from speaker A — no newline, single-space joins.
        let chunks = vec![mic(0, "hello"), mic(2000, "world"), mic(5000, "again")];
        assert_eq!(
            mic_only_labeller(chunks, vec![seg(0, 10000, "A")]),
            "Speaker 1: hello world again"
        );
    }

    #[test]
    fn build_transcript_speaker_switch_inserts_newline_and_prefix() {
        let chunks = vec![
            mic(0, "First turn."),
            mic(3500, "Second turn."),
            mic(7000, "Third turn."),
        ];
        let segs = vec![seg(0, 3000, "A"), seg(3000, 6000, "B"), seg(6000, 9000, "A")];
        // Display numbers assigned in first-encounter order: A=1, B=2.
        // A returns later → "Speaker 1:" again, not a new number.
        assert_eq!(
            mic_only_labeller(chunks, segs),
            "Speaker 1: First turn.\nSpeaker 2: Second turn.\nSpeaker 1: Third turn."
        );
    }

    #[test]
    fn build_transcript_skips_empty_chunks() {
        let chunks = vec![mic(0, "real text"), mic(1000, "   "), mic(2000, "more")];
        assert_eq!(
            mic_only_labeller(chunks, vec![seg(0, 5000, "A")]),
            "Speaker 1: real text more"
        );
    }

    #[test]
    fn build_transcript_remote_call_mic_is_you_sys_is_diarized() {
        // Remote-call shape: mic chunks get fixed "You" label; sys chunks
        // get diarized. Ordering by (start_ms, source) interleaves them.
        let chunks = vec![
            mic(0, "Hi there."),
            sys(500, "Hello."),
            mic(2500, "How are you?"),
            sys(4000, "Doing well."),
        ];
        let sys_segs = vec![seg(0, 10000, "REMOTE_A")];
        let display_map = build_display_map(&chunks, &sys_segs, ChunkSource::Sys);
        let labeller = move |c: &ChunkRecord| match c.source {
            ChunkSource::Mic => Some("You".to_string()),
            ChunkSource::Sys => assign_speaker(c.start_ms, &sys_segs)
                .and_then(|sid| display_map.get(sid).map(|n| format!("Speaker {n}"))),
        };
        assert_eq!(
            build_labelled_transcript(&chunks, &whole_chunk_pieces(labeller)),
            "You: Hi there.\nSpeaker 1: Hello.\nYou: How are you?\nSpeaker 1: Doing well."
        );
    }

    /// Mic chunk with explicit chunk-relative word timings — mirrors
    /// `sys_with_words` for the stream that only started getting diarized
    /// in hybrid mode once `build_hybrid_labels` landed.
    fn mic_with_words(start_ms: u64, words: Vec<(&str, u64, u64)>) -> ChunkRecord {
        let mut c = sys_with_words(start_ms, words);
        c.source = ChunkSource::Mic;
        c
    }

    /// Build the splitter `diarize_and_apply`'s hybrid branch builds, so the
    /// end-to-end tests exercise the real labelling path without the sidecar.
    fn hybrid_splitter(
        chunks: &[ChunkRecord],
        mic_segments: Vec<Segment>,
        sys_segments: Vec<Segment>,
    ) -> impl Fn(&ChunkRecord) -> Vec<LabelledPiece> + Send {
        let labels = build_hybrid_labels(chunks, &mic_segments, &sys_segments);
        let sys_fallback = format!("Speaker {}", labels.next_free);
        let HybridLabels { mic, sys, .. } = labels;
        move |c: &ChunkRecord| match c.source {
            ChunkSource::Mic if mic_segments.is_empty() => {
                single_piece(c, Some("You".to_string()))
            }
            ChunkSource::Mic => split_by_labels(c, &mic_segments, &mic),
            ChunkSource::Sys if sys_segments.is_empty() => {
                single_piece(c, Some(sys_fallback.clone()))
            }
            ChunkSource::Sys => split_by_labels(c, &sys_segments, &sys),
        }
    }

    #[test]
    fn hybrid_in_person_meeting_with_one_stray_sys_chunk_still_separates_the_room() {
        // The regression this whole change exists for. A 45-minute in-person
        // meeting with three people, where something played through the system
        // output for a couple of seconds (a chime, a video). That single sys
        // chunk used to flip the branch to (mic+sys) = "remote call", skip the
        // mic diarize entirely, and hard-label all three people `You:` —
        // producing three transcript lines for 45 minutes of speech.
        let chunks = vec![
            mic(0, "Michael opens."),
            mic(20_000, "Stian answers."),
            sys(110_000, "Enhver likhet med virkelige hendelser er tilfeldig."),
            mic(120_000, "Petter weighs in."),
            mic(140_000, "Michael again."),
        ];
        let mic_segs = vec![
            seg(0, 15_000, "M"),
            seg(20_000, 30_000, "S"),
            seg(120_000, 130_000, "P"),
            seg(140_000, 150_000, "M"),
        ];
        let sys_segs = vec![seg(110_000, 112_000, "TV")];

        let labels = build_hybrid_labels(&chunks, &mic_segs, &sys_segs);
        assert_eq!(labels.mic.len(), 3, "three voices on the mic stay distinct");
        assert!(
            !labels.mic.values().any(|l| l == "You"),
            "a room of three is not one `You`"
        );

        // Numbering is chronological across the merged stream, so the sys blip
        // takes the number its position earns (3) and the third person in the
        // room follows it (4). What matters is that no two voices share one.
        assert_eq!(labels.mic.get("M").map(String::as_str), Some("Speaker 1"));
        assert_eq!(labels.mic.get("S").map(String::as_str), Some("Speaker 2"));
        assert_eq!(labels.sys.get("TV").map(String::as_str), Some("Speaker 3"));
        assert_eq!(labels.mic.get("P").map(String::as_str), Some("Speaker 4"));
        assert_eq!(labels.next_free, 5);

        let splitter = hybrid_splitter(&chunks, mic_segs, sys_segs);
        assert_eq!(
            build_labelled_transcript(&chunks, &splitter),
            "Speaker 1: Michael opens.\n\
             Speaker 2: Stian answers.\n\
             Speaker 3: Enhver likhet med virkelige hendelser er tilfeldig.\n\
             Speaker 4: Petter weighs in.\n\
             Speaker 1: Michael again."
        );
    }

    #[test]
    fn hybrid_lone_mic_voice_keeps_the_you_label() {
        // Solo remote call — the shape the `You` shortcut was built for. The
        // mic diarize resolves to one voice, so that voice is the user and
        // keeps `You`, and the remote side still numbers from 1.
        let chunks = vec![
            mic(0, "Hi there."),
            sys(500, "Hello."),
            mic(2500, "How are you?"),
            sys(4000, "Doing well."),
        ];
        let mic_segs = vec![seg(0, 10_000, "ME")];
        let sys_segs = vec![seg(0, 2000, "REMOTE_A"), seg(3500, 6000, "REMOTE_B")];

        let labels = build_hybrid_labels(&chunks, &mic_segs, &sys_segs);
        assert_eq!(labels.mic.get("ME").map(String::as_str), Some("You"));
        assert_eq!(
            labels.sys.get("REMOTE_A").map(String::as_str),
            Some("Speaker 1"),
            "remote side numbers from 1, not 2 — `You` doesn't consume a number"
        );
        assert_eq!(labels.sys.get("REMOTE_B").map(String::as_str), Some("Speaker 2"));
        assert_eq!(labels.next_free, 3);

        let splitter = hybrid_splitter(&chunks, mic_segs, sys_segs);
        assert_eq!(
            build_labelled_transcript(&chunks, &splitter),
            "You: Hi there.\nSpeaker 1: Hello.\nYou: How are you?\nSpeaker 2: Doing well."
        );
    }

    #[test]
    fn hybrid_echo_voice_is_dropped_before_the_two_room_voices_are_numbered() {
        // Two people in the room on a call played through the speakers. The
        // mic diarize heard the remote side's echo as a third voice.
        let take = crate::echo::tests::echoed_take(60.0, &[(0.0, 20.0), (30.0, 50.0)]);
        let chunks = vec![
            sys(0, "Remote opens the call."),
            mic(5_000, "Anna answers over it."),
            mic(20_500, "Anna again, in the pause."),
            sys(30_000, "Remote carries on."),
            mic(35_000, "Bjorn talks over the call."),
            mic(52_000, "Bjorn, in the pause."),
        ];
        let mic_segs = vec![
            seg(1_000, 4_000, "ECHO"),
            seg(5_000, 8_000, "ANNA"),
            seg(20_500, 25_000, "ANNA"),
            seg(31_000, 45_000, "ECHO"),
            seg(35_000, 40_000, "BJORN"),
            seg(52_000, 58_000, "BJORN"),
        ];
        let sys_segs = vec![seg(0, 20_000, "REMOTE"), seg(30_000, 50_000, "REMOTE")];

        let (kept, _) = take.check(mic_segs);
        let labels = build_hybrid_labels(&chunks, &kept, &sys_segs);
        assert_eq!(labels.mic.get("ECHO"), None, "the echo is no voice at all");
        assert_eq!(labels.mic.get("ANNA").map(String::as_str), Some("Speaker 2"));
        assert_eq!(labels.mic.get("BJORN").map(String::as_str), Some("Speaker 3"));
        assert_eq!(labels.sys.get("REMOTE").map(String::as_str), Some("Speaker 1"));
    }

    #[tokio::test]
    async fn the_cancelled_mic_lives_in_the_temp_dir_only_as_long_as_the_diarize() {
        // Audio under `recordings/` or `diagnostics/` would sit outside
        // `keep_audio` and "Delete stored audio".
        let wav = CancelledMicWav::write(&[3277; 1600]).await.expect("written");
        let path = wav.path().to_path_buf();
        assert!(path.starts_with(std::env::temp_dir()), "{}", path.display());
        assert!(!path.components().any(|c| c.as_os_str() == "no.humla.app"));
        assert_eq!(wav::read_i16_mono_16k(&path).await.unwrap(), vec![3277; 1600]);
        let other = CancelledMicWav::write(&[0; 16]).await.unwrap();
        assert_ne!(other.path(), path, "two diarizes never share one file");
        drop(wav);
        assert!(!path.exists(), "gone once the diarize is done with it");
    }

    #[test]
    fn hybrid_lone_user_earns_you_once_the_echo_voices_are_dropped() {
        // The user plus two voices made of echo.
        // Whisper transcribed the echo on the mic too, and the voices are
        // numbered before the text dedup drops those chunks — so each echo
        // voice is reached, and would cost the user `You`.
        let take = crate::echo::tests::echoed_take(60.0, &[(0.0, 20.0), (30.0, 50.0)]);
        let chunks = vec![
            sys(0, "The remote side opens the call now."),
            mic(1_000, "The remote side opens the call now."),
            mic(21_000, "I answer in the pause."),
            sys(30_000, "The remote side carries on talking."),
            mic(31_000, "The remote side carries on talking."),
            mic(52_000, "And once more."),
        ];
        let mic_segs = vec![
            seg(500, 19_000, "ECHO_A"),
            seg(21_000, 28_000, "ME"),
            seg(31_000, 49_000, "ECHO_B"),
            seg(52_000, 57_000, "ME"),
        ];
        let sys_segs = vec![seg(0, 20_000, "REMOTE"), seg(30_000, 50_000, "REMOTE")];
        assert_eq!(build_hybrid_labels(&chunks, &mic_segs, &sys_segs).mic.len(), 3);

        let (kept, _) = take.check(mic_segs);
        let labels = build_hybrid_labels(&chunks, &kept, &sys_segs);
        assert_eq!(labels.mic.len(), 1);
        assert_eq!(labels.mic.get("ME").map(String::as_str), Some("You"));
        let splitter = hybrid_splitter(&chunks, kept, sys_segs);
        assert_eq!(
            build_labelled_transcript(&chunks, &splitter),
            "Speaker 1: The remote side opens the call now.\n\
             You: I answer in the pause.\n\
             Speaker 1: The remote side carries on talking.\n\
             You: And once more."
        );
    }

    #[test]
    fn hybrid_speaker_numbers_never_collide_across_streams() {
        let chunks = vec![
            mic(0, "Room one."),
            mic(10_000, "Room two."),
            sys(20_000, "Remote one."),
            sys(30_000, "Remote two."),
        ];
        let mic_segs = vec![seg(0, 5000, "A"), seg(10_000, 15_000, "B")];
        let sys_segs = vec![seg(20_000, 25_000, "X"), seg(30_000, 35_000, "Y")];

        let labels = build_hybrid_labels(&chunks, &mic_segs, &sys_segs);
        let mut all: Vec<&String> = labels.mic.values().chain(labels.sys.values()).collect();
        all.sort();
        let before = all.len();
        all.dedup();
        assert_eq!(before, all.len(), "every speaker got a unique label: {all:?}");
        assert_eq!(all.len(), 4);
        assert_eq!(labels.next_free, 5);
    }

    #[test]
    fn hybrid_sys_fallback_number_clears_the_mic_speakers() {
        // Mic diarize succeeded with two voices, sys diarize produced nothing.
        // The sys fallback label must not reuse `Speaker 1` — that would merge
        // the system stream into a person who is actually in the room.
        let chunks = vec![
            mic(0, "Room one."),
            mic(10_000, "Room two."),
            sys(20_000, "Undiarized remote audio."),
        ];
        let mic_segs = vec![seg(0, 5000, "A"), seg(10_000, 15_000, "B")];

        let labels = build_hybrid_labels(&chunks, &mic_segs, &[]);
        assert_eq!(labels.mic.len(), 2);
        assert!(labels.sys.is_empty());
        assert_eq!(labels.next_free, 3, "fallback takes Speaker 3");

        let splitter = hybrid_splitter(&chunks, mic_segs, Vec::new());
        assert_eq!(
            build_labelled_transcript(&chunks, &splitter),
            "Speaker 1: Room one.\nSpeaker 2: Room two.\nSpeaker 3: Undiarized remote audio."
        );
    }

    #[test]
    fn hybrid_total_diarize_failure_matches_the_legacy_labels() {
        // Neither stream could be diarized. Degrade to exactly what the old
        // channel-attribution branch produced, so a diarize outage is no
        // worse than it was before: mic = You, sys = a distinct Speaker 1.
        let chunks = vec![
            mic(0, "Ok thanks."),
            sys(500, "You got it."),
            mic(2000, "See you tomorrow."),
        ];
        let labels = build_hybrid_labels(&chunks, &[], &[]);
        assert!(labels.mic.is_empty() && labels.sys.is_empty());
        assert_eq!(labels.next_free, 1);

        let splitter = hybrid_splitter(&chunks, Vec::new(), Vec::new());
        assert_eq!(
            build_labelled_transcript(&chunks, &splitter),
            "You: Ok thanks.\nSpeaker 1: You got it.\nYou: See you tomorrow."
        );
    }

    #[test]
    fn hybrid_mic_diarize_failure_alone_still_numbers_the_remote_side_from_one() {
        // mic.wav missing (sidecar SIGKILL'd before close) but sys diarized
        // fine — the old behaviour exactly, since there's no mic speaker to
        // push the remote numbering past.
        let chunks = vec![mic(0, "Mine."), sys(500, "Theirs."), sys(9000, "Other theirs.")];
        let sys_segs = vec![seg(0, 5000, "X"), seg(8000, 12_000, "Y")];

        let labels = build_hybrid_labels(&chunks, &[], &sys_segs);
        assert!(labels.mic.is_empty());
        assert_eq!(labels.sys.get("X").map(String::as_str), Some("Speaker 1"));
        assert_eq!(labels.sys.get("Y").map(String::as_str), Some("Speaker 2"));

        let splitter = hybrid_splitter(&chunks, Vec::new(), sys_segs);
        assert_eq!(
            build_labelled_transcript(&chunks, &splitter),
            "You: Mine.\nSpeaker 1: Theirs.\nSpeaker 2: Other theirs."
        );
    }

    #[test]
    fn hybrid_mic_chunk_splits_mid_chunk_between_two_people_in_the_room() {
        // Previously impossible: the hybrid branch wrapped every mic chunk in
        // `single_piece("You")`, so a 15-second VAD chunk holding a two-person
        // exchange in the room emitted a single line. Word timings now drive a
        // mid-chunk split on the mic stream the same way they do on sys.
        let chunks = vec![
            mic_with_words(
                0,
                vec![
                    ("Hva", 0, 500),
                    ("mener", 500, 1000),
                    ("du?", 1000, 1500),
                    ("Jeg", 6000, 6500),
                    ("er", 6500, 7000),
                    ("enig.", 7000, 7500),
                ],
            ),
            sys(20_000, "Chime."),
        ];
        let mic_segs = vec![seg(0, 3000, "A"), seg(5000, 9000, "B")];
        let sys_segs = vec![seg(20_000, 21_000, "T")];

        let labels = build_hybrid_labels(&chunks, &mic_segs, &sys_segs);
        assert_eq!(labels.mic.len(), 2, "two voices inside one mic chunk");

        let pieces = split_by_labels(&chunks[0], &mic_segs, &labels.mic);
        assert_eq!(pieces.len(), 2, "chunk split at the speaker boundary");
        assert_eq!(pieces[0].text, "Hva mener du?");
        assert_eq!(pieces[1].text, "Jeg er enig.");
        assert_ne!(pieces[0].label, pieces[1].label);
    }

    #[test]
    fn hybrid_segment_no_chunk_reaches_does_not_consume_a_number() {
        // Same rule `build_display_map` follows: a segment nobody spoke over
        // must not burn a display number, or the labels a user sees start at
        // `Speaker 2` for no visible reason.
        let chunks = vec![mic(0, "Only voice."), sys(50_000, "Remote.")];
        // "GHOST" covers 20-30s, where no chunk starts.
        let mic_segs = vec![seg(0, 10_000, "A"), seg(20_000, 30_000, "GHOST")];
        let sys_segs = vec![seg(50_000, 55_000, "X")];

        let labels = build_hybrid_labels(&chunks, &mic_segs, &sys_segs);
        assert_eq!(labels.mic.len(), 1, "GHOST never reached, so never numbered");
        // One reached mic voice → it's the user, and sys numbers from 1.
        assert_eq!(labels.mic.get("A").map(String::as_str), Some("You"));
        assert_eq!(labels.sys.get("X").map(String::as_str), Some("Speaker 1"));
    }

    #[test]
    fn incidental_sys_stream_gives_the_whole_head_count_to_the_mic() {
        // The K2 shape: 156 mic chunks, 1 sys chunk. The sys side is a chime,
        // not a participant, so it must not consume a speaker from the hint —
        // the room needs all three, because `withSpeakers(exactly:)` is the
        // only thing that stops VBx collapsing a dominant-speaker meeting onto
        // one cluster.
        let mut chunks: Vec<ChunkRecord> = (0..156).map(|i| mic(i * 15_000, "talk")).collect();
        chunks.push(sys(110_000, "chime"));

        assert_eq!(hybrid_sys_hint(Some(3), &chunks), None, "sys gets no hint");
        // Sys found nothing, so the mic keeps the full total.
        assert_eq!(mic_hint_after_sys(Some(3), &[]), Some(3));
    }

    #[test]
    fn real_remote_call_still_reserves_one_speaker_for_the_mic() {
        // A genuine call: the system stream carries a real share of the
        // conversation, so the old `n - 1` semantics hold and the mic ends up
        // forced to exactly the one voice that makes `You` correct.
        let chunks = vec![
            mic(0, "me"),
            sys(1_000, "them"),
            mic(2_000, "me"),
            sys(3_000, "them"),
        ];
        assert_eq!(hybrid_sys_hint(Some(3), &chunks), Some(2));
        let sys_segs = vec![seg(0, 5_000, "R1"), seg(5_000, 9_000, "R2")];
        assert_eq!(mic_hint_after_sys(Some(3), &sys_segs), Some(1));
    }

    #[test]
    fn hybrid_hints_are_none_without_a_note_head_count() {
        // No hint set on the note ("Auto") → nothing to route. This is the
        // state the K2 note was in, which is why VBx was left to guess.
        let chunks = vec![mic(0, "a"), sys(1_000, "b")];
        assert_eq!(hybrid_sys_hint(None, &chunks), None);
        assert_eq!(mic_hint_after_sys(None, &[]), None);
    }

    #[test]
    fn mic_hint_floors_at_one_speaker() {
        // Sys accounted for as many voices as the user expected (or more) —
        // never ask the diarizer for zero or a negative count.
        let sys_segs = vec![seg(0, 1_000, "A"), seg(1_000, 2_000, "B"), seg(2_000, 3_000, "C")];
        assert_eq!(mic_hint_after_sys(Some(3), &sys_segs), Some(1));
        assert_eq!(mic_hint_after_sys(Some(2), &sys_segs), Some(1));
    }

    #[test]
    fn hybrid_sys_hint_handles_no_chunks() {
        assert_eq!(hybrid_sys_hint(Some(3), &[]), None);
    }

    #[test]
    fn distinct_speaker_count_counts_voices_not_segments() {
        let segs = vec![
            seg(0, 1000, "A"),
            seg(1000, 2000, "B"),
            seg(2000, 3000, "A"),
            seg(3000, 4000, "C"),
        ];
        assert_eq!(distinct_speaker_count(&segs), 3);
        assert_eq!(distinct_speaker_count(&[]), 0);
    }

    #[test]
    fn max_speaker_number_finds_highest() {
        let text = "Speaker 1: hi\nSpeaker 2: hello\nSpeaker 1: again";
        assert_eq!(max_speaker_number(text), 2);
    }

    #[test]
    fn max_speaker_number_zero_when_no_labels() {
        assert_eq!(max_speaker_number("just plain text"), 0);
        assert_eq!(max_speaker_number("Michael: hi\nWilma: hello"), 0);
        assert_eq!(max_speaker_number(""), 0);
    }

    #[test]
    fn max_speaker_number_handles_multi_digit() {
        let text = "Speaker 1: hi\nSpeaker 12: hello";
        assert_eq!(max_speaker_number(text), 12);
    }

    #[test]
    fn offset_speaker_numbers_adds_offset() {
        let text = "Speaker 1: hi\nSpeaker 2: hello";
        assert_eq!(
            offset_speaker_numbers(text, 3),
            "Speaker 4: hi\nSpeaker 5: hello"
        );
    }

    #[test]
    fn offset_speaker_numbers_preserves_renamed() {
        // Only literal "Speaker N:" prefixes get rewritten; renamed lines
        // and free-text mentions stay untouched.
        let text = "Michael: hi\nWilma: hello\nSpeaker 1 was great";
        assert_eq!(offset_speaker_numbers(text, 5), text);
    }

    #[test]
    fn combine_with_empty_snapshot_passes_through() {
        let new = "Speaker 1: hi\nSpeaker 2: hello";
        assert_eq!(combine_with_snapshot("", new), new);
        assert_eq!(combine_with_snapshot("   \n  ", new), new);
    }

    #[test]
    fn combine_with_empty_new_returns_snapshot() {
        let snap = "Michael: prior content";
        assert_eq!(combine_with_snapshot(snap, ""), snap);
    }

    #[test]
    fn combine_offsets_new_session_speakers() {
        // Snapshot has Speaker 1 + Speaker 2; new session also numbers
        // from 1 — should be bumped to 3 + 4 to avoid collision.
        let snap = "Speaker 1: prior A\nSpeaker 2: prior B";
        let new = "Speaker 1: new A\nSpeaker 2: new B";
        assert_eq!(
            combine_with_snapshot(snap, new),
            "Speaker 1: prior A\nSpeaker 2: prior B\nSpeaker 3: new A\nSpeaker 4: new B"
        );
    }

    #[test]
    fn combine_no_offset_when_snapshot_uses_renamed() {
        // Snapshot only has renamed labels (no "Speaker N:") — offset is 0,
        // new session keeps its original numbering.
        let snap = "Michael: prior\nWilma: prior";
        let new = "Speaker 1: new";
        assert_eq!(
            combine_with_snapshot(snap, new),
            "Michael: prior\nWilma: prior\nSpeaker 1: new"
        );
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
    fn offset_speaker_label_bumps_numbered_only() {
        assert_eq!(offset_speaker_label("Speaker 1", 3), "Speaker 4");
        assert_eq!(offset_speaker_label("Speaker 12", 2), "Speaker 14");
        // Zero offset, non-numbered, and renamed labels pass through.
        assert_eq!(offset_speaker_label("Speaker 1", 0), "Speaker 1");
        assert_eq!(offset_speaker_label("You", 5), "You");
        assert_eq!(offset_speaker_label("Michael", 5), "Michael");
        assert_eq!(offset_speaker_label("", 5), "");
    }

    #[test]
    fn session_speaker_offset_matches_combine_offset() {
        // The offset baked into a session's timeline must equal the one
        // combine_with_snapshot applies to the DB transcript, so the reader
        // (rendered from concatenated timelines) and the chip strip agree.
        let snap = "Speaker 1: a\nSpeaker 2: b";
        assert_eq!(session_speaker_offset(snap), 2);
        assert_eq!(session_speaker_offset(""), 0);
        assert_eq!(session_speaker_offset("You: hi\nMichael: yo"), 0);
    }

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
mod unify_tests {
    use super::*;
    use crate::diarize::Segment;

    fn seg(start_ms: u64, end_ms: u64, sid: &str) -> Segment {
        Segment { start_ms, end_ms, speaker_id: sid.to_string() }
    }

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

    fn span(start_ms: u64, end_ms: u64, label: &str, source: Option<ChunkSource>) -> LabelSpan {
        LabelSpan { start_ms, end_ms, label: label.to_string(), source }
    }

    fn usess(
        id: &str,
        mode: SessionMode,
        chunks: Vec<ChunkRecord>,
        mic_offset_ms: u64,
        sys_offset_ms: u64,
        old_spans: Vec<LabelSpan>,
    ) -> UnifySession {
        UnifySession {
            session_id: id.to_string(),
            mode,
            chunks,
            mic_offset_ms,
            sys_offset_ms,
            old_spans,
        }
    }

    /// (label, text) per entry of a timeline JSONL string.
    fn entries_of(jsonl: &str) -> Vec<(String, String)> {
        jsonl
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                let v: serde_json::Value = serde_json::from_str(l).unwrap();
                (
                    v.get("label").and_then(|s| s.as_str()).unwrap().to_string(),
                    v.get("text").and_then(|s| s.as_str()).unwrap().to_string(),
                )
            })
            .collect()
    }

    /// Parse a timeline JSONL back into label spans, mirroring what the
    /// orchestrator reads off disk on the next run.
    fn spans_of(jsonl: &str) -> Vec<LabelSpan> {
        let values: Vec<serde_json::Value> = jsonl
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        spans_from_values(&values)
    }

    #[test]
    fn concat_offsets_accumulate_from_sample_counts() {
        // 16 kHz mono → 16 samples/ms. 16000 samples = 1000 ms.
        assert_eq!(concat_offsets_ms(&[16_000, 8_000, 4_000]), vec![0, 1_000, 1_500]);
        assert_eq!(concat_offsets_ms(&[]), Vec::<u64>::new());
        assert_eq!(concat_offsets_ms(&[500]), vec![0]);
    }

    #[test]
    fn session_mode_reflects_chunk_sources() {
        assert_eq!(session_mode(&[mic(0, "a")]), Some(SessionMode::MicOnly));
        assert_eq!(session_mode(&[sys(0, "a")]), Some(SessionMode::SysOnly));
        assert_eq!(
            session_mode(&[mic(0, "a"), sys(1, "b")]),
            Some(SessionMode::Hybrid)
        );
        assert_eq!(session_mode(&[]), None);
    }

    #[test]
    fn session_unifiable_requires_every_diarized_stream() {
        // Mic-only diarizes mic.wav; sys-only diarizes sys.wav; hybrid now
        // diarizes BOTH, so it needs both on disk. A hybrid session missing one
        // can't join the concat — `concat_wavs` fails the whole pass on an
        // unreadable input, and skipping it would misalign later sessions'
        // offsets — so it stays frozen with its existing labels.
        assert!(session_unifiable(SessionMode::MicOnly, true, false));
        assert!(!session_unifiable(SessionMode::MicOnly, false, true));
        assert!(session_unifiable(SessionMode::SysOnly, false, true));
        assert!(!session_unifiable(SessionMode::SysOnly, true, false));
        assert!(session_unifiable(SessionMode::Hybrid, true, true));
        assert!(!session_unifiable(SessionMode::Hybrid, false, true));
        assert!(!session_unifiable(SessionMode::Hybrid, true, false));
    }

    #[test]
    fn generated_label_detection() {
        assert!(is_generated_label("Speaker 1"));
        assert!(is_generated_label("Speaker 12"));
        assert!(is_generated_label("You"));
        assert!(is_generated_label(""));
        assert!(!is_generated_label("Michael"));
        assert!(!is_generated_label("Speaker"));
        assert!(!is_generated_label("Speaker x"));
        assert!(!is_generated_label("Speaker 1 (guest)"));
    }

    /// Two mic-only takes of the same 2-person conversation. The combined
    /// clustering sees voice A in both takes → ONE label across sessions
    /// (the bug this issue fixes: per-take clustering gave take 2's voice A
    /// a fresh number).
    #[test]
    fn same_voice_across_sessions_gets_one_label() {
        // Take 1 is 10 s (offset 0), take 2 starts at 10 000 in concat time.
        let s1 = usess(
            "a",
            SessionMode::MicOnly,
            vec![mic(0, "hello from voice one."), mic(6_000, "reply from voice two.")],
            0,
            0,
            Vec::new(),
        );
        let s2 = usess(
            "b",
            SessionMode::MicOnly,
            vec![mic(1_000, "voice one again in take two.")],
            10_000,
            0,
            Vec::new(),
        );
        // Combined-timeline segments: spk_a talks 0–5 s and 10.5–15 s,
        // spk_b talks 6–10 s.
        let segs = vec![
            seg(0, 5_000, "spk_a"),
            seg(6_000, 10_000, "spk_b"),
            seg(10_500, 15_000, "spk_a"),
        ];
        let out = unify_relabel(&[s1, s2], &segs, &[], 0);
        assert_eq!(out.timelines.len(), 2);
        assert!(out.notices.is_empty());

        let t1 = entries_of(&out.timelines[0].1);
        let t2 = entries_of(&out.timelines[1].1);
        assert_eq!(t1[0].0, "Speaker 1");
        assert_eq!(t1[1].0, "Speaker 2");
        // Take 2's chunk is voice A → same "Speaker 1", NOT "Speaker 3".
        assert_eq!(t2[0].0, "Speaker 1");

        // Times stay session-local: take 2's entry starts at 1000, not 11000.
        let v: serde_json::Value = serde_json::from_str(
            out.timelines[1].1.lines().next().unwrap(),
        )
        .unwrap();
        assert_eq!(v.get("start_ms").and_then(|s| s.as_u64()), Some(1_000));
    }

    #[test]
    fn hybrid_mic_is_diarized_and_unified_with_the_in_person_take() {
        // Take 1 in-person (mic-only), take 2 a remote call (hybrid). The
        // hybrid take's mic used to be hard-labelled `You`, which hid the fact
        // that it's the same person who spoke in take 1. Diarizing it means the
        // unify pass can recognise the voice and give both takes one label.
        // Numbering stays one sequence across streams in reading order.
        let s1 = usess(
            "a",
            SessionMode::MicOnly,
            vec![mic(0, "in person voice.")],
            0,
            0,
            Vec::new(),
        );
        let s2 = usess(
            "b",
            SessionMode::Hybrid,
            vec![mic(0, "me on the call."), sys(2_000, "remote person answering.")],
            0,
            0,
            Vec::new(),
        );
        let mic_segs = vec![seg(0, 5_000, "spk_m")];
        let sys_segs = vec![seg(1_500, 6_000, "spk_r")];
        let out = unify_relabel(&[s1, s2], &mic_segs, &sys_segs, 0);

        let t1 = entries_of(&out.timelines[0].1);
        let t2 = entries_of(&out.timelines[1].1);
        assert_eq!(t1[0].0, "Speaker 1");
        assert_eq!(
            t2[0].0, "Speaker 1",
            "one voice across both takes — not `You` on a note with an in-person take"
        );
        assert_eq!(t2[1].0, "Speaker 2");
    }

    #[test]
    fn all_hybrid_takes_with_a_lone_mic_voice_keep_you() {
        // Every take is a remote call and the combined mic stream is one voice:
        // that's the shape `You` was built for, so it survives the unify pass
        // and the remote side still numbers from 1.
        let s1 = usess(
            "a",
            SessionMode::Hybrid,
            vec![mic(0, "me first."), sys(2_000, "them first.")],
            0,
            0,
            Vec::new(),
        );
        let s2 = usess(
            "b",
            SessionMode::Hybrid,
            vec![mic(0, "me again."), sys(2_000, "them again.")],
            0,
            0,
            Vec::new(),
        );
        let mic_segs = vec![seg(0, 5_000, "spk_me")];
        let sys_segs = vec![seg(1_500, 6_000, "spk_them")];
        let out = unify_relabel(&[s1, s2], &mic_segs, &sys_segs, 0);

        let t1 = entries_of(&out.timelines[0].1);
        let t2 = entries_of(&out.timelines[1].1);
        assert_eq!(t1[0].0, "You");
        assert_eq!(t1[1].0, "Speaker 1", "`You` doesn't consume a number");
        assert_eq!(t2[0].0, "You");
        assert_eq!(t2[1].0, "Speaker 1");
    }

    #[test]
    fn hybrid_takes_with_several_mic_voices_number_the_room() {
        // The multi-session form of the in-person regression: a hybrid take
        // whose mic holds more than one person must not collapse them onto
        // `You`, even though every take is hybrid.
        let s1 = usess(
            "a",
            SessionMode::Hybrid,
            vec![
                mic(0, "first colleague."),
                mic(6_000, "second colleague."),
                sys(12_000, "remote voice."),
            ],
            0,
            0,
            Vec::new(),
        );
        let mic_segs = vec![seg(0, 4_000, "spk_a"), seg(5_000, 9_000, "spk_b")];
        let sys_segs = vec![seg(11_000, 15_000, "spk_r")];
        let out = unify_relabel(&[s1], &mic_segs, &sys_segs, 0);

        let t = entries_of(&out.timelines[0].1);
        assert_eq!(t[0].0, "Speaker 1");
        assert_eq!(t[1].0, "Speaker 2");
        assert_eq!(t[2].0, "Speaker 3");
        assert!(
            !t.iter().any(|e| e.0 == "You"),
            "two people sharing the mic are not one `You`"
        );
    }

    #[test]
    fn timeline_entries_carry_source() {
        let s = usess(
            "a",
            SessionMode::Hybrid,
            vec![mic(0, "me talking."), sys(3_000, "remote talking.")],
            0,
            0,
            Vec::new(),
        );
        let sys_segs = vec![seg(2_500, 6_000, "spk_r")];
        let out = unify_relabel(&[s], &[], &sys_segs, 0);
        let spans = spans_of(&out.timelines[0].1);
        assert_eq!(spans[0].source, Some(ChunkSource::Mic));
        assert_eq!(spans[1].source, Some(ChunkSource::Sys));
    }

    /// A user rename ("Michael") on take 1's cluster survives unification and
    /// spreads to take 2's chunks of the same voice.
    #[test]
    fn custom_rename_carries_onto_unified_cluster() {
        let s1 = usess(
            "a",
            SessionMode::MicOnly,
            vec![mic(0, "hello from michael.")],
            0,
            0,
            // Old timeline: the user renamed this span to "Michael".
            vec![span(0, 1_200, "Michael", Some(ChunkSource::Mic))],
        );
        let s2 = usess(
            "b",
            SessionMode::MicOnly,
            vec![mic(500, "michael again in take two.")],
            8_000,
            0,
            // Take 2 previously carried a generated (offset) label.
            vec![span(500, 2_000, "Speaker 2", Some(ChunkSource::Mic))],
        );
        let segs = vec![seg(0, 5_000, "spk_a"), seg(8_000, 12_000, "spk_a")];
        let out = unify_relabel(&[s1, s2], &segs, &[], 0);
        assert!(out.notices.is_empty());
        assert_eq!(entries_of(&out.timelines[0].1)[0].0, "Michael");
        assert_eq!(entries_of(&out.timelines[1].1)[0].0, "Michael");
    }

    /// Two different custom names collapse into one cluster: the one with
    /// more speech time wins and the merge is surfaced as a notice.
    #[test]
    fn custom_name_collision_keeps_longer_and_notices() {
        let s1 = usess(
            "a",
            SessionMode::MicOnly,
            vec![mic(0, "a long stretch of alice talking here.")],
            0,
            0,
            // ~2.4 s of "Alice".
            vec![span(0, 2_400, "Alice", Some(ChunkSource::Mic))],
        );
        let s2 = usess(
            "b",
            SessionMode::MicOnly,
            vec![mic(0, "short bob bit.")],
            10_000,
            0,
            // ~1.0 s of "Bob".
            vec![span(0, 1_000, "Bob", Some(ChunkSource::Mic))],
        );
        // One combined cluster spans both takes.
        let segs = vec![seg(0, 15_000, "spk_a")];
        let out = unify_relabel(&[s1, s2], &segs, &[], 0);
        assert_eq!(entries_of(&out.timelines[0].1)[0].0, "Alice");
        assert_eq!(entries_of(&out.timelines[1].1)[0].0, "Alice");
        assert_eq!(out.notices.len(), 1);
        assert!(out.notices[0].contains("Alice"), "{}", out.notices[0]);
        assert!(out.notices[0].contains("Bob"), "{}", out.notices[0]);
    }

    /// "You" is pipeline-generated, not a user rename — a hybrid take's old
    /// "You" entries must not rename a time-overlapping remote cluster.
    #[test]
    fn you_label_does_not_rename_sys_clusters() {
        let s = usess(
            "a",
            SessionMode::Hybrid,
            vec![mic(0, "me talking over them."), sys(200, "remote person talking.")],
            0,
            0,
            vec![
                span(0, 2_000, "You", Some(ChunkSource::Mic)),
                span(200, 2_200, "Speaker 1", Some(ChunkSource::Sys)),
            ],
        );
        let sys_segs = vec![seg(0, 6_000, "spk_r")];
        let out = unify_relabel(&[s], &[], &sys_segs, 0);
        let t = entries_of(&out.timelines[0].1);
        assert_eq!(t[0].0, "You");
        // Remote cluster keeps a generated number — not "You".
        assert_eq!(t[1].0, "Speaker 1");
    }

    /// A rename on the user's own line ("You" → "Michael") must follow the
    /// mic stream only; the time-overlapping remote cluster keeps its number.
    #[test]
    fn same_source_guard_stops_cross_stream_rename_leak() {
        let s = usess(
            "a",
            SessionMode::Hybrid,
            vec![mic(0, "me the renamed user."), sys(100, "remote person here.")],
            0,
            0,
            vec![
                // User renamed their own mic line to "Michael"; sys entry
                // overlaps it in wall time (crosstalk) but is another stream.
                span(0, 2_000, "Michael", Some(ChunkSource::Mic)),
                span(100, 2_100, "Speaker 1", Some(ChunkSource::Sys)),
            ],
        );
        let sys_segs = vec![seg(0, 6_000, "spk_r")];
        let out = unify_relabel(&[s], &[], &sys_segs, 0);
        let t = entries_of(&out.timelines[0].1);
        // Mic side: generated "You" inherits the user's rename.
        assert_eq!(t[0].0, "Michael");
        // Sys side: unaffected by the mic-stream rename.
        assert_eq!(t[1].0, "Speaker 1");
    }

    /// Old timelines written before the `source` field existed still carry
    /// renames (source unknown → match any stream).
    #[test]
    fn sourceless_old_spans_still_carry_renames() {
        let s1 = usess(
            "a",
            SessionMode::MicOnly,
            vec![mic(0, "hello from wilma.")],
            0,
            0,
            vec![span(0, 1_200, "Wilma", None)],
        );
        let s2 = usess(
            "b",
            SessionMode::MicOnly,
            vec![mic(0, "wilma again.")],
            5_000,
            0,
            Vec::new(),
        );
        let segs = vec![seg(0, 10_000, "spk_a")];
        let out = unify_relabel(&[s1, s2], &segs, &[], 0);
        assert_eq!(entries_of(&out.timelines[0].1)[0].0, "Wilma");
        assert_eq!(entries_of(&out.timelines[1].1)[0].0, "Wilma");
    }

    /// Frozen (non-unifiable) sessions keep their numbers; unified generated
    /// labels are offset past them. Custom names are never offset.
    #[test]
    fn label_offset_bumps_generated_numbers_past_frozen() {
        let s1 = usess(
            "a",
            SessionMode::MicOnly,
            vec![mic(0, "voice one talking."), mic(6_000, "voice two talking.")],
            0,
            0,
            Vec::new(),
        );
        let s2 = usess(
            "b",
            SessionMode::MicOnly,
            vec![mic(0, "voice one again.")],
            10_000,
            0,
            Vec::new(),
        );
        let segs = vec![
            seg(0, 5_000, "spk_a"),
            seg(6_000, 10_000, "spk_b"),
            seg(10_000, 14_000, "spk_a"),
        ];
        // A frozen first take already used Speaker 1 + Speaker 2.
        let out = unify_relabel(&[s1, s2], &segs, &[], 2);
        let t1 = entries_of(&out.timelines[0].1);
        let t2 = entries_of(&out.timelines[1].1);
        assert_eq!(t1[0].0, "Speaker 3");
        assert_eq!(t1[1].0, "Speaker 4");
        assert_eq!(t2[0].0, "Speaker 3");
    }

    /// Re-running the pass with its own output as the "old" timelines must
    /// reproduce byte-identical timelines (and stop re-noticing an already-
    /// resolved collision) — idempotency by construction.
    #[test]
    fn rerun_with_own_output_is_identical() {
        let chunks1 = vec![mic(0, "a long stretch of alice talking here.")];
        let chunks2 = vec![mic(0, "short bob bit."), mic(4_000, "second voice appears.")];
        let segs = vec![seg(0, 12_000, "spk_a"), seg(14_000, 20_000, "spk_b")];
        let make = |old1: Vec<LabelSpan>, old2: Vec<LabelSpan>| {
            unify_relabel(
                &[
                    usess("a", SessionMode::MicOnly, chunks1.clone(), 0, 0, old1),
                    usess("b", SessionMode::MicOnly, chunks2.clone(), 10_000, 0, old2),
                ],
                &segs,
                &[],
                0,
            )
        };
        // Run 1: collision between two customs → "Alice" wins, notice fires.
        let run1 = make(
            vec![span(0, 2_400, "Alice", Some(ChunkSource::Mic))],
            vec![span(0, 1_000, "Bob", Some(ChunkSource::Mic))],
        );
        assert_eq!(run1.notices.len(), 1);

        // Run 2: old = run 1's output. Same clustering → identical bytes,
        // and the collision is already resolved so no notice repeats.
        let run2 = make(spans_of(&run1.timelines[0].1), spans_of(&run1.timelines[1].1));
        assert_eq!(run1.timelines, run2.timelines);
        assert!(run2.notices.is_empty());

        // Run 3 keeps the fixpoint.
        let run3 = make(spans_of(&run2.timelines[0].1), spans_of(&run2.timelines[1].1));
        assert_eq!(run2.timelines, run3.timelines);
    }

    /// No-custom-names path is also a fixpoint from the first run.
    #[test]
    fn rerun_without_customs_is_identical() {
        let chunks1 = vec![mic(0, "voice one talking."), mic(6_000, "voice two talking.")];
        let chunks2 = vec![mic(1_000, "voice one again.")];
        let segs = vec![
            seg(0, 5_000, "spk_a"),
            seg(6_000, 10_000, "spk_b"),
            seg(10_500, 15_000, "spk_a"),
        ];
        let make = |old1: Vec<LabelSpan>, old2: Vec<LabelSpan>| {
            unify_relabel(
                &[
                    usess("a", SessionMode::MicOnly, chunks1.clone(), 0, 0, old1),
                    usess("b", SessionMode::MicOnly, chunks2.clone(), 10_000, 0, old2),
                ],
                &segs,
                &[],
                0,
            )
        };
        let run1 = make(Vec::new(), Vec::new());
        let run2 = make(spans_of(&run1.timelines[0].1), spans_of(&run1.timelines[1].1));
        assert_eq!(run1.timelines, run2.timelines);
        assert!(run2.notices.is_empty());
    }

    /// An unlabelled piece (empty label) must never gain a name through
    /// overlap with a custom span.
    #[test]
    fn empty_labels_never_gain_names() {
        let news = vec![vec![span(0, 2_000, "", Some(ChunkSource::Mic))]];
        let olds = vec![vec![span(0, 2_000, "Michael", Some(ChunkSource::Mic))]];
        let (map, notices) = custom_name_map(&news, &olds);
        assert!(map.is_empty());
        assert!(notices.is_empty());
    }

    #[test]
    fn max_speaker_in_timeline_reads_exact_labels() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("timeline.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"start_ms":0,"end_ms":1,"label":"Speaker 2","text":"a"}"#,
                "\n",
                r#"{"start_ms":1,"end_ms":2,"label":"Michael","text":"b"}"#,
                "\n",
                r#"{"start_ms":2,"end_ms":3,"label":"You","text":"c"}"#,
                "\n",
            ),
        )
        .unwrap();
        assert_eq!(max_speaker_in_timeline(&path), 2);
        assert_eq!(max_speaker_in_timeline(&tmp.path().join("absent.jsonl")), 0);
    }

    fn candidate(id: &str, index: u32, mode: SessionMode) -> UnifyCandidate {
        UnifyCandidate {
            entry: sessions::SessionEntry {
                id: id.to_string(),
                index,
                started_at: String::new(),
                duration_ms: 60_000,
                streams: Vec::new(),
                transcribed: true,
            },
            dir: PathBuf::new(),
            chunks: Vec::new(),
            mode,
        }
    }

    #[test]
    fn the_unify_pass_judges_each_hybrid_take_inside_its_own_span_and_never_a_mic_only_one() {
        // Take 1 (0–60 s) was in person, with Hege at the laptop. Take 2
        // (60–120 s) was a call on the speakers with Hege remote, so the mic
        // heard her only while sys sounded.
        let in_person = candidate("s1", 1, SessionMode::MicOnly);
        let call = candidate("s2", 2, SessionMode::Hybrid);
        let echoes = [
            EchoPass { analysis: None, cancelled: None, ms: None },
            EchoPass {
                analysis: Some(crate::echo::tests::echoed_take(60.0, &[(0.0, 20.0), (30.0, 50.0)])),
                cancelled: None,
                ms: Some(0),
            },
        ];
        let segments = vec![
            seg(1_000, 15_000, "HEGE"),
            seg(21_000, 29_000, "ME"),
            seg(61_000, 75_000, "HEGE"),
            seg(81_000, 89_000, "ME"),
            seg(92_000, 108_000, "HEGE"),
            seg(112_000, 118_000, "ME"),
        ];

        let (kept, dump) =
            drop_joined_echo(&[&in_person, &call], &echoes, &[0, 60_000], &segments);

        let hege: Vec<(u64, u64)> = kept
            .iter()
            .filter(|s| s.speaker_id == "HEGE")
            .map(|s| (s.start_ms, s.end_ms))
            .collect();
        assert_eq!(hege, vec![(1_000, 15_000)], "her echo goes, and only her echo");
        assert_eq!(kept.iter().filter(|s| s.speaker_id == "ME").count(), 3);
        assert_eq!(dump.len(), 1, "a mic-only take has no echo pass to record");
        assert_eq!((dump[0].session_id, dump[0].index), ("s2", 2));
        let check = dump[0].echo.check.as_ref().expect("the call was judged");
        assert!(check.voices.iter().any(|v| v.speaker_id == "HEGE" && v.dropped), "{check:?}");
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
