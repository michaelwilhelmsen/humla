use crate::db;
use crate::diarize;
use crate::local_whisper;
use crate::sessions;
use crate::wav;
use crate::recording::{ChunkRecord, ChunkSource, DiagnosticPayload, ErrorPayload, Inflight, Phase, RecordingStatus, SidecarEvent, TranscriptPayload};
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
// so the `commands::<name>` paths in lib.rs's generate_handler! stay unchanged.
// Shared helpers + constants (err, DEFAULT_*, emit_*, key/config readers,
// transcript post-processing) stay in this root module and are reached from
// submodules via `super::` / `crate::`.
mod api_keys;
mod assets;
mod chat;
mod clients;
// `pub(crate)` so `crate::menubar` can resolve the active workspace when it
// creates a note for a headless recording.
pub(crate) mod cloud;
mod export;
// Live cloud-sync worker glue. Behind the `cloud` feature; `pub` so `run()` can
// reach `cloud_worker::install`. Not a #[tauri::command] group, so no glob
// re-export below.
#[cfg(feature = "cloud")]
pub mod cloud_worker;
mod folders;
mod local_llm;
mod mcp;
mod menubar;
mod models;
mod notes;
mod permissions;
mod settings;
mod summary;
mod summary_prompts;
mod telemetry;
mod transcription_config;
pub use api_keys::*;
pub use assets::*;
pub use chat::*;
pub use clients::*;
pub use cloud::*;
pub use export::*;
pub use folders::*;
pub use local_llm::*;
pub use mcp::*;
pub use menubar::*;
pub use models::*;
pub use notes::*;
pub use permissions::*;
pub use settings::*;
pub use summary::*;
pub use telemetry::*;
pub use summary_prompts::*;
pub use transcription_config::*;

// Shared helpers that moved into submodules but are still called by the
// recording / transcribe / summary code remaining in this root module.
use api_keys::read_provider_api_key;
use models::local_model_path;
use transcription_config::read_transcribe_config;

pub(crate) const DEFAULT_LANGUAGE: &str = "no";
// Default diarization engine. community1 = FluidAudio's
// OfflineDiarizerManager (the path we shipped through v0.11.0). Existing
// installs keep this transparently. Users who hit the rapid-turn ceiling
// can switch to "sortformer" in Settings → Transcription → Speaker
// diarization. Both engines coexist; the user has to download whichever
// they want before recording.
const DEFAULT_DIARIZE_MODEL: &str = "community1";

// Diarizer threshold defaults — match the sidecar's hardcoded values so
// "default" in settings produces the same behaviour as the original
// fixed-knob releases. Users tweak these in Settings → Transcription
// → Speaker diarization → Advanced when iterating on recordings the
// stock thresholds get wrong. Stored as strings (settings table is
// string-keyed); parsed at use site.
const DEFAULT_COMMUNITY1_THRESHOLD: &str = "0.5";
const DEFAULT_SORTFORMER_SILENCE_THRESHOLD: &str = "0.5";
const DEFAULT_SORTFORMER_PRED_THRESHOLD: &str = "0.25";
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
        {
            let state: State<AppState> = app.state();
            let engine = active_diarize_engine(&state);
            match diarize::status(&app, engine).await {
                Ok(s) if s.downloaded => {}
                _ => {
                    return Err(
                        "Diarize model isn't downloaded. Download it in Settings → Transcription → Speaker diarization, then try again."
                            .to_string(),
                    );
                }
            }
        }
        emit_status(&app, Some(&note_id), Phase::Diarizing);
        match unify_note_speakers(&app, &note_id).await {
            Ok(true) => {
                emit_status(&app, None, Phase::Idle);
                return Ok(());
            }
            Ok(false) => {
                // Not enough retained per-session audio to unify — fall
                // through to the latest-take re-diarize.
                emit_status(&app, None, Phase::Idle);
            }
            Err(e) => {
                emit_status(&app, None, Phase::Idle);
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

    let state: State<AppState> = app.state();
    let engine = active_diarize_engine(&state);
    let thresholds = read_diarize_thresholds(&state);
    let expected_speakers = {
        let conn = state.db.lock();
        db::get_note(&conn, &note_id)
            .ok()
            .and_then(|n| n.expected_speakers)
            .filter(|n| *n > 0)
    };

    match diarize::status(&app, engine).await {
        Ok(s) if s.downloaded => {}
        _ => {
            return Err(
                "Diarize model isn't downloaded. Download it in Settings → Transcription → Speaker diarization, then try again."
                    .to_string(),
            );
        }
    }

    rediarize_apply_to_chunks(
        app,
        note_id,
        session_id,
        mic_wav,
        sys_wav,
        chunks,
        expected_speakers,
        engine,
        thresholds,
    )
    .await
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

/// Mirror of diarize_and_apply's branching, but operating on caller-
/// supplied paths + chunks instead of recording-session state. No
/// snapshot — the transcript is being rebuilt from scratch from the
/// chunk timings, not appended to an in-flight session.
async fn rediarize_apply_to_chunks(
    app: AppHandle,
    note_id: String,
    session_id: String,
    mic_wav: Option<PathBuf>,
    sys_wav: Option<PathBuf>,
    chunks: Vec<ChunkRecord>,
    expected_speakers: Option<i64>,
    engine: diarize::Engine,
    thresholds: diarize::Thresholds,
) -> anyhow::Result<()> {
    // Before deciding the capture mode — a hallucinated chunk on an otherwise
    // silent stream would misclassify the whole recording. Also cleans up notes
    // recorded before the transcribe-time guard existed, which is the point of
    // running it here rather than trusting the chunk log.
    let chunks = drop_incidental_stream_hallucinations(chunks);
    if chunks.is_empty() {
        emit_status(&app, None, Phase::Idle);
        return Err(anyhow::anyhow!("no usable chunks after dropping collapsed ones"));
    }
    let mic_chunks_present = chunks.iter().any(|c| c.source == ChunkSource::Mic);
    let sys_chunks_present = chunks.iter().any(|c| c.source == ChunkSource::Sys);

    emit_status(&app, Some(&note_id), Phase::Diarizing);

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
    }
    let stage: DiarizeStage = match (mic_chunks_present, sys_chunks_present) {
        (true, false) => {
            let Some(wav) = mic_wav.clone() else {
                emit_status(&app, None, Phase::Idle);
                return Err(anyhow::anyhow!(
                    "mic chunks present but no saved mic.wav"
                ));
            };
            let segments =
                diarize_and_maybe_clean(&app, &wav, expected_speakers, engine, thresholds).await?;
            if segments.is_empty() {
                // No splitter to build → write the bare diagnostic (no
                // pieces) before bailing so the user can still inspect
                // "diarize ran but found nothing".
                write_diagnostics_json(
                    &app, &note_id, engine, "mic", &segments, &[], &chunks, &thresholds, None,
                    None,
                )
                .await;
                emit_status(&app, None, Phase::Idle);
                return Err(anyhow::anyhow!("diarize returned no segments"));
            }
            let segments_for_dump = segments.clone();
            let display_map = build_display_map(&chunks, &segments, ChunkSource::Mic);
            DiarizeStage {
                splitter: Box::new(move |c: &ChunkRecord| {
                    split_by_segments(c, &segments, &display_map)
                }),
                mic_segments_for_dump: segments_for_dump,
                sys_segments_for_dump: Vec::new(),
                source_tag: "mic",
            }
        }
        (false, true) => {
            let Some(wav) = sys_wav.clone() else {
                emit_status(&app, None, Phase::Idle);
                return Err(anyhow::anyhow!(
                    "sys chunks present but no saved sys.wav"
                ));
            };
            let segments =
                diarize_and_maybe_clean(&app, &wav, expected_speakers, engine, thresholds).await?;
            if segments.is_empty() {
                write_diagnostics_json(
                    &app, &note_id, engine, "sys", &[], &segments, &chunks, &thresholds, None,
                    None,
                )
                .await;
                emit_status(&app, None, Phase::Idle);
                return Err(anyhow::anyhow!("diarize returned no segments"));
            }
            let segments_for_dump = segments.clone();
            let display_map = build_display_map(&chunks, &segments, ChunkSource::Sys);
            DiarizeStage {
                splitter: Box::new(move |c: &ChunkRecord| {
                    split_by_segments(c, &segments, &display_map)
                }),
                mic_segments_for_dump: Vec::new(),
                sys_segments_for_dump: segments_for_dump,
                source_tag: "sys",
            }
        }
        (true, true) => {
            // Hybrid: diarize both streams and number them off one counter.
            // `You` is kept only when the mic resolves to a single voice —
            // see `build_hybrid_labels`. Mirrors `diarize_and_apply`'s
            // hybrid branch; change both together.
            // System stream first, mic second with the remaining head-count —
            // see `hybrid_sys_hint` / `mic_hint_after_sys` for why the hint is
            // worth more on the mic. Errors are logged rather than swallowed:
            // an empty result here is indistinguishable from "diarize refused",
            // and that ambiguity cost a debugging round.
            let sys_speaker_hint = hybrid_sys_hint(expected_speakers, &chunks);
            let sys_segments = if let Some(p) = sys_wav.as_ref() {
                diarize_and_maybe_clean(&app, p, sys_speaker_hint, engine, thresholds)
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
                diarize_and_maybe_clean(&app, p, mic_speaker_hint, engine, thresholds)
                    .await
                    .unwrap_or_else(|e| {
                        eprintln!("rediarize: mic diarize failed ({e}), mic falls back to You");
                        Vec::new()
                    })
            } else {
                eprintln!("rediarize hybrid: no saved mic.wav, mic falls back to You");
                Vec::new()
            };
            eprintln!(
                "rediarize hybrid: mic resolved to {} voice(s)",
                distinct_speaker_count(&mic_segments)
            );
            let mic_segments_for_dump = mic_segments.clone();
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
            }
        }
        (false, false) => {
            emit_status(&app, None, Phase::Idle);
            return Err(anyhow::anyhow!("no chunks recorded for either source"));
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
        Some(&pieces_pre),
        Some(&pieces_post),
    )
    .await;

    let split_chunk = stage.splitter;
    let new_transcript = build_labelled_transcript(&chunks, split_chunk.as_ref());
    if new_transcript.trim().is_empty() {
        emit_status(&app, None, Phase::Idle);
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
    write_playback_assets(
        &app,
        &note_id,
        &session_id,
        timeline,
        mic_wav.as_deref(),
        sys_wav.as_deref(),
    )
    .await;

    // Through the shared commit, so re-diarize inherits its refusal to trade a
    // non-empty transcript for an empty projection rather than restating it.
    let full_transcript = rebuild_note_transcript(&app, &note_id).map_err(|e| anyhow::anyhow!(e))?;
    if let Err(e) = commit_rebuilt_transcript(&app, &note_id, full_transcript) {
        emit_status(&app, None, Phase::Idle);
        return Err(anyhow::anyhow!(e));
    }
    // Re-diarize rewrote this session's timeline.jsonl — re-push the metadata
    // (idempotent) so the record exists; the frontend re-uploads the timeline
    // asset after the command returns.
    session_changed_for_sync(&app, &note_id, &session_id);

    emit_status(&app, None, Phase::Idle);
    Ok(())
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

/// One word's text + millisecond bounds in stream-absolute time.
/// Drives the playback view's karaoke-style word highlight.
#[derive(Clone, serde::Serialize)]
pub struct TimelineWord {
    pub text: String,
    pub start_ms: u64,
    pub end_ms: u64,
}

/// Per-note transcript timeline used to drive playback highlighting.
/// One entry per chunk (~5–15 s VAD-bounded utterance); the frontend
/// renders each as its own row so the active-chunk highlight can
/// follow the audio at sentence granularity. `words` may be empty for
/// chunks whose provider didn't expose token timestamps (gpt-4o
/// transcribe family), in which case the UI degrades to chunk-level
/// highlight. `end_ms` lets the player render overlapping mic+sys
/// chunks as both-active simultaneously instead of greedily switching
/// to whichever started most recently — older timelines (pre-end_ms)
/// fall back to start-only behavior on read.
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineEntry {
    pub start_ms: u64,
    pub end_ms: u64,
    pub label: String,
    pub text: String,
    pub words: Vec<TimelineWord>,
    // Which session (persisted recording) this entry belongs to, and its
    // 0-based line index *within that session's* timeline.jsonl. `note_timeline`
    // concatenates every session's timeline into one merged document (so the
    // reader never hides text), and the per-chunk edit IPCs
    // (set-label / delete) route back to the right session file via these.
    // `start_ms` / `end_ms` / word times stay session-*local* — the player
    // loads one session's playback.wav at a time and matches only that
    // session's entries against the playhead.
    pub session_id: String,
    pub session_index: u32,
    pub chunk_idx: usize,
}

/// Parse one session's `timeline.jsonl` into entries carrying that session's
/// identity. Skips malformed lines; backfills a usable `end_ms` for legacy
/// entries (pre-`end_ms`) by stretching to the next entry's start (or +5 s
/// for the last). Times stay session-*local* — one file is one session's own
/// playback timeline.
fn parse_session_timeline(
    path: &std::path::Path,
    session_id: &str,
    session_index: u32,
) -> Vec<TimelineEntry> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (idx, line) in content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
    {
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("timeline: skip malformed line: {e}");
                continue;
            }
        };
        let words = v
            .get("words")
            .and_then(|w| w.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|w| {
                        let text = w.get("text").and_then(|s| s.as_str())?.to_string();
                        let start_ms = w.get("start_ms").and_then(|s| s.as_u64())?;
                        let end_ms = w.get("end_ms").and_then(|s| s.as_u64())?;
                        Some(TimelineWord { text, start_ms, end_ms })
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.push(TimelineEntry {
            start_ms: v.get("start_ms").and_then(|s| s.as_u64()).unwrap_or(0),
            end_ms: v.get("end_ms").and_then(|s| s.as_u64()).unwrap_or(0),
            label: v.get("label").and_then(|s| s.as_str()).unwrap_or("").to_string(),
            text: v.get("text").and_then(|s| s.as_str()).unwrap_or("").to_string(),
            words,
            session_id: session_id.to_string(),
            session_index,
            chunk_idx: idx,
        });
    }
    for i in 0..out.len() {
        if out[i].end_ms <= out[i].start_ms {
            let fallback = out
                .iter()
                .skip(i + 1)
                .map(|e| e.start_ms)
                .find(|&s| s > out[i].start_ms)
                .unwrap_or_else(|| out[i].start_ms.saturating_add(5_000));
            out[i].end_ms = fallback;
        }
    }
    out
}

/// Merged timeline for a note: every session's `timeline.jsonl` concatenated
/// in manifest order, each entry tagged with its session id/index and its
/// local chunk index. Legacy flat notes resolve to a single session. Empty
/// for older notes / failed reads — the frontend treats "no timeline" as "no
/// highlighting available" and renders the plain transcript instead.
///
/// This is the field-report reader fix (#16): the styled reader renders the
/// FULL merged transcript across every take; the player only karaoke-matches
/// the *active* session's entries against its loaded playback.wav, and never
/// hides text from sessions it isn't currently playing.
#[tauri::command]
pub fn note_timeline(app: AppHandle, note_id: String) -> Result<Vec<TimelineEntry>, String> {
    let app_dir = app.path().app_data_dir().map_err(err)?;
    let recordings = sessions::recordings_dir(&app_dir, &note_id);
    let mut out = Vec::new();
    for (entry, dir) in sessions::resolve_sessions(&recordings) {
        out.extend(parse_session_timeline(
            &dir.join("timeline.jsonl"),
            &entry.id,
            entry.index,
        ));
    }
    Ok(out)
}

/// Outcome of [`note_timeline_repair`].
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineRepair {
    /// A synthesized session was written for text no timeline accounted for.
    pub repaired: bool,
    /// Whether the merged timelines now account for every word of
    /// `note.transcript`. When false the reader must fall back to the plain
    /// labelled view — the turn list would hide the difference.
    pub covers_transcript: bool,
}

/// Detect and repair transcript text that no session timeline accounts for
/// (#169), then report whether the note still has a gap.
///
/// Called on note open, which is deliberately the whole migration (ADR-0004):
/// the merged timeline is parsed and in memory at that point, so detection is
/// a comparison over data we already have rather than new I/O, and all four
/// paths that rebuild the transcript from the timelines — cycling a chunk's
/// speaker label, deleting a chunk, re-diarizing, unifying sessions — are UI
/// actions on an already-open note, so the repair always precedes them. That
/// ordering is the point: before it, any of those four rewrote
/// `note.transcript` to the projection and permanently deleted the orphaned
/// text, taking it out of the summary, chat retrieval and embeddings.
///
/// Idempotent: after a repair the projection covers the transcript, so the
/// next open finds nothing to do.
///
/// A note with *no* timeline at all is left alone. It renders the plain
/// textarea, so it hides nothing, and synthesizing a session for it would
/// take its free-text editing away; `commit_rebuilt_transcript` is what
/// protects those from the destructive paths.
#[tauri::command]
pub async fn note_timeline_repair(
    app: AppHandle,
    note_id: String,
) -> Result<TimelineRepair, String> {
    let clean = TimelineRepair { repaired: false, covers_transcript: true };
    let transcript = {
        let state: State<AppState> = app.state();
        let conn = state.db.lock();
        db::get_note(&conn, &note_id).map_err(err)?.transcript
    };
    if transcript.trim().is_empty() {
        return Ok(clean);
    }
    let projection = rebuild_note_transcript(&app, &note_id)?;
    if projection.trim().is_empty() {
        return Ok(clean); // no timeline at all — not this issue's shape
    }
    if projection_covers(&transcript, &projection) {
        return Ok(clean);
    }
    let Some(lines) = orphaned_prefix(&transcript, &projection) else {
        // Something diverged that a prefix repair can't explain (a timeline
        // present but short because malformed lines were skipped, an asset
        // that never downloaded). Leave it to the render-time guard, which
        // shows all the text rather than guessing at its shape.
        return Ok(TimelineRepair { repaired: false, covers_transcript: false });
    };
    let jsonl = synthesize_orphan_timeline(&lines);

    let app_dir = app.path().app_data_dir().map_err(err)?;
    let recordings = sessions::recordings_dir(&app_dir, &note_id);
    let session_id = repair_session_id(&note_id, &lines);
    let write_id = session_id.clone();
    // Serialize the manifest read-modify-write against the cloud pull worker
    // and the post-stop append, exactly as the other manifest writers do.
    let lock = app.state::<AppState>().manifest_lock.clone();
    let _manifest_guard = lock.lock().await;
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        // A legacy flat note has no manifest, so its take only resolves via
        // the flat fallback — which `resolve_sessions` stops applying the
        // moment a manifest exists. Migrate it into a real entry first, or
        // writing ours would hide the take we are repairing around.
        sessions::migrate_flat_if_needed(&recordings, &uuid::Uuid::new_v4().to_string())?;
        let dir = sessions::session_dir(&recordings, &write_id);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("timeline.jsonl"), jsonl)?;
        let mut manifest =
            sessions::read_manifest(&recordings).unwrap_or_else(sessions::SessionsManifest::empty);
        // Index 0: the orphaned text predates every recorded take, and
        // sessions resolve in index order. Existing indices are left alone so
        // a concurrent sync reconcile has nothing to disagree with, and
        // `append_session` (max + 1) is unaffected.
        //
        // Replace rather than append: the id is derived, so a manifest pulled
        // from a device that already repaired this note carries the same entry,
        // and pushing a second copy of it would project the orphan twice.
        let entry = sessions::SessionEntry {
            id: write_id.clone(),
            index: 0,
            started_at: String::new(),
            duration_ms: 0,
            streams: Vec::new(),
        };
        match manifest.sessions.iter_mut().find(|e| e.id == write_id) {
            Some(existing) => *existing = entry,
            None => manifest.sessions.push(entry),
        }
        sessions::write_manifest(&recordings, &manifest)
    })
    .await
    .map_err(err)?
    .map_err(|e| format!("repair timeline: {e}"))?;

    session_changed_for_sync(&app, &note_id, &session_id);
    // Re-project rather than assume: the reader's fallback is driven by this
    // answer, and claiming a coverage the files don't actually have is how the
    // text would stay hidden. Cheap — only a repair that happened pays for it.
    let repaired_projection = rebuild_note_transcript(&app, &note_id)?;
    Ok(TimelineRepair {
        repaired: true,
        covers_transcript: projection_covers(&transcript, &repaired_projection),
    })
}

/// Session metadata for a note, in recording order. Drives the playback
/// carousel (numbered pills + per-session date/duration/streams) and the
/// styled reader's session dividers. Empty for notes with no recordings.
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteSession {
    pub id: String,
    pub index: u32,
    pub started_at: String,
    pub duration_ms: u64,
    pub streams: Vec<String>,
    /// Whether this session has a playable `playback.wav` on disk.
    pub has_playback: bool,
}

/// List a note's recording sessions (see [`NoteSession`]).
#[tauri::command]
pub fn note_sessions(app: AppHandle, note_id: String) -> Result<Vec<NoteSession>, String> {
    let app_dir = app.path().app_data_dir().map_err(err)?;
    let recordings = sessions::recordings_dir(&app_dir, &note_id);
    let out = sessions::resolve_sessions(&recordings)
        .into_iter()
        .map(|(e, dir)| NoteSession {
            id: e.id,
            index: e.index,
            started_at: e.started_at,
            duration_ms: e.duration_ms,
            streams: e.streams,
            has_playback: dir.join("playback.wav").exists(),
        })
        .collect();
    Ok(out)
}

/// Path to a specific session's `playback.wav`, or `None` when absent. The
/// frontend feeds this through `convertFileSrc` into the `<audio>` element
/// when the user switches the carousel to that session.
#[tauri::command]
pub fn note_session_playback_path(
    app: AppHandle,
    note_id: String,
    session_id: String,
) -> Result<Option<String>, String> {
    let app_dir = app.path().app_data_dir().map_err(err)?;
    let recordings = sessions::recordings_dir(&app_dir, &note_id);
    let Some(dir) = sessions::resolve_session_dir(&recordings, &session_id) else {
        return Ok(None);
    };
    let path = dir.join("playback.wav");
    if !path.exists() {
        return Ok(None);
    }
    Ok(path.to_str().map(|s| s.to_string()))
}

/// Parse a `timeline.jsonl` into raw JSON values, skipping blank/malformed
/// lines. Shared by the per-chunk edit commands.
fn read_timeline_values(path: &std::path::Path) -> Vec<serde_json::Value> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .collect()
}

/// Write timeline values back as JSONL (one object per line).
fn write_timeline_values(path: &std::path::Path, values: &[serde_json::Value]) -> Result<(), String> {
    let mut out = String::new();
    for v in values {
        out.push_str(&v.to_string());
        out.push('\n');
    }
    std::fs::write(path, out).map_err(|e| format!("write timeline: {e}"))
}

/// Group one session's timeline values into transcript lines: consecutive
/// same-label entries join with a space; a label change starts a new line
/// prefixed `Label: `. Mirrors `build_labelled_transcript`. Returns the
/// session's contribution with no trailing newline.
fn group_values_to_transcript(values: &[serde_json::Value]) -> String {
    let mut out = String::new();
    let mut last_label: Option<String> = None;
    for v in values {
        let label = v.get("label").and_then(|s| s.as_str()).unwrap_or("").to_string();
        let text = v.get("text").and_then(|s| s.as_str()).unwrap_or("").trim();
        if text.is_empty() {
            continue;
        }
        if last_label.as_deref() != Some(label.as_str()) {
            if !out.is_empty() {
                out.push('\n');
            }
            if !label.is_empty() {
                out.push_str(&format!("{label}: "));
            }
            last_label = Some(label);
        } else {
            out.push(' ');
        }
        out.push_str(text);
    }
    out
}

/// Rebuild the note's full DB transcript from every session's timeline, in
/// manifest order, joined by newlines. Because each session's timeline stores
/// its speaker labels *already offset* (see `serialize_timeline`), the plain
/// concatenation reproduces the same labelled transcript
/// `combine_with_snapshot` wrote — so a per-chunk edit in one session never
/// clobbers the others.
fn rebuild_note_transcript(app: &AppHandle, note_id: &str) -> Result<String, String> {
    let app_dir = app.path().app_data_dir().map_err(err)?;
    let recordings = sessions::recordings_dir(&app_dir, note_id);
    let mut parts = Vec::new();
    for (_, dir) in sessions::resolve_sessions(&recordings) {
        let values = read_timeline_values(&dir.join("timeline.jsonl"));
        let part = group_values_to_transcript(&values);
        if !part.trim().is_empty() {
            parts.push(part);
        }
    }
    Ok(parts.join("\n"))
}

/// Split a transcript line into its speaker label and the words after it,
/// mirroring the reader's parse (`parseTranscriptLines` in `Note.tsx`): an
/// unbroken run of at most 40 non-colon characters, then `": "`. Anything
/// else is an unlabelled line.
fn split_label(line: &str) -> (Option<&str>, &str) {
    let trimmed = line.trim();
    let Some((label, rest)) = trimmed.split_once(": ") else {
        return (None, trimmed);
    };
    if label.is_empty() || label.chars().count() > 40 || label.contains(':') {
        return (None, trimmed);
    }
    (Some(label), rest)
}

/// A transcript reduced to comparable words: speaker labels dropped, case and
/// punctuation folded away. This is how the transcript and the timelines'
/// projection are compared — never by line count, because the two grouping
/// rules differ on purpose (Rust groups on label, `groupTimeline` on label
/// *and* session), and never by raw text, because a cross-note speaker rename
/// rewrites the transcript and leaves the timeline alone.
fn comparable_words(transcript: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in transcript.lines() {
        let (_, rest) = split_label(line);
        for word in rest.split_whitespace() {
            let w: String = word
                .chars()
                .filter(|c| c.is_alphanumeric())
                .flat_map(|c| c.to_lowercase())
                .collect();
            if !w.is_empty() {
                out.push(w);
            }
        }
    }
    out
}

/// Whether the timelines' projection accounts for every word of the
/// transcript, in order.
///
/// Deliberately directional: the reader renders the timelines, so a projection
/// carrying *more* than the transcript hides nothing and needs no fallback —
/// only words the transcript has and the projection lacks are invisible. A
/// subsequence test says exactly that, and it is insensitive to the two
/// grouping rules disagreeing about where lines break (they do, on purpose).
fn projection_covers(transcript: &str, projection: &str) -> bool {
    let pw = comparable_words(projection);
    let mut p = pw.iter();
    comparable_words(transcript)
        .iter()
        .all(|w| p.any(|c| c == w))
}

/// The leading transcript lines that `projection` — what the note's session
/// timelines can account for — does not cover.
///
/// `combine_with_snapshot` prepends the prior take's transcript to the new
/// one, so orphaned text is always a *prefix*: the projection is a suffix of
/// the transcript in word order. Returns `None` when nothing is orphaned, or
/// when the gap can't be attributed to a whole run of leading lines — the
/// render-time guard covers those shapes rather than guessing at a repair.
fn orphaned_prefix<'a>(transcript: &'a str, projection: &str) -> Option<Vec<&'a str>> {
    let tw = comparable_words(transcript);
    let pw = comparable_words(projection);
    if pw.is_empty() || tw.len() <= pw.len() {
        return None;
    }
    let mut missing = tw.len() - pw.len();
    if tw[missing..] != pw[..] {
        return None; // not a clean prefix gap — something else diverged
    }
    let mut out = Vec::new();
    for line in transcript.lines() {
        if missing == 0 {
            break;
        }
        let n = comparable_words(line).len();
        if n > missing {
            return None; // the gap ends mid-line; don't split a turn to fit
        }
        missing -= n;
        out.push(line);
    }
    out.retain(|l| !l.trim().is_empty());
    if missing > 0 || out.is_empty() {
        return None;
    }
    Some(out)
}

/// Milliseconds each synthesized entry occupies. Arbitrary: a repaired
/// session has no audio, so the bounds only have to be ordered and distinct.
const SYNTHESIZED_ENTRY_MS: u64 = 5_000;

/// UUID namespace for the ids repair-on-open mints. **Fixed forever** — change
/// it and two app versions stop agreeing on the id, which is the whole point.
const REPAIR_SESSION_NAMESPACE: uuid::Uuid =
    uuid::Uuid::from_u128(0x0169_c0de_5e55_1047_a11e_d7ea_11ce_0001);

/// The synthesized session's id, derived from what it repairs rather than
/// minted at random.
///
/// Every device that opens a shared note sees the same orphan — the transcript
/// syncs whole and the sessions that fail to account for it sync too — so every
/// device repairs it independently. With random ids each mints a *different*
/// session, the server's unique key is `(note, client_id)` so both records are
/// accepted, and the note ends up projecting the orphan twice; the next rebuild
/// then writes the duplicate into `note.transcript`. Deriving the id from the
/// note and the orphaned lines makes those writes the same write:
/// `upsert_record` finds the existing record and PATCHes it, and
/// `reconcile_manifest` (keyed by id) collapses the manifests locally.
///
/// A v5 UUID keeps the id in the `^[A-Za-z0-9-]{1,64}$` shape the server pins
/// `client_id` to — a readable sentinel like `__orphan__` would be rejected.
fn repair_session_id(note_id: &str, lines: &[&str]) -> String {
    let mut seed = String::from(note_id);
    for line in lines {
        seed.push('\n');
        seed.push_str(line.trim());
    }
    uuid::Uuid::new_v5(&REPAIR_SESSION_NAMESPACE, seed.as_bytes()).to_string()
}

/// Serialise orphaned transcript lines as a session timeline. Each line keeps
/// whatever speaker label it carried and loses nothing else; there are no word
/// timings to recover, and none are invented (ADR-0004 — a stale mapping is
/// worse than none).
fn synthesize_orphan_timeline(lines: &[&str]) -> String {
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        let (label, text) = split_label(line);
        let start = i as u64 * SYNTHESIZED_ENTRY_MS;
        out.push_str(
            &serde_json::json!({
                "start_ms": start,
                "end_ms": start + SYNTHESIZED_ENTRY_MS,
                "label": label.unwrap_or(""),
                "text": text,
                "words": [],
            })
            .to_string(),
        );
        out.push('\n');
    }
    out
}

/// Persist a rebuilt transcript to the DB, ping sync, and notify the UI.
///
/// Refuses to replace a non-empty transcript with an empty projection. Notes
/// recorded before sessions existed have a transcript and no timeline at all,
/// and every rebuild path is a UI action (cycle a label, delete a chunk,
/// re-diarize, unify) that would otherwise blank them in one click. Those
/// notes are out of #169's reader symptom — with no timeline they render the
/// textarea and hide nothing — but they sit behind the same destructive paths.
fn commit_rebuilt_transcript(app: &AppHandle, note_id: &str, transcript: String) -> Result<(), String> {
    {
        let state: State<AppState> = app.state();
        let conn = state.db.lock();
        if transcript.trim().is_empty()
            && db::get_note(&conn, note_id)
                .map(|n| !n.transcript.trim().is_empty())
                .unwrap_or(false)
        {
            return Err(
                "refusing to clear the transcript: this note's recordings have no timeline to rebuild from"
                    .to_string(),
            );
        }
        db::set_transcript(&conn, note_id, &transcript).map_err(err)?;
    }
    note_changed_for_sync(app, note_id); // timeline edit rewrote the transcript
    let _ = app.emit(
        "transcript_replaced",
        TranscriptPayload {
            note_id: note_id.to_string(),
            text: transcript,
        },
    );
    Ok(())
}

/// Override the speaker label for a single chunk in a session's timeline,
/// then rebuild `note.transcript` across *all* sessions so the saved text
/// reflects the change. Used when the user clicks a chunk pill to cycle
/// through speakers — handles cases where diarize merged or split turns
/// incorrectly.
///
/// `session_id` selects which take's timeline file to edit (legacy flat
/// notes pass [`sessions::LEGACY_SESSION_ID`]); `chunk_idx` is the 0-based
/// line index within *that* file.
#[tauri::command]
pub fn note_timeline_set_chunk_label(
    app: AppHandle,
    note_id: String,
    session_id: String,
    chunk_idx: usize,
    new_label: String,
) -> Result<(), String> {
    let app_dir = app.path().app_data_dir().map_err(err)?;
    let recordings = sessions::recordings_dir(&app_dir, &note_id);
    let Some(dir) = sessions::resolve_session_dir(&recordings, &session_id) else {
        return Err("no timeline for this session".to_string());
    };
    let path = dir.join("timeline.jsonl");
    if !path.exists() {
        return Err("no timeline for this note".to_string());
    }
    let mut entries = read_timeline_values(&path);
    if chunk_idx >= entries.len() {
        return Err(format!(
            "chunk_idx {chunk_idx} out of bounds (timeline has {} entries)",
            entries.len()
        ));
    }
    entries[chunk_idx]["label"] = serde_json::Value::String(new_label);
    write_timeline_values(&path, &entries)?;

    let transcript = rebuild_note_transcript(&app, &note_id)?;
    commit_rebuilt_transcript(&app, &note_id, transcript)
}

/// Rewrite the text of one rendered *turn* — a run of same-label entries the
/// reader merged into a single paragraph — inside a session's parsed timeline
/// values.
///
/// The replacement lands in the lowest index and every other entry in the run
/// is emptied; `group_values_to_transcript` skips empty-text entries, so the
/// turn re-derives as one line with no blank where the tail entries were. The
/// entries are kept rather than removed because a chunk index *is* a line
/// position: dropping one would misroute the next label cycle or delete.
///
/// Word timings are dropped on every entry in the run — they describe the text
/// that was there, and a stale word→time mapping is worse than none. Entry
/// bounds stay, so the turn still highlights as playback passes it; only
/// per-word karaoke is lost, on that turn alone.
///
/// Bounds are checked before any write, so a bad index can't leave a
/// multi-entry turn half-rewritten.
fn apply_group_text(
    entries: &mut [serde_json::Value],
    chunk_idxs: &[usize],
    new_text: &str,
) -> Result<(), String> {
    if chunk_idxs.is_empty() {
        return Err("no chunk indices given".to_string());
    }
    for &idx in chunk_idxs {
        if idx >= entries.len() {
            return Err(format!(
                "chunk_idx {idx} out of bounds (timeline has {} entries)",
                entries.len()
            ));
        }
        // Assigning a key into a non-object `Value` panics, and a panic in a
        // command takes the app with it. A timeline line that parsed as valid
        // JSON but isn't an object is malformed either way — fail it here.
        if !entries[idx].is_object() {
            return Err(format!("timeline entry {idx} is not an object"));
        }
    }
    // One transcript line per turn: a newline typed into the textarea would
    // otherwise produce text no timeline entry accounts for.
    let text = new_text.split_whitespace().collect::<Vec<_>>().join(" ");

    let mut sorted: Vec<usize> = chunk_idxs.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    for (n, &idx) in sorted.iter().enumerate() {
        entries[idx]["text"] = serde_json::Value::String(if n == 0 {
            text.clone()
        } else {
            String::new()
        });
        entries[idx]["words"] = serde_json::Value::Array(Vec::new());
    }
    Ok(())
}

/// Replace the text of one turn in a session's timeline, then rebuild
/// `note.transcript` across all sessions from the result — so the transcript
/// stays a projection of the timeline and a free-text edit can no longer
/// orphan it (#170).
///
/// `chunk_idxs` is the full run of entries the reader merged into that turn
/// (`TimelineGroup.indices`), so a multi-entry turn is one atomic call that
/// re-derives the transcript once.
#[tauri::command]
pub fn note_timeline_set_chunk_text(
    app: AppHandle,
    note_id: String,
    session_id: String,
    chunk_idxs: Vec<usize>,
    new_text: String,
) -> Result<(), String> {
    let app_dir = app.path().app_data_dir().map_err(err)?;
    let recordings = sessions::recordings_dir(&app_dir, &note_id);
    let Some(dir) = sessions::resolve_session_dir(&recordings, &session_id) else {
        return Err("no timeline for this session".to_string());
    };
    let path = dir.join("timeline.jsonl");
    if !path.exists() {
        return Err("no timeline for this note".to_string());
    }
    let mut entries = read_timeline_values(&path);
    apply_group_text(&mut entries, &chunk_idxs, &new_text)?;
    write_timeline_values(&path, &entries)?;

    let transcript = rebuild_note_transcript(&app, &note_id)?;
    commit_rebuilt_transcript(&app, &note_id, transcript)
}

/// Drop a single chunk from a session's timeline by index, then rebuild
/// `note.transcript` across all sessions from what's left. Used when the user
/// clicks the per-row × in the player view to remove an off-topic chunk.
#[tauri::command]
pub fn note_timeline_delete_chunk(
    app: AppHandle,
    note_id: String,
    session_id: String,
    chunk_idx: usize,
) -> Result<(), String> {
    let app_dir = app.path().app_data_dir().map_err(err)?;
    let recordings = sessions::recordings_dir(&app_dir, &note_id);
    let Some(dir) = sessions::resolve_session_dir(&recordings, &session_id) else {
        return Err("no timeline for this session".to_string());
    };
    let path = dir.join("timeline.jsonl");
    if !path.exists() {
        return Err("no timeline for this note".to_string());
    }
    let mut entries = read_timeline_values(&path);
    if chunk_idx >= entries.len() {
        return Err(format!(
            "chunk_idx {chunk_idx} out of bounds (timeline has {} entries)",
            entries.len()
        ));
    }
    entries.remove(chunk_idx);
    // Leave a zero-byte file when emptied so subsequent reads still find it.
    write_timeline_values(&path, &entries)?;

    let transcript = rebuild_note_transcript(&app, &note_id)?;
    commit_rebuilt_transcript(&app, &note_id, transcript)
}

/// Rewrite every timeline entry whose label exactly matches `old_label` to
/// use `new_label` instead, across **all** of the note's sessions. Mirrors
/// the regex line-anchored rename the frontend does on `note.transcript`, so
/// the player's chunk highlights stay in sync when the user renames (or
/// merges, #23) a speaker via the chip strip. Best-effort: skips sessions
/// with no timeline and malformed lines instead of failing the whole rewrite.
#[tauri::command]
pub fn note_timeline_rename(
    app: AppHandle,
    note_id: String,
    old_label: String,
    new_label: String,
) -> Result<(), String> {
    if old_label == new_label {
        return Ok(());
    }
    let app_dir = app.path().app_data_dir().map_err(err)?;
    let recordings = sessions::recordings_dir(&app_dir, &note_id);
    for (_, dir) in sessions::resolve_sessions(&recordings) {
        let path = dir.join("timeline.jsonl");
        if !path.exists() {
            continue;
        }
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let mut out = String::with_capacity(content.len());
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let mut v: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => {
                    out.push_str(line);
                    out.push('\n');
                    continue;
                }
            };
            if v.get("label").and_then(|s| s.as_str()) == Some(old_label.as_str()) {
                v["label"] = serde_json::Value::String(new_label.clone());
            }
            out.push_str(&v.to_string());
            out.push('\n');
        }
        if let Err(e) = std::fs::write(&path, out) {
            eprintln!("timeline rename: write {}: {e}", path.display());
        }
    }
    Ok(())
}

// ---- Speaker diarization model management ---------------------------------

/// Resolve the active diarization engine from settings. Used by
/// diarize_and_apply when it needs to know which engine to call without
/// the caller having to thread the value through.
fn active_diarize_engine(state: &State<AppState>) -> diarize::Engine {
    let conn = state.db.lock();
    let id = db::get_setting(&conn, "diarize_model")
        .ok()
        .flatten()
        .unwrap_or_else(|| DEFAULT_DIARIZE_MODEL.to_string());
    diarize::Engine::from_setting(&id)
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

/// Drop-in replacement for `diarize::diarize_file` at sites that feed
/// segments into `split_by_segments`. Runs the diarize sidecar, then
/// (when enabled) hands the segments through `diarize::clean_segments`
/// to merge same-speaker fragments, drop contained-inside noise, and
/// floor sub-150ms artifacts. Logs the pre/post counts so the user can
/// see the reduction in the dev console.
async fn diarize_and_maybe_clean(
    app: &AppHandle,
    audio_path: &std::path::Path,
    num_speakers: Option<i64>,
    engine: diarize::Engine,
    thresholds: diarize::Thresholds,
) -> anyhow::Result<Vec<diarize::Segment>> {
    let raw = diarize::diarize_file(app, audio_path, num_speakers, engine, thresholds).await?;
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
/// `pieces_pre` and `pieces_post` are the labelled-piece sequence the
/// transcript emitter walks, captured before and after
/// `bridge_short_interjections` runs. When both are present the dump
/// also includes a `bridge_changes` diff so flicker-absorption
/// decisions (and non-decisions) are inspectable.
async fn write_diagnostics_json(
    app: &AppHandle,
    note_id: &str,
    engine: diarize::Engine,
    source: &str,
    mic_segments: &[diarize::Segment],
    sys_segments: &[diarize::Segment],
    chunks: &[ChunkRecord],
    thresholds: &diarize::Thresholds,
    pieces_pre: Option<&[LabelledPiece]>,
    pieces_post: Option<&[LabelledPiece]>,
) {
    let Ok(app_dir) = app.path().app_data_dir() else {
        eprintln!("diagnostics: app_data_dir unavailable, skipping write");
        return;
    };
    let dir = app_dir.join("diagnostics").join(note_id);
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        eprintln!("diagnostics: mkdir {}: {e}", dir.display());
        return;
    }
    let engine_arg = match engine {
        diarize::Engine::Community1 => "community1",
        diarize::Engine::Sortformer => "sortformer",
    };
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
    let pieces_pre_json = pieces_pre.map(|pieces| pieces_to_json(pieces));
    let pieces_post_json = pieces_post.map(|pieces| pieces_to_json(pieces));
    let bridge_changes = match (pieces_pre, pieces_post) {
        (Some(pre), Some(post)) if pre.len() == post.len() => pre
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
            "sortformer_silence": thresholds.sortformer_silence,
            "sortformer_pred": thresholds.sortformer_pred,
        },
        "mic_segments": mic_segments,
        "sys_segments": sys_segments,
        "chunks": chunk_payload,
        "pieces_before_bridge": pieces_pre_json,
        "pieces_after_bridge": pieces_post_json,
        "bridge_changes": bridge_changes,
        "created_at": chrono::Utc::now().timestamp_millis(),
    });

    let json = match serde_json::to_string_pretty(&payload) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("diagnostics: serialize: {e}");
            return;
        }
    };
    if let Err(e) = tokio::fs::write(&path, json).await {
        eprintln!("diagnostics: write {}: {e}", path.display());
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
) {
    let Ok(app_dir) = app.path().app_data_dir() else {
        return;
    };
    let recordings = sessions::recordings_dir(&app_dir, note_id);
    let duration_ms = timeline_duration_ms(&sessions::session_write_dir(&recordings, session_id));
    let session_id = session_id.to_string();
    let started_at = started_at.to_string();
    // Serialize the append (read-modify-write of sessions.json) against a
    // concurrent cloud pull reconcile so neither loses the other's session.
    let lock = app.state::<AppState>().manifest_lock.clone();
    let _manifest_guard = lock.lock().await;
    let res = tokio::task::spawn_blocking(move || {
        sessions::append_session(&recordings, &session_id, &started_at, duration_ms, streams)
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

/// Read the device's `keep_audio` decision (#24). The single gate consulted by
/// every path that would write, upload or download a WAV.
pub(crate) fn keep_audio_enabled(app: &AppHandle) -> bool {
    let state: State<AppState> = app.state();
    let raw = {
        let conn = state.db.lock();
        db::get_setting(&conn, "keep_audio").ok().flatten()
    };
    sessions::retain_audio(raw.as_deref())
}

async fn maybe_keep_audio(app: &AppHandle, note_id: &str, snapshot: &PostStopSnapshot) {
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
    if let Some(src) = mic_wav {
        if let Err(e) = tokio::fs::copy(&src, target.join("mic.wav")).await {
            eprintln!("keep_audio: copy mic: {e}");
        }
    }
    if let Some(src) = sys_wav {
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

/// Read the user-tunable diarizer thresholds from settings. Missing
/// values fall back to the DEFAULT_* constants at the top of this file
/// so a fresh DB (no settings rows yet) uses the same numbers the
/// settings UI shows. Unparseable values still drop to None — we don't
/// paper over a malformed value because silently picking the default
/// when the user typed "abc" hides the bug.
fn read_diarize_thresholds(state: &State<AppState>) -> diarize::Thresholds {
    let conn = state.db.lock();
    let community1_clustering = db::get_setting(&conn, "community1_threshold")
        .ok()
        .flatten()
        .or_else(|| Some(DEFAULT_COMMUNITY1_THRESHOLD.to_string()))
        .and_then(|s| s.parse::<f64>().ok());
    let sortformer_silence = db::get_setting(&conn, "sortformer_silence_threshold")
        .ok()
        .flatten()
        .or_else(|| Some(DEFAULT_SORTFORMER_SILENCE_THRESHOLD.to_string()))
        .and_then(|s| s.parse::<f32>().ok());
    let sortformer_pred = db::get_setting(&conn, "sortformer_pred_threshold")
        .ok()
        .flatten()
        .or_else(|| Some(DEFAULT_SORTFORMER_PRED_THRESHOLD.to_string()))
        .and_then(|s| s.parse::<f32>().ok());
    diarize::Thresholds {
        community1_clustering,
        sortformer_silence,
        sortformer_pred,
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
    Ok(if s.note_id.is_some() { "recording" } else { "idle" })
}

#[tauri::command]
pub async fn recording_start(
    app: AppHandle,
    state: State<'_, AppState>,
    note_id: String,
) -> Result<(), String> {
    // Self-heal stale sessions before refusing. We get here when the
    // session struct still has note_id set — could be a real recording
    // in progress, or a zombie left behind by a dev reload / app crash
    // that didn't flow through recording_stop.
    //
    // - If the tracked child has already exited → pure garbage, clear it.
    // - If the child is still running but its reader handle is gone (the
    //   stdout pipe was closed without recording_stop running) → orphan,
    //   SIGTERM it and take over.
    // - Only when both child AND reader are alive do we treat it as a
    //   genuine concurrent recording and refuse.
    let stale_child: Option<tokio::process::Child> = {
        let mut s = state.recording.lock();
        if s.note_id.is_some() {
            let child_dead = match s.child.as_mut() {
                Some(c) => matches!(c.try_wait(), Ok(Some(_)) | Err(_)),
                None => true,
            };
            let reader_dead = s.reader.as_ref().map_or(true, |r| r.is_finished());

            if !child_dead && !reader_dead {
                return Err("already recording".into());
            }

            let stale = s.child.take();
            s.note_id = None;
            s.temp_dir = None;
            s.reader = None;
            s.inflight = Arc::new(parking_lot::Mutex::new(Vec::new()));
            stale
        } else {
            None
        }
    };
    if let Some(mut c) = stale_child {
        if let Some(pid) = c.id() {
            #[cfg(unix)]
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
        }
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), c.wait()).await;
        let _ = c.kill().await;
    }

    // Pre-check the configured provider's prerequisites (API key / local model
    // present) and fire the local-Whisper prewarm. Shared with `import_audio`.
    ensure_provider_ready(&app, &state, &note_id).await?;

    // Pre-check microphone permission — without it we can't capture anything useful.
    if let Ok(p) = permissions_status(app.clone()).await {
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
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.kill_on_drop(true);
    // Detach the child into a new session so macOS TCC doesn't tie its
    // microphone / screen-recording authorization to the parent dev binary.
    // Without this, the sidecar inherits the parent's TCC "responsible process"
    // and is silently denied even though its own binary is granted.
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
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
        s.mic_trail.lock().clear();
        s.sys_trail.lock().clear();
        s.chunk_log.lock().clear();
        *s.mic_full_wav_path.lock() = None;
        *s.sys_full_wav_path.lock() = None;
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
                    if dispatch_sidecar_event(&app_clone, &note_id_clone, ev, &inflight_for_reader, None).await {
                        break;
                    }
                }
                Err(e) => eprintln!("bad sidecar line: {e} -- {line}"),
            }
        }
        // Reader exited (sidecar closed its pipe). If the session is still
        // marked as recording for THIS note, that means the sidecar died
        // without us asking — i.e. a crash. Clean up and notify the UI so
        // the user isn't pinned in a stale "recording" state.
        let state: tauri::State<AppState> = app_clone.state();
        let (was_active, lock_id, lock_heartbeat) = {
            let mut s = state.recording.lock();
            if s.note_id.as_deref() == Some(&note_id_clone) {
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

    state.recording.lock().reader = Some(reader_handle);

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
    let (child, note_id, temp_dir, inflight, reader, lock_id, lock_heartbeat) = {
        let mut s = state.recording.lock();
        let note_id = s.note_id.take().ok_or("not recording")?;
        let child = s.child.take();
        let temp_dir = s.temp_dir.take();
        // The reader holds a clone of this same Arc, so chunks emitted during
        // shutdown still land in the list we drain below. Swap in a fresh
        // list to keep `s` self-consistent for the next session.
        let inflight = std::mem::replace(&mut s.inflight, Arc::new(parking_lot::Mutex::new(Vec::new())));
        let reader = s.reader.take();
        let lock_id = s.lock_id.take();
        let lock_heartbeat = s.lock_heartbeat.take();
        (child, note_id, temp_dir, inflight, reader, lock_id, lock_heartbeat)
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

    emit_status(&app, Some(&note_id), Phase::Stopping);

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

    // Wait for the stdout reader to finish first: it exits when the sidecar
    // closes the pipe, which is guaranteed now that `child.wait()` returned.
    // After this point no more transcribe handles can be pushed to inflight.
    if let Some(r) = reader {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), r).await;
    }

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
    // seconds in the typical case.
    emit_status(&app, Some(&note_id), Phase::Stopping);
    drain_inflight(&inflight).await;

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
        run_post_stop_chain(app_for_post, note_for_post, temp_dir, post_stop).await;
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
    let drain = async {
        loop {
            let next = inflight.lock().pop();
            match next {
                Some(h) => { let _ = h.await; }
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
    }
}

/// Freeze the live capture's session-derived state into a [`PostStopSnapshot`].
/// Must be called *after* the inflight drain so `chunk_log` includes every
/// finished chunk. `session_id` / `session_started_at` fall back to fresh
/// values if (somehow) unset — a missing id only costs this take its own
/// subdir, never a clobber. Shared by `recording_stop` and `finish_import`.
fn take_post_stop_snapshot(s: &mut crate::recording::LiveCapture) -> PostStopSnapshot {
    let mic_wav = s.mic_full_wav_path.lock().clone();
    let sys_wav = s.sys_full_wav_path.lock().clone();
    let chunks = s.chunk_log.lock().clone();
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
) {
    emit_status(&app, Some(&note_id), Phase::Diarizing);

    // Make the note's storage session-shaped before writing this take. For a
    // pre-feature flat note, migrate its single take into a session subdir so
    // both takes survive.
    let _ = prepare_sessions_for_new_take(&app, &note_id).await;

    // Copy full WAVs into this session's subdir FIRST when keep_audio is on.
    // diarize_and_apply cleans up the temp paths after it's done, so retention
    // has to happen before it runs. (#16's "a 2nd+ session force-retains its
    // sources" exception is gone — #24 made the setting absolute, so a
    // multi-take note recorded with keep_audio off simply can't be unified.)
    maybe_keep_audio(&app, &note_id, &post_stop).await;

    let session_id = post_stop.session_id.clone();
    let session_started_at = post_stop.session_started_at.clone();
    let streams = session_streams(&post_stop.chunks);
    let transcribed_nothing = post_stop.chunks.is_empty();
    // Record what the provider heard before diarizing (issue #167).
    // Deliberately *not* inside diarize_and_apply: that returns early when
    // the diarize model isn't downloaded, so a user without it would never
    // get a detected language. File import inherits this for free — it
    // drives the same chain.
    record_detected_language(&app, &note_id, &post_stop.chunks);
    if let Err(e) = diarize_and_apply(app.clone(), note_id.clone(), post_stop).await {
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
        if let Some(dir) = temp_dir {
            let _ = tokio::fs::remove_dir_all(dir).await;
        }
        emit_status(&app, None, Phase::Idle);
        return;
    }

    // Append this take to the manifest so it shows up in the carousel and in
    // session-aware path resolution. Duration comes from the timeline
    // diarize_and_apply just wrote (max end_ms); 0 when nothing landed.
    finalize_session(&app, &note_id, &session_id, &session_started_at, streams).await;
    // Cross-session speaker unification (#17): once the note has two or more
    // takes with retained source audio + chunk timings, re-cluster the
    // concatenated audio so one voice carries one label across takes —
    // replacing the offset-only numbers diarize_and_apply just wrote. Must
    // run after finalize_session (the manifest has to include this take) and
    // before the temp-dir cleanup is irrelevant (it reads the retained
    // session copies, not the temp WAVs). No-op for single-session notes;
    // failures keep the per-take labels.
    match unify_note_speakers(&app, &note_id).await {
        Ok(true) => eprintln!("unify: cross-session speaker unification applied"),
        Ok(false) => {}
        Err(e) => eprintln!("unify: failed, keeping per-take labels: {e}"),
    }
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
    if let Some(dir) = temp_dir {
        let _ = tokio::fs::remove_dir_all(dir).await;
    }
    emit_status(&app, None, Phase::Idle);
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
) -> Result<(), String> {
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
        other => {
            let provider_id = other.provider_id();
            if read_provider_api_key(state, provider_id)?.is_none() {
                let label = match provider_id {
                    "openai" => "OpenAI",
                    "deepgram" => "Deepgram",
                    "groq" => "Groq",
                    _ => "the selected provider",
                };
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
    Ok(())
}

/// Handle one decoded sidecar event during a live capture or an import.
///
/// Chunks are transcribed on spawned tasks tracked in `inflight`. When
/// `backlog` is `Some`, a permit is acquired *before* spawning (and released
/// when the task finishes), so a full-speed import replay can't pile up
/// hundreds of parked pre-gate tasks. Live recording passes `None` — realtime
/// arrival needs no bound. Returns `true` when the sidecar signalled `Stopped`
/// and the reader loop should break.
async fn dispatch_sidecar_event(
    app: &AppHandle,
    note_id: &str,
    event: SidecarEvent,
    inflight: &Inflight,
    backlog: Option<&Arc<Semaphore>>,
) -> bool {
    match event {
        SidecarEvent::Chunk { source, path, start_ms } => {
            let pb = PathBuf::from(path);
            let app2 = app.clone();
            let note_id2 = note_id.to_string();
            // Block the reader here until a permit frees when importing — this
            // is the backpressure. `acquire_owned` yields a permit we move into
            // the spawned task so it's released the instant the chunk finishes.
            let permit = match backlog {
                Some(sem) => sem.clone().acquire_owned().await.ok(),
                None => None,
            };
            let h = tokio::spawn(async move {
                let _permit = permit; // held until this transcribe returns
                if let Err(e) =
                    transcribe_chunk(app2.clone(), note_id2.clone(), source, pb, start_ms).await
                {
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
        SidecarEvent::FullRecording { source, path, duration_ms: _ } => {
            // Stash the path on the session; the diarization pass on stop reads
            // it. Each source has its own slot so the post-stop pass can branch
            // (mic-only → diarize mic; both present → "You" + diarize sys).
            let state: State<AppState> = app.state();
            let session = state.recording.lock();
            let slot = match source {
                ChunkSource::Mic => &session.mic_full_wav_path,
                ChunkSource::Sys => &session.sys_full_wav_path,
            };
            *slot.lock() = Some(PathBuf::from(path));
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
        SidecarEvent::Heartbeat { mic_frames, sys_frames, chunks, mic_peak, sys_peak } => {
            let _ = app.emit("recording_diagnostic", DiagnosticPayload {
                note_id: note_id.to_string(),
                mic_frames,
                sys_frames,
                chunks,
                mic_peak,
                sys_peak,
            });
            false
        }
        SidecarEvent::Diagnostic { message } => {
            // Non-fatal sidecar notice (e.g. mic capture recovered after a
            // device change). Reuse the recording_error channel — it's a
            // transient auto-dismissing toast on the frontend.
            eprintln!("sidecar diagnostic: {message}");
            let _ = app.emit("recording_error", ErrorPayload {
                note_id: Some(note_id.to_string()),
                message,
            });
            false
        }
    }
}

/// Seed a note title from an imported audio file's name: the file stem with no
/// extension, trimmed. Falls back to "Imported audio" for a path with no usable
/// stem. Kept deliberately literal — the user's filename is meaningful, so we
/// don't reflow or re-case it.
pub(crate) fn title_from_filename(path: &std::path::Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.trim())
        .unwrap_or("");
    if stem.is_empty() {
        "Imported audio".to_string()
    } else {
        stem.to_string()
    }
}

/// Import an existing audio file into a NEW note and run it through the full
/// transcription pipeline (VAD chunking → transcribe fan-out → mic-only diarize
/// → playback assets → summary-ready), streaming the transcript live.
///
/// Mirrors `recording_start` but: skips the mic/screen permission checks and the
/// cloud recording lock (a fresh personal note has no contention), seeds the
/// title from the filename, and spawns the sidecar in `--import` mode (decode →
/// resample → replay through the same VAD ChunkWriter as the mic stream). Still
/// runs the provider prereq check. Occupies the single live-capture slot, so
/// Record and Import are mutually exclusive.
#[tauri::command]
pub async fn import_audio(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
    // From the import config dialog. `language` is a code like "en"/"no";
    // `expected_speakers` is a diarization hint (None = auto).
    language: String,
    expected_speakers: Option<i64>,
) -> Result<db::Note, String> {
    let source_path = PathBuf::from(&path);
    if !source_path.exists() {
        return Err(format!("File not found: {path}"));
    }

    // Same single-slot self-heal + refusal as recording_start: import and live
    // recording share the one capture slot.
    let stale_child: Option<tokio::process::Child> = {
        let mut s = state.recording.lock();
        if s.note_id.is_some() {
            let child_dead = match s.child.as_mut() {
                Some(c) => matches!(c.try_wait(), Ok(Some(_)) | Err(_)),
                None => true,
            };
            let reader_dead = s.reader.as_ref().map_or(true, |r| r.is_finished());
            if !child_dead && !reader_dead {
                return Err("already recording or importing".into());
            }
            let stale = s.child.take();
            s.note_id = None;
            s.temp_dir = None;
            s.reader = None;
            s.inflight = Arc::new(parking_lot::Mutex::new(Vec::new()));
            stale
        } else {
            None
        }
    };
    if let Some(mut c) = stale_child {
        if let Some(pid) = c.id() {
            #[cfg(unix)]
            unsafe { libc::kill(pid as i32, libc::SIGTERM); }
        }
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), c.wait()).await;
        let _ = c.kill().await;
    }

    // Create the destination note now so provider-prereq errors can attach to
    // it and the title can be seeded from the filename. v1: a Personal
    // (unsynced) note — no cloud recording lock needed.
    let note = {
        let conn = state.db.lock();
        // Language comes from the import dialog. Fall back to the global default
        // only if the caller sent an empty string (defensive / older frontend).
        let language = if language.trim().is_empty() {
            db::get_setting(&conn, "language")
                .map_err(err)?
                .unwrap_or_else(|| DEFAULT_LANGUAGE.to_string())
        } else {
            language.clone()
        };
        let default_preset = db::get_setting(&conn, "default_summary_preset")
            .map_err(err)?
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "meeting".to_string());
        let mut note = db::create_note(&conn, &language, &default_preset, "").map_err(err)?;
        let title = title_from_filename(&source_path);
        db::update_note(
            &conn,
            &note.id,
            &db::NotePatch {
                title: Some(title.clone()),
                // `Some(expected_speakers)` writes the hint; `Some(None)` (the
                // "Auto" choice) leaves diarization to auto-detect.
                expected_speakers: Some(expected_speakers),
                ..Default::default()
            },
        )
        .map_err(err)?;
        note.title = title;
        note.expected_speakers = expected_speakers;
        note
    };
    state.sync.note_upserted(&note.id);
    let note_id = note.id.clone();

    // Provider prereq (key / local model). Skipped vs. recording_start:
    // mic/screen permission + cloud recording lock.
    ensure_provider_ready(&app, &state, &note_id).await?;

    emit_status(&app, Some(&note_id), Phase::Importing);

    let temp_dir = std::env::temp_dir().join(format!("notes-app-import-{}", note_id));
    std::fs::create_dir_all(&temp_dir).map_err(err)?;

    let sidecar_path = sidecar_path(&app)?;
    let mut cmd = Command::new(&sidecar_path);
    cmd.arg("--import").arg(&source_path);
    cmd.arg("--out").arg(&temp_dir);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.kill_on_drop(true);
    // Detach into a new session for parity with the live path (harmless here —
    // import needs no TCC-bound permissions).
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                // Non-fatal: continue without detaching.
            }
            Ok(())
        });
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("spawn audio-capture (import): {e}"))?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let stderr = child.stderr.take().ok_or("no stderr")?;

    // Drain stderr (same humla-error filter as recording_start).
    {
        let app_err = app.clone();
        let note_id_err = note_id.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                let trimmed = line.trim();
                if trimmed.is_empty() { continue; }
                eprintln!("audio-capture(import) stderr: {trimmed}");
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
    {
        let mut s = state.recording.lock();
        s.note_id = Some(note_id.clone());
        s.child = Some(child);
        s.temp_dir = Some(temp_dir);
        s.inflight = inflight.clone();
        s.session_id = Some(uuid::Uuid::new_v4().to_string());
        s.session_started_at = Some(chrono::Utc::now().to_rfc3339());
        s.mic_trail.lock().clear();
        s.sys_trail.lock().clear();
        s.chunk_log.lock().clear();
        *s.mic_full_wav_path.lock() = None;
        *s.sys_full_wav_path.lock() = None;
        // Fresh note → no prior transcript to prepend on diarize.
        *s.transcript_at_start.lock() = String::new();
        // Import holds no cloud recording lock.
        s.lock_id = None;
    }

    let app_clone = app.clone();
    let note_id_clone = note_id.clone();
    let inflight_for_reader = inflight.clone();
    let reader_handle = tokio::spawn(async move {
        // Bounded backlog so a full-speed replay can't pile up parked tasks.
        let backlog = Arc::new(Semaphore::new(IMPORT_BACKLOG_PERMITS));
        let mut reader = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            let trimmed = line.trim();
            if trimmed.is_empty() { continue; }
            match serde_json::from_str::<SidecarEvent>(trimmed) {
                Ok(ev) => {
                    if dispatch_sidecar_event(
                        &app_clone,
                        &note_id_clone,
                        ev,
                        &inflight_for_reader,
                        Some(&backlog),
                    )
                    .await
                    {
                        break;
                    }
                }
                Err(e) => eprintln!("bad sidecar line: {e} -- {line}"),
            }
        }
        // Replay finished (Stopped, or the sidecar closed its pipe). Drive the
        // same post-stop chain the live path runs on recording_stop.
        finish_import(&app_clone, &note_id_clone).await;
    });

    state.recording.lock().reader = Some(reader_handle);

    Ok(note)
}

/// Completion path for an import: the sidecar replayed the whole file and closed
/// its pipe. Drain the outstanding transcribes, free the capture slot, then run
/// the shared post-stop chain (mic-only diarize → playback assets →
/// summary-ready). No SIGTERM/child-wait — the import sidecar exits on its own.
async fn finish_import(app: &AppHandle, note_id: &str) {
    let state: State<AppState> = app.state();
    // Bail if a newer capture already claimed the slot (shouldn't happen —
    // import holds it until here).
    let inflight = {
        let s = state.recording.lock();
        if s.note_id.as_deref() != Some(note_id) {
            return;
        }
        s.inflight.clone()
    };
    // No more chunks are coming (reader loop exited), so drain what's queued.
    drain_inflight(&inflight).await;

    let (temp_dir, post_stop) = {
        let mut s = state.recording.lock();
        let temp_dir = s.temp_dir.take();
        let post_stop = take_post_stop_snapshot(&mut s);
        // Free the slot so a new capture can start while this one's diarize runs
        // in the background (mirrors recording_stop).
        s.note_id = None;
        s.child = None;
        s.reader = None;
        s.inflight = Arc::new(parking_lot::Mutex::new(Vec::new()));
        (temp_dir, post_stop)
    };

    run_post_stop_chain(app.clone(), note_id.to_string(), temp_dir, post_stop).await;
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
    let (expected_speakers, engine, thresholds) = {
        let state: State<AppState> = app.state();
        let eng = active_diarize_engine(&state);
        let thr = read_diarize_thresholds(&state);
        let conn = state.db.lock();
        let hint = db::get_note(&conn, &note_id)
            .ok()
            .and_then(|n| n.expected_speakers)
            .filter(|n| *n > 0);
        (hint, eng, thr)
    };
    if chunks.is_empty() {
        eprintln!("diarize: no chunks captured, skipping");
        return Ok(());
    }
    // Diarization is optional — the model may not be downloaded. This used to
    // `return Ok(())` right here, which is the origin of #169: the chunks had
    // already been live-appended to `note.transcript`, so the recording ended
    // with text in the note and no session, no timeline behind it. Under
    // ADR-0004 the timeline is canonical for a note's content, so a recording
    // that lands text always writes a session — this path just writes it with
    // no speaker labels. Tell the user why the labels never arrive.
    let diarize_available = matches!(diarize::status(&app, engine).await, Ok(s) if s.downloaded);
    if !diarize_available {
        eprintln!("diarize: model not downloaded, saving the timeline without labels");
        emit_error(
            &app,
            Some(&note_id),
            "Speaker diarization model isn't downloaded — transcript saved without speaker labels. Download it from Settings → Speaker diarization.",
        );
    }

    let mic_chunks_present = chunks.iter().any(|c| c.source == ChunkSource::Mic);
    let sys_chunks_present = chunks.iter().any(|c| c.source == ChunkSource::Sys);

    if diarize_available {
        emit_status(&app, Some(&note_id), Phase::Diarizing);
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
                Some(wav) => match diarize_and_maybe_clean(&app, &wav, expected_speakers, engine, thresholds).await {
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
                        write_diagnostics_json(&app, &note_id, engine, "mic", &segments, &[], &chunks, &thresholds, None, None).await;
                        let display_map = build_display_map(&chunks, &segments, ChunkSource::Mic);
                        Box::new(move |c: &ChunkRecord| split_by_segments(c, &segments, &display_map))
                    }
                },
            }
        }
        (false, true) => {
            // Edge case: system-only recording. Same shape as mic-only;
            // same three failure modes drop to the single-speaker fallback.
            let single_speaker_fallback = || -> Box<Splitter> {
                Box::new(|c: &ChunkRecord| single_piece(c, Some("Speaker 1".to_string())))
            };
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
                Some(wav) => match diarize_and_maybe_clean(&app, &wav, expected_speakers, engine, thresholds).await {
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
                        write_diagnostics_json(&app, &note_id, engine, "sys", &[], &segments, &chunks, &thresholds, None, None).await;
                        let display_map = build_display_map(&chunks, &segments, ChunkSource::Sys);
                        Box::new(move |c: &ChunkRecord| split_by_segments(c, &segments, &display_map))
                    }
                },
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
                Some(wav) => diarize_and_maybe_clean(&app, &wav, sys_speaker_hint, engine, thresholds)
                    .await
                    .unwrap_or_else(|e| {
                        eprintln!("diarize: sys diarize failed ({e})");
                        emit_error(
                            &app,
                            Some(&note_id),
                            &format!("Diarization failed for the remote side ({e}); remote speech grouped under one speaker."),
                        );
                        Vec::new()
                    }),
            };
            let mic_speaker_hint = mic_hint_after_sys(expected_speakers, &sys_segments);
            let mic_segments = match mic_wav.clone() {
                None => {
                    eprintln!("diarize: hybrid but mic_full.wav missing, mic falls back to You");
                    Vec::new()
                }
                Some(wav) => diarize_and_maybe_clean(&app, &wav, mic_speaker_hint, engine, thresholds)
                    .await
                    .unwrap_or_else(|e| {
                        eprintln!("diarize: hybrid mic diarize failed ({e}), mic falls back to You");
                        Vec::new()
                    }),
            };
            if mic_segments.is_empty() && sys_segments.is_empty() {
                emit_error(
                    &app,
                    Some(&note_id),
                    "Diarization found no distinct speakers; speech grouped by audio source.",
                );
            }
            write_diagnostics_json(&app, &note_id, engine, "hybrid", &mic_segments, &sys_segments, &chunks, &thresholds, None, None).await;
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
    write_playback_assets(
        &app,
        &note_id,
        &session_id,
        timeline,
        mic_wav.as_deref(),
        sys_wav.as_deref(),
    )
    .await;

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
fn assign_speaker<'a>(chunk_start_ms: u64, segments: &'a [diarize::Segment]) -> Option<&'a str> {
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

/// One contiguous run of words within a chunk that all belong to the
/// same speaker. A 15-second VAD chunk that opens with one voice and
/// ends with another becomes two pieces; a single-voice chunk stays
/// one piece.
///
/// `words` are kept chunk-relative (matching `ChunkWord`'s convention)
/// so the timeline serialiser can rebase them to stream-absolute the
/// same way it always has, and may be empty when the underlying chunk
/// had no word data and we fell back to whole-chunk labelling.
#[derive(Clone, Debug)]
struct LabelledPiece {
    label: Option<String>,
    text: String,
    words: Vec<crate::recording::ChunkWord>,
}

/// Wrap a chunk's full text + words as a single labelled piece. Used by
/// the paths that don't split (mic = "You" in hybrid mode, single-
/// speaker fallbacks when diarize returned nothing).
fn single_piece(c: &ChunkRecord, label: Option<String>) -> Vec<LabelledPiece> {
    vec![LabelledPiece {
        label,
        text: c.text.clone(),
        words: c.words.clone(),
    }]
}

/// Split a chunk into per-speaker pieces by walking its word timings
/// against the diarizer's segments. Each word's stream-absolute
/// midpoint (chunk.start_ms + word's chunk-relative midpoint) is looked
/// up in `segments`; consecutive same-speaker words coalesce into one
/// piece, and the speaker label changes mid-chunk produce additional
/// pieces.
///
/// We use the word's *midpoint* rather than its `start_ms` so a word
/// straddling a segment boundary lands on whichever side it spends more
/// of its duration in — start-only would give the leading word of a
/// new turn the previous speaker's label.
///
/// Falls back to whole-chunk labelling (one piece, label decided by
/// `chunk.start_ms`) when the chunk has no words. That covers OpenAI
/// chunks (current API path doesn't expose word timestamps) and
/// re-diarize from older diagnostic JSONs that didn't persist words.
fn split_by_segments(
    c: &ChunkRecord,
    segments: &[diarize::Segment],
    display_map: &std::collections::HashMap<String, u32>,
) -> Vec<LabelledPiece> {
    let labels: std::collections::HashMap<String, String> = display_map
        .iter()
        .map(|(sid, n)| (sid.clone(), format!("Speaker {n}")))
        .collect();
    split_by_labels(c, segments, &labels)
}

/// Same word-walking split as `split_by_segments`, but the label map holds
/// the *final* display string per speaker id rather than a number. That
/// lets the hybrid branch mix a channel-attributed label (`You`, when the
/// mic diarize found a single voice) with numbered ones on the same
/// recording, and lets the second stream's numbers continue past the
/// first's instead of restarting at 1.
fn split_by_labels(
    c: &ChunkRecord,
    segments: &[diarize::Segment],
    labels: &std::collections::HashMap<String, String>,
) -> Vec<LabelledPiece> {
    let label_for_time = |abs_ms: u64| -> Option<String> {
        assign_speaker(abs_ms, segments)
            .and_then(|sid| labels.get(sid))
            .cloned()
    };
    if c.words.is_empty() {
        return single_piece(c, label_for_time(c.start_ms));
    }
    let mut pieces: Vec<LabelledPiece> = Vec::new();
    for word in &c.words {
        let mid = word.start_ms.saturating_add(word.end_ms.saturating_sub(word.start_ms) / 2);
        let abs = c.start_ms.saturating_add(mid);
        let label = label_for_time(abs);
        match pieces.last_mut() {
            Some(last) if last.label == label => {
                last.text.push(' ');
                last.text.push_str(&word.text);
                last.words.push(word.clone());
            }
            _ => {
                pieces.push(LabelledPiece {
                    label,
                    text: word.text.clone(),
                    words: vec![word.clone()],
                });
            }
        }
    }
    pieces
}

/// Cross-stream echo dedup. When a meeting plays through laptop speakers,
/// the mic re-captures the speaker output and Whisper transcribes the same
/// words on both streams ("You: ..." + "Speaker 1: ..." with near-identical
/// text). This pass drops mic chunks whose tokens are mostly contained in
/// time-overlapping sys chunks. The OS-level fix
/// (`AVAudioInputNode.setVoiceProcessingEnabled`) ducks the system output
/// device, which is unusable for a meeting recorder — so we cancel at the
/// transcript layer instead. See `feedback_voice_processing.md` for the
/// long story.
///
/// Behaviour:
/// - No-op when there are no sys chunks (in-person mode, mic-only).
/// - Skips mic chunks under `MIN_MIC_TOKENS` so brief acks ("yeah", "ok")
///   aren't dropped just because those words also appear somewhere in the
///   sys window.
/// - Containment coefficient (intersection / smaller set) rather than
///   Jaccard, because a single sys chunk can be much longer than a mic
///   chunk; Jaccard would dilute below threshold even on a perfect match.
fn dedup_mic_against_sys(chunks: &[ChunkRecord]) -> Vec<ChunkRecord> {
    // Time tolerance for matching mic chunks to sys chunks. Boundaries
    // don't align (each source is VAD-bounded independently) so a sys
    // chunk's content can sit anywhere from a few seconds before a mic
    // chunk starts to a chunk-length after.
    const PRE_MS: u64 = 5_000;
    const POST_MS: u64 = 15_000;
    // Token-overlap threshold above which a mic chunk is considered an
    // echo of the sys text and dropped. Genuine simultaneous speech
    // (you talking while the remote speaks) typically shares <0.3 of
    // tokens because the words are different.
    const SIMILARITY_THRESHOLD: f32 = 0.6;
    // Skip dedup for mic chunks under this many tokens — brief acks
    // ("yeah", "ok") match by chance against any windowed sys text.
    const MIN_MIC_TOKENS: usize = 3;

    let has_sys = chunks.iter().any(|c| c.source == ChunkSource::Sys);
    if !has_sys {
        return chunks.to_vec();
    }

    let sys_chunks: Vec<&ChunkRecord> = chunks
        .iter()
        .filter(|c| c.source == ChunkSource::Sys)
        .collect();

    let mut kept: Vec<ChunkRecord> = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        if chunk.source != ChunkSource::Mic {
            kept.push(chunk.clone());
            continue;
        }
        let mic_tokens = normalize_tokens(&chunk.text);
        if mic_tokens.len() < MIN_MIC_TOKENS {
            kept.push(chunk.clone());
            continue;
        }
        let lower = chunk.start_ms.saturating_sub(PRE_MS);
        let upper = chunk.start_ms.saturating_add(POST_MS);
        let mut sys_window = String::new();
        for s in &sys_chunks {
            if s.start_ms >= lower && s.start_ms <= upper {
                if !sys_window.is_empty() {
                    sys_window.push(' ');
                }
                sys_window.push_str(&s.text);
            }
        }
        if sys_window.is_empty() {
            kept.push(chunk.clone());
            continue;
        }
        let sim = token_containment(&mic_tokens, &normalize_tokens(&sys_window));
        if sim < SIMILARITY_THRESHOLD {
            kept.push(chunk.clone());
        }
        // else: this mic chunk is an echo of the sys content — drop it.
    }
    kept
}

/// Lowercase, split on non-alphanumeric, drop tokens shorter than 2
/// chars. Matches the granularity Whisper emits — punctuation differs
/// across streams ("Hello." vs "hello") and one-letter tokens are too
/// noisy to count toward overlap.
fn normalize_tokens(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 1)
        .map(|w| w.to_string())
        .collect()
}

/// Build the playback-asset bundle for a recording: a single mixed
/// `playback.wav` plus a `timeline.jsonl` mapping each speaker turn
/// back to its first-chunk start_ms, so the frontend can render the
/// transcript with chunk spans and highlight whichever one matches the
/// audio's current position.
///
/// `playback.wav` is gated on `keep_audio` (#24) — it is a full recording of
/// the meeting, so writing it while the setting said "keep nothing" was the
/// dishonesty that issue names. `timeline.jsonl` is written either way: it is
/// text (word timings), and the merged reader and session dividers are built
/// from it whether or not there is anything to play.
async fn write_playback_assets(
    app: &AppHandle,
    note_id: &str,
    session_id: &str,
    timeline: String,
    mic_wav: Option<&std::path::Path>,
    sys_wav: Option<&std::path::Path>,
) {
    if mic_wav.is_none() && sys_wav.is_none() && timeline.is_empty() {
        return;
    }
    let Some(target) = session_write_dir(app, note_id, session_id).await else {
        eprintln!("playback: session dir unavailable");
        return;
    };

    if keep_audio_enabled(app) {
        if let Err(e) = build_playback_wav(mic_wav, sys_wav, &target.join("playback.wav")).await {
            eprintln!("playback: build_playback_wav: {e}");
        }
    }

    if !timeline.is_empty() {
        if let Err(e) = tokio::fs::write(target.join("timeline.jsonl"), timeline).await {
            eprintln!("playback: write timeline.jsonl: {e}");
        }
    }
}

/// Combine the per-source full WAVs into a single mono 16-kHz WAV the
/// `<audio>` element can play. Mixes equal-weight when both streams are
/// present; copies the byte stream when only one exists. Equal weight
/// is the right default — the user is more interested in hearing both
/// sides at the same level than in any acoustic faithfulness, and the
/// upstream streams have already gone through their own gain stages.
async fn build_playback_wav(
    mic: Option<&std::path::Path>,
    sys: Option<&std::path::Path>,
    out: &std::path::Path,
) -> anyhow::Result<()> {
    match (mic, sys) {
        (Some(m), Some(s)) => {
            let mic_samples = wav::read_f32_mono_16k(m).await?;
            let sys_samples = wav::read_f32_mono_16k(s).await?;
            let n = mic_samples.len().max(sys_samples.len());
            let mut mixed = Vec::with_capacity(n);
            for i in 0..n {
                let a = mic_samples.get(i).copied().unwrap_or(0.0);
                let b = sys_samples.get(i).copied().unwrap_or(0.0);
                mixed.push((a + b) * 0.5);
            }
            wav::write_pcm16_mono_16k(out, &mixed).await?;
        }
        (Some(m), None) => {
            tokio::fs::copy(m, out).await?;
        }
        (None, Some(s)) => {
            tokio::fs::copy(s, out).await?;
        }
        (None, None) => {}
    }
    Ok(())
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
            let word_count = entry.text.split_whitespace().count() as u64;
            let estimated = word_count.saturating_mul(350).max(1_000);
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

/// Run the cross-session speaker unification pass (#17) on a note.
///
/// Returns `Ok(true)` when the pass ran and rewrote labels, `Ok(false)` when
/// the note doesn't qualify (fewer than two sessions, fewer than two with
/// retained source audio + chunk timings, or the diarize model isn't
/// downloaded) — callers then keep the per-take offset labelling exactly as
/// before. `Err` means the pass started but failed; existing labels are left
/// untouched (the pass only writes after diarize succeeded on every needed
/// stream).
///
/// Cost note: this re-diarizes the note's ENTIRE concatenated audio (per
/// stream) each time it runs — on every stop of a multi-session note the
/// diarizer processes all takes, not just the new one. That's inherent to
/// the approach (clustering must see all takes at once to unify voices) and
/// bounded by note length; the concat WAVs live in a temp dir and are
/// removed when the pass ends.
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

pub(crate) async fn unify_note_speakers(
    app: &AppHandle,
    note_id: &str,
) -> anyhow::Result<bool> {
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
        return Ok(false);
    }

    let (expected_speakers, engine, thresholds) = {
        let state: State<AppState> = app.state();
        let eng = active_diarize_engine(&state);
        let thr = read_diarize_thresholds(&state);
        let conn = state.db.lock();
        let hint = db::get_note(&conn, note_id)
            .ok()
            .and_then(|n| n.expected_speakers)
            .filter(|n| *n > 0);
        (hint, eng, thr)
    };
    match diarize::status(app, engine).await {
        Ok(s) if s.downloaded => {}
        _ => {
            eprintln!("unify: diarize model not downloaded, keeping per-take labels");
            return Ok(false);
        }
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
        return Ok(false);
    }

    // Concat WAVs are scratch files — never inside the session dirs. Unique per
    // invocation so overlapping passes can't clobber each other's concats.
    let tmp = unify_scratch_dir(note_id);
    tokio::fs::create_dir_all(&tmp).await?;
    let result = unify_apply(
        app,
        note_id,
        &tmp,
        &unifiable,
        &frozen_dirs,
        expected_speakers,
        engine,
        thresholds,
    )
    .await;
    let _ = tokio::fs::remove_dir_all(&tmp).await;
    result
}

/// The IO half of the unify pass, split out so the caller can always clean
/// up the temp dir. Concatenates + diarizes per stream, runs the pure
/// relabel, writes every unified session's timeline, and rebuilds the DB
/// transcript from all sessions (frozen ones included, untouched).
#[allow(clippy::too_many_arguments)]
async fn unify_apply(
    app: &AppHandle,
    note_id: &str,
    tmp: &std::path::Path,
    unifiable: &[UnifyCandidate],
    frozen_dirs: &[PathBuf],
    expected_speakers: Option<i64>,
    engine: diarize::Engine,
    thresholds: diarize::Thresholds,
) -> anyhow::Result<bool> {
    // Per-stream concatenation in manifest order. Mic concat = every session
    // that captured mic (mic-only *and* hybrid — a hybrid take's mic is
    // diarized now, not assumed to be one person); sys concat = sys-only +
    // hybrid. Matches the per-source passes diarize_and_apply runs on a single
    // take. `session_unifiable` guarantees a hybrid session has both WAVs, so
    // neither concat can hit a missing file and misalign the offsets.
    let mic_paths: Vec<PathBuf> = unifiable
        .iter()
        .filter(|c| c.mode != SessionMode::SysOnly)
        .map(|c| c.dir.join("mic.wav"))
        .collect();
    let sys_paths: Vec<PathBuf> = unifiable
        .iter()
        .filter(|c| c.mode != SessionMode::MicOnly)
        .map(|c| c.dir.join("sys.wav"))
        .collect();
    let mic_concat = tmp.join("mic-concat.wav");
    let sys_concat = tmp.join("sys-concat.wav");
    let mic_offsets = concat_wavs(&mic_paths, &mic_concat).await?;
    let sys_offsets = concat_wavs(&sys_paths, &sys_concat).await?;

    // Same speaker-count hint semantics as the per-take pass. With no system
    // stream in play the note's expected_speakers applies to the mic directly
    // (in-person). Once both streams exist the total can't be split a priori,
    // so the mic goes in unhinted and the sys stream is asked for whatever the
    // mic didn't account for. Sortformer's 4-speaker cap applies to the
    // combined audio exactly as it does to a single take — no special-casing.
    let mic_hint = if sys_paths.is_empty() {
        expected_speakers
    } else {
        None
    };
    let mic_segments = match &mic_offsets {
        Some(_) => diarize_and_maybe_clean(app, &mic_concat, mic_hint, engine, thresholds).await?,
        None => Vec::new(),
    };
    let sys_hint = if mic_paths.is_empty() {
        expected_speakers
    } else {
        expected_speakers
            .map(|n| (n - distinct_speaker_count(&mic_segments).max(1) as i64).max(1))
    };
    let sys_segments = match &sys_offsets {
        Some(_) => {
            diarize_and_maybe_clean(app, &sys_concat, sys_hint, engine, thresholds).await?
        }
        None => Vec::new(),
    };
    if mic_offsets.is_some() && mic_segments.is_empty() {
        anyhow::bail!("diarize returned no segments for the combined mic stream");
    }
    if sys_offsets.is_some() && sys_segments.is_empty() {
        anyhow::bail!("diarize returned no segments for the combined system stream");
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
            // Must mirror `mic_paths` / `sys_paths` above exactly — these
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
    Ok(true)
}

/// Jaccard similarity: |A ∩ B| / |A ∪ B|. 1.0 when the sets are
/// equal, scaling down with each token unique to either side. Used
/// for cross-chunk dedup where both chunks come from the same source
/// at similar VAD lengths — penalising added unique tokens lets us
/// keep legitimate continuations (chunk N+1 = chunk N's content +
/// new sentence) while still catching exact / near-exact repeats
/// from a Whisper hallucination loop. Containment is the wrong
/// metric here because `min()` makes it symmetric and scores a
/// strict superset as 1.0.
fn token_jaccard(a: &[String], b: &[String]) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let set_a: std::collections::HashSet<&str> = a.iter().map(String::as_str).collect();
    let set_b: std::collections::HashSet<&str> = b.iter().map(String::as_str).collect();
    let inter = set_a.intersection(&set_b).count() as f32;
    let union = set_a.union(&set_b).count() as f32;
    if union == 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// Containment coefficient: |A ∩ B| / min(|A|, |B|). 1.0 when A ⊆ B
/// (or vice versa). Used for cross-stream echo dedup where a sys
/// window concatenated from multiple chunks is often much larger
/// than a single mic chunk; Jaccard's union-in-the-denominator
/// would suppress the score below threshold even on a perfect echo.
fn token_containment(a: &[String], b: &[String]) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let set_a: std::collections::HashSet<&str> = a.iter().map(String::as_str).collect();
    let set_b: std::collections::HashSet<&str> = b.iter().map(String::as_str).collect();
    let inter = set_a.intersection(&set_b).count() as f32;
    let smaller = set_a.len().min(set_b.len()) as f32;
    if smaller == 0.0 {
        0.0
    } else {
        inter / smaller
    }
}

/// Maximum word count for a labelled piece to be considered a "short
/// interjection" eligible for absorption into the surrounding speaker.
/// Bumped from 3 → 6 after the segment-cleaning pass exposed a long
/// tail of clearly mis-attributed mid-sentence fragments at the
/// 4–6 word range ("Sjefen over alle sjefer", "Jeg tok det feil. Det er"
/// etc.) — all sandwich-shaped, all sub-3500ms, all obviously the
/// surrounding speaker's own words. Real ≥7 word replies, even when
/// sandwiched, still pass through.
const BRIDGE_MAX_WORDS: usize = 6;

/// Maximum acoustic duration for a piece to be considered an
/// interjection. Word-count alone can't separate "yeah totally" (a
/// 0.6s backchannel) from "let me check" (a 1.5s real reply); the
/// acoustic length is what makes them distinguishable. Bumped from
/// 1500 → 3500 ms together with `BRIDGE_MAX_WORDS` so a 4-second
/// "Den er litt sånn …" mid-sentence fragment can be absorbed when
/// it's clearly the surrounding speaker — without sweeping up true
/// short replies delivered slowly (those usually run past 3500 ms).
const BRIDGE_MAX_DURATION_MS: u64 = 3_500;

/// Collapse short cross-speaker interjections into the surrounding
/// speaker run. The diarizer (FluidAudio community-1 + VBx) sometimes
/// flips speaker mid-utterance for a brief backchannel — a 0.4 s
/// "yeah" inside a longer monologue gets its own segment, the
/// word-level split in `split_by_segments` honours that, and the user
/// ends up with `Speaker 1: ... Speaker 2: yeah Speaker 1: ...` cut
/// across three lines. Tuning the diarizer's `minDurationOn` upstream
/// reduces the noise floor; this pass cleans up what slips through.
///
/// Rule: a piece P is rewritten to its neighbours' label when
///   - P has a label,
///   - the previous labelled piece and the next labelled piece both
///     have the same label as each other, different from P's,
///   - P has word timestamps available (so we can measure duration),
///   - P's text has at most `BRIDGE_MAX_WORDS` words AND its acoustic
///     span is at most `BRIDGE_MAX_DURATION_MS`.
///
/// Pieces with no word timestamps are skipped: that's the OpenAI
/// transcribe path and older diagnostic JSONs, where we can't tell a
/// real short turn from a backchannel and the safer default is to
/// leave the speaker boundary in place.
///
/// Pieces with no label (the graceful-degrade path when diarize emits
/// nothing) are passed over when finding the "previous"/"next"
/// labelled neighbour, so they don't break the sandwich pattern.
fn bridge_short_interjections(pieces: &mut [LabelledPiece]) {
    if pieces.len() < 3 {
        return;
    }
    for i in 1..pieces.len() - 1 {
        let cur_label = match &pieces[i].label {
            Some(l) => l.clone(),
            None => continue,
        };
        if pieces[i].text.split_whitespace().count() > BRIDGE_MAX_WORDS {
            continue;
        }
        let words = &pieces[i].words;
        if words.is_empty() {
            continue;
        }
        let span_ms = words
            .last()
            .map(|w| w.end_ms)
            .unwrap_or(0)
            .saturating_sub(words.first().map(|w| w.start_ms).unwrap_or(0));
        if span_ms > BRIDGE_MAX_DURATION_MS {
            continue;
        }
        let prev_label = pieces[..i]
            .iter()
            .rev()
            .find_map(|p| p.label.clone());
        let next_label = pieces[i + 1..]
            .iter()
            .find_map(|p| p.label.clone());
        let (Some(prev), Some(next)) = (prev_label, next_label) else {
            continue;
        };
        if prev != next || prev == cur_label {
            continue;
        }
        pieces[i].label = Some(prev);
    }
}

/// Maximum chain length to consider for text-continuation absorption.
/// Bounds the blast radius if the algorithm latches onto a long
/// legitimate cross-speaker exchange that happens to alternate without
/// clean punctuation breaks. Six pieces covers every mis-attribution
/// chain we've observed in dogfooded recordings.
const CONTINUATION_CHAIN_MAX_LEN: usize = 6;

/// Whether `text`'s last meaningful character is a sentence terminator
/// (`.!?:;`). Trailing ellipsis ("..." or Unicode "…") is stripped first
/// because Whisper uses them to mark trailing-off mid-utterance, not
/// sentence end — a distinction `ends_sentence` deliberately ignores
/// since its callers (the text-level merge passes) want to be
/// conservative about fusing across any pause cue.
fn piece_ends_terminator(text: &str) -> bool {
    let mut trimmed = text.trim_end();
    loop {
        let prev_len = trimmed.len();
        trimmed = trimmed.trim_end_matches('…').trim_end();
        if trimmed.ends_with("...") {
            trimmed = trimmed[..trimmed.len() - 3].trim_end();
        }
        if trimmed.len() == prev_len {
            break;
        }
    }
    let core = trimmed
        .trim_end_matches(|c: char| matches!(c, '"' | '\'' | ')' | ']' | '»' | '”' | '’'));
    matches!(
        core.chars().last(),
        Some('.') | Some('!') | Some('?') | Some(':') | Some(';')
    )
}

/// Whether `text` opens with a continuation cue — lowercase letter or
/// leading ellipsis. Pairs with `piece_ends_terminator` to detect when
/// two consecutive pieces are one sentence even though the diarizer
/// gave them different labels.
fn piece_starts_continuation(text: &str) -> bool {
    let trimmed = text.trim_start();
    if trimmed.starts_with("...") || trimmed.starts_with('…') {
        return true;
    }
    trimmed
        .chars()
        .next()
        .map(|c| c.is_lowercase())
        .unwrap_or(false)
}

/// Collapse "text continuation chains" — consecutive pieces where each
/// pair signals "still one sentence" via text cues (previous ends
/// without a terminator AND current starts with a lowercase letter or
/// ellipsis). Within such a chain, if labels disagree, one wins and
/// the whole chain is relabelled to it.
///
/// Winner selection:
///   1. If the pieces immediately before and after the chain share a
///      label, AND that label appears somewhere in the chain, use it.
///      This catches the A-B-A-long-fragment pattern: surrounding
///      Speaker 1 context outvotes the mis-attributed middle even
///      when the middle has more words.
///   2. Otherwise the label with the highest total word count in the
///      chain wins. The longer text is the more reliably-diarized
///      side, so when context can't break the tie we trust word
///      density.
///
/// Complements `bridge_short_interjections`, which only fires on
/// strict A-B-A sandwiches with B short and brief. This rule catches:
///   - A-B-A with B long (multi-word mid-sentence fragments the
///     acoustic bridge rejects on word/duration limits)
///   - A-B-C with no clean acoustic sandwich but clear textual flow
///   - Longer chains where Sortformer alternated labels piece-by-piece
///     through a single speaker's utterance
///
/// Trades a small risk of merging a legitimate cross-speaker
/// interruption (the interrupter happens to finish the original
/// speaker's sentence) for substantial reduction of single-speaker
/// fragmentation. Acceptable because clean interruptions usually
/// carry their own punctuation cues; we only act when the text says
/// "still one sentence."
fn absorb_text_continuation_chains(pieces: &mut [LabelledPiece]) {
    if pieces.len() < 2 {
        return;
    }
    let mut i = 0;
    while i < pieces.len() {
        let mut end = i;
        while end + 1 < pieces.len()
            && end - i + 1 < CONTINUATION_CHAIN_MAX_LEN
            && !piece_ends_terminator(&pieces[end].text)
            && piece_starts_continuation(&pieces[end + 1].text)
        {
            end += 1;
        }
        if end > i {
            let pre_label = if i > 0 {
                pieces[i - 1].label.clone()
            } else {
                None
            };
            let post_label = if end + 1 < pieces.len() {
                pieces[end + 1].label.clone()
            } else {
                None
            };
            let mut word_count_by_label: std::collections::HashMap<String, usize> =
                std::collections::HashMap::new();
            for k in i..=end {
                if let Some(label) = &pieces[k].label {
                    let wc = pieces[k].text.split_whitespace().count();
                    *word_count_by_label.entry(label.clone()).or_insert(0) += wc;
                }
            }
            if word_count_by_label.len() >= 2 {
                let chosen = pre_label
                    .as_ref()
                    .filter(|l| post_label.as_ref() == Some(l))
                    .filter(|l| word_count_by_label.contains_key(l.as_str()))
                    .cloned()
                    .or_else(|| {
                        word_count_by_label
                            .iter()
                            .max_by_key(|(_, &c)| c)
                            .map(|(k, _)| k.clone())
                    });
                if let Some(winner) = chosen {
                    for k in i..=end {
                        if pieces[k].label.is_some() {
                            pieces[k].label = Some(winner.clone());
                        }
                    }
                }
            }
        }
        i = end + 1;
    }
}

/// Rebuild the transcript by walking chunks in chronological order and
/// emitting each one prefixed with its assigned label. Same-label runs
/// get a single space between chunks (continuation); label changes get
/// a newline + new prefix. Chunks the labeller declines to label
/// (returns `None`) get joined to whatever came before them with a
/// space, no prefix change — typically only happens when diarize
/// produces zero segments and we're degrading gracefully.
///
/// Chronological ordering uses `(start_ms, source)`. Mic and system
/// chunks each carry start_ms relative to their own stream's first
/// frame — close to but not exactly the same as global wall time
/// (the streams start within a few hundred ms of each other). The
/// tie-break preferring `Mic` reflects the typical UX assumption that
/// the user speaks first; in practice the imprecision is well below
/// the threshold a reader would notice.
///
/// Cross-stream echo dedup runs first via `dedup_mic_against_sys` —
/// see that function for details. Short cross-speaker interjections
/// are absorbed into the surrounding speaker via
/// `bridge_short_interjections` before emit.
/// Pure pipeline up to (but not including) `bridge_short_interjections`.
/// Extracted so the diagnostic dump can capture the same pre-bridge piece
/// sequence the transcript emitter walks, without duplicating the
/// dedup/sort/split logic.
fn build_pieces_unbridged(
    chunks: &[ChunkRecord],
    split_chunk: &(dyn Fn(&ChunkRecord) -> Vec<LabelledPiece> + Send),
) -> Vec<LabelledPiece> {
    let kept = dedup_mic_against_sys(chunks);
    let mut sorted: Vec<&ChunkRecord> = kept.iter().collect();
    sorted.sort_by_key(|c| {
        let source_rank = match c.source {
            ChunkSource::Mic => 0,
            ChunkSource::Sys => 1,
        };
        (c.start_ms, source_rank)
    });

    let mut all_pieces: Vec<LabelledPiece> = Vec::new();
    for chunk in sorted {
        for piece in split_chunk(chunk) {
            if piece.text.trim().is_empty() {
                continue;
            }
            all_pieces.push(piece);
        }
    }
    all_pieces
}

fn build_labelled_transcript(
    chunks: &[ChunkRecord],
    split_chunk: &(dyn Fn(&ChunkRecord) -> Vec<LabelledPiece> + Send),
) -> String {
    let mut all_pieces = build_pieces_unbridged(chunks, split_chunk);
    bridge_short_interjections(&mut all_pieces);
    absorb_text_continuation_chains(&mut all_pieces);

    let mut output = String::new();
    let mut last_label: Option<String> = None;

    for piece in all_pieces {
        let trimmed = piece.text.trim();
        if trimmed.is_empty() {
            continue;
        }
        match &piece.label {
            Some(label) => {
                if last_label.as_deref() != Some(label.as_str()) {
                    if !output.is_empty() {
                        output.push('\n');
                    }
                    output.push_str(&format!("{label}: "));
                    last_label = Some(label.clone());
                } else {
                    output.push(' ');
                }
            }
            None => {
                if !output.is_empty() {
                    output.push(' ');
                }
            }
        }
        output.push_str(trimmed);
    }
    merge_run_on_sentences(&output)
}

/// Strip a leading `<label>: ` from a transcript line, returning the
/// remainder. Mirrors the format `build_labelled_transcript` emits, so
/// the merge pass can drop the absorbed line's prefix when joining.
fn strip_label_prefix(line: &str) -> &str {
    let trimmed = line.trim_start();
    if let Some(colon) = trimmed.find(':') {
        let label = &trimmed[..colon];
        if !label.is_empty() && label.len() <= 40 && !label.contains('\n') {
            let rest = &trimmed[colon + 1..];
            return rest.trim_start();
        }
    }
    line
}

/// Whether `line` ends with a sentence-terminating punctuation mark,
/// allowing trailing close-quote/close-paren after the terminator
/// (`he said.` / `(yes!)` / `"done."`). Trailing whitespace is ignored.
///
/// Em-dash (`—`), en-dash (`–`), and a final ASCII hyphen are also
/// treated as terminators. They mark a speaker trailing off mid-thought
/// or being interrupted — fusing across them would join two distinct
/// turns into nonsense.
fn ends_sentence(line: &str) -> bool {
    let trimmed = line.trim_end();
    let cleaned: String = trimmed
        .chars()
        .rev()
        .take_while(|c| matches!(c, '"' | '\'' | ')' | ']' | '»' | '”' | '’' | ' '))
        .collect();
    let len = cleaned.chars().count();
    let mut chars = trimmed.chars();
    let total = trimmed.chars().count();
    if total == 0 {
        return false;
    }
    let target_idx = total.saturating_sub(len + 1);
    let last_meaningful = chars.nth(target_idx);
    matches!(
        last_meaningful,
        Some('.') | Some('!') | Some('?') | Some(':') | Some(';')
            | Some('…') | Some('—') | Some('–') | Some('-')
    )
}

/// Whether `line` (after any `<label>: ` prefix) begins with a
/// lowercase letter — the textual cue that this line is the
/// continuation of the previous line's clause rather than a fresh
/// sentence. Lines that start with a digit, opening quote, or
/// punctuation (parenthetical, em-dash) don't qualify; we want a
/// strong signal.
fn starts_lowercase(line: &str) -> bool {
    let body = strip_label_prefix(line);
    body.chars()
        .next()
        .map(|c| c.is_lowercase())
        .unwrap_or(false)
}

/// Maximum word count for a "boundary fragment" that the diarizer
/// likely mis-attributed across the speaker line. 1–3 words covers the
/// real-world failure mode (Whisper emitted "...way. It" and the next
/// chunk started "does pay off..."; or "...Brand" + "new. Hadn't done
/// anything..."). Going wider risks moving real content between
/// speakers.
const BOUNDARY_FRAGMENT_MAX_WORDS: usize = 3;

/// Split a transcript line into `(label_with_separator, body)`. The
/// label part includes the leading whitespace, the label text, the
/// `:`, and the space after it — so concatenating the two halves
/// reproduces the original line exactly. Returns `("", line)` when
/// there's no recognisable label prefix (defensive — production
/// transcripts always have one, but the merge passes also run on
/// hand-edited text).
fn split_label_prefix(line: &str) -> (&str, &str) {
    let trimmed = line.trim_start();
    let leading_ws_len = line.len() - trimmed.len();
    if let Some(colon) = trimmed.find(':') {
        let label = &trimmed[..colon];
        if !label.is_empty() && label.len() <= 40 && !label.contains('\n') {
            let after_colon = &trimmed[colon + 1..];
            let body_offset = colon + 1 + (after_colon.len() - after_colon.trim_start().len());
            let total_offset = leading_ws_len + body_offset;
            return (&line[..total_offset], &line[total_offset..]);
        }
    }
    ("", line)
}

/// Byte offset of the *last* sentence-terminator (`.!?…`) in `s`, or
/// `None` if there isn't one. Used to find the boundary between the
/// last finished sentence on a line and any trailing fragment.
/// Excludes `:;` (more often mid-clause than sentence-final) and
/// dash variants (those signal trailing-off, not a clean sentence
/// boundary we want to split on).
fn last_terminator_index(s: &str) -> Option<usize> {
    s.char_indices()
        .filter(|(_, c)| matches!(c, '.' | '!' | '?' | '…'))
        .map(|(i, _)| i)
        .last()
}

/// Byte offset of the *first* sentence-terminator (`.!?…`) in `s`,
/// or `None` if there isn't one.
fn first_terminator_index(s: &str) -> Option<usize> {
    s.char_indices()
        .find(|(_, c)| matches!(c, '.' | '!' | '?' | '…'))
        .map(|(i, _)| i)
}

/// Length in bytes of the character starting at byte index `i` in `s`.
/// Used to step *past* a terminator character we located via
/// `last_terminator_index` / `first_terminator_index`.
fn char_len_at(s: &str, i: usize) -> usize {
    s[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(0)
}

/// Detect "the diarizer attributed the *next* speaker's first 1–3
/// words to the *previous* speaker's turn" — a common Whisper-Word
/// alignment artefact when a sentence terminator lands mid-chunk and
/// the next sentence's opening words spill into the same chunk before
/// the speaker change actually occurs.
///
/// Pattern (after stripping speaker labels):
/// - `prev` body ends with `<sentence>. <1–3 capitalised words>`
///   where the trailing words contain *no* terminator of their own
///   (they're the start of a new sentence, not its end).
/// - `next` body starts with a *lowercase* letter (the continuation
///   that confirms those trailing words begin the same sentence).
///
/// Fix: trim the trailing words from `prev`, prepend them to `next`'s
/// body. Speaker labels are preserved on both lines.
fn forward_move_trailing(prev: &str, next: &str) -> Option<(String, String)> {
    let (prev_label, prev_body) = split_label_prefix(prev);
    let (next_label, next_body) = split_label_prefix(next);

    // Bail on empty / label-only lines.
    if prev_body.trim().is_empty() || next_body.trim().is_empty() {
        return None;
    }

    // The trailing fragment lives after the LAST `.!?…` in prev.
    let term_idx = last_terminator_index(prev_body)?;
    let after_term_byte = term_idx + char_len_at(prev_body, term_idx);
    let trailing = prev_body.get(after_term_byte..)?.trim();
    if trailing.is_empty() {
        return None;
    }

    let trailing_words: Vec<&str> = trailing.split_whitespace().collect();
    if trailing_words.is_empty() || trailing_words.len() > BOUNDARY_FRAGMENT_MAX_WORDS {
        return None;
    }

    // First trailing word must be a sentence start (capitalised). If
    // it's lowercase, this isn't a "next-speaker's-sentence-start"
    // pattern — the full merge below will catch it instead.
    let first_word = *trailing_words.first()?;
    let first_char = first_word.chars().next()?;
    if !first_char.is_uppercase() {
        return None;
    }

    // None of the trailing words may already contain a terminator —
    // that would mean the trailing fragment is itself a complete
    // sentence, which probably belongs to prev as the speaker's last
    // word, not to next.
    if trailing_words
        .iter()
        .any(|w| w.chars().any(|c| matches!(c, '.' | '!' | '?' | '…')))
    {
        return None;
    }

    // next must start lowercase — confirms the trailing fragment is
    // the start of a sentence the next speaker continues.
    if !starts_lowercase(next) {
        return None;
    }

    // Build new prev: keep through the terminator, drop the trailing
    // fragment.
    let kept = prev_body
        .get(..after_term_byte)?
        .trim_end()
        .to_string();
    let new_prev = format!("{prev_label}{kept}");

    // Build new next: prepend trailing fragment to existing body.
    let trailing_joined = trailing_words.join(" ");
    let new_next = format!("{next_label}{trailing_joined} {next_body}");

    Some((new_prev, new_next))
}

/// Detect "the diarizer attributed the *previous* speaker's last 1–3
/// words to the *next* speaker's turn" — the inverse of
/// `forward_move_trailing`.
///
/// Pattern (after stripping speaker labels):
/// - `prev` body ends *without* a sentence terminator (the speaker
///   was mid-clause when the chunk boundary fell).
/// - `next` body starts with `<1–3 words><terminator> <more
///   content>` — i.e. a short fragment that closes a sentence,
///   followed by genuinely new content that's clearly the next
///   speaker's turn.
///
/// Fix: detach the leading fragment (up to and including the first
/// terminator) from `next`, append it to `prev`. Speaker labels are
/// preserved on both lines.
fn backward_move_leading(prev: &str, next: &str) -> Option<(String, String)> {
    let (prev_label, prev_body) = split_label_prefix(prev);
    let (next_label, next_body) = split_label_prefix(next);

    if prev_body.trim().is_empty() || next_body.trim().is_empty() {
        return None;
    }

    // prev must be open (no terminator at end).
    if ends_sentence(prev) {
        return None;
    }

    // next's body must contain a terminator within the first 1–3
    // words.
    let term_idx = first_terminator_index(next_body)?;
    let term_end = term_idx + char_len_at(next_body, term_idx);
    let leading = next_body.get(..term_end)?.trim();
    let trailing = next_body.get(term_end..)?.trim_start();

    let leading_words: Vec<&str> = leading.split_whitespace().collect();
    if leading_words.is_empty() || leading_words.len() > BOUNDARY_FRAGMENT_MAX_WORDS {
        return None;
    }

    // There must be content after the terminator — otherwise we'd be
    // stripping `next` entirely, which is what `merge_run_on_sentences`
    // already does (and gets the speaker attribution wrong, which is
    // why this targeted move exists).
    if trailing.is_empty() {
        return None;
    }

    // Build new prev: append leading fragment to existing body.
    let new_prev = format!("{prev_label}{} {leading}", prev_body.trim_end());

    // Build new next: just the trailing portion, with its label.
    let new_next = format!("{next_label}{trailing}");

    Some((new_prev, new_next))
}

/// Detect a stutter / immediate-repetition signal at a line boundary.
///
/// When `prev` body has format `<earlier text>... <X> <terminator>
/// <Y>` and `next` body has format `<Z> <terminator> <rest>`, the
/// phrase `Y Z` may form a unit. If that exact phrase already appears
/// in `prev` *before* the trailing `Y`, the speaker stuttered or
/// repeated themselves — `Y Z` is the second occurrence, all on the
/// same speaker's line. That's the cue to prefer backward-move (pull
/// `Z` back to prev) over forward-move (push `Y` forward to next).
///
/// Without this signal, the AMC-style case ("AMC was a young company.
/// Brand new. Brand" + "new. Hadn't done anything...") and the "It
/// does pay off" case look identical to the structural rules — both
/// have a 1-word trailing in prev and a 1-word leading in next. Only
/// the repetition check separates them.
fn has_repetition_signal(prev: &str, next: &str) -> bool {
    let (_, prev_body) = split_label_prefix(prev);
    let (_, next_body) = split_label_prefix(next);

    let prev_term = match last_terminator_index(prev_body) {
        Some(i) => i,
        None => return false,
    };
    let after_prev_term = prev_term + char_len_at(prev_body, prev_term);
    let trailing = match prev_body.get(after_prev_term..) {
        Some(t) => t.trim(),
        None => return false,
    };
    if trailing.is_empty() {
        return false;
    }

    let next_term = match first_terminator_index(next_body) {
        Some(i) => i,
        None => return false,
    };
    let leading = match next_body.get(..next_term) {
        Some(l) => l.trim(),
        None => return false,
    };
    if leading.is_empty() {
        return false;
    }

    let search_zone = match prev_body.get(..after_prev_term) {
        Some(z) => z.to_lowercase(),
        None => return false,
    };
    let combined = format!("{trailing} {leading}").to_lowercase();
    search_zone.contains(&combined)
}

/// Heuristic chunk-boundary cleanup. Three operations applied at
/// every adjacent line pair, with the choice between forward and
/// backward moves disambiguated by a repetition-signal tiebreaker:
///
/// 1. **Forward-move** (`forward_move_trailing`): the next speaker's
///    sentence-opening words got attributed to the previous speaker.
///    Trim them off prev, prepend to next.
/// 2. **Backward-move** (`backward_move_leading`): the previous
///    speaker's sentence-closing words got attributed to the next
///    speaker. Trim them off next, append to prev.
/// 3. **Full merge**: prev ends without a terminator AND next starts
///    lowercase, but neither (1) nor (2) applied. Fuse next's body
///    into prev (its label is dropped). Common pattern: Whisper
///    chopped a single sentence across chunks and the diarizer
///    flipped speaker on the second half.
///
/// When forward and backward both qualify, the choice is made by
/// `has_repetition_signal`: if the trailing-of-prev + leading-of-next
/// phrase already appears earlier in prev, the speaker repeated
/// themselves and the words belong on prev (backward); otherwise the
/// trailing was misattributed and the words belong on next (forward).
///
/// All three preserve the textual signal as the safety check —
/// Whisper at a real speaker boundary almost always emits a
/// terminator AND capitalises the next sentence's first word, so
/// when neither holds the boundary is almost certainly a chunk
/// artefact rather than a real turn change.
///
/// Runs after `bridge_short_interjections` (which handles same-line
/// short interjections inside a single chunk), catching what the
/// structural sandwich rule can't.
fn merge_run_on_sentences(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut merged: Vec<String> = Vec::with_capacity(lines.len());
    for line in lines {
        if let Some(prev) = merged.last_mut() {
            let forward = forward_move_trailing(prev, line);
            let backward = backward_move_leading(prev, line);
            let chosen = match (forward, backward) {
                (Some(f), Some(b)) => {
                    if has_repetition_signal(prev, line) {
                        Some(b)
                    } else {
                        Some(f)
                    }
                }
                (Some(f), None) => Some(f),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            };
            if let Some((new_prev, new_next)) = chosen {
                *prev = new_prev;
                merged.push(new_next);
                continue;
            }
            if !ends_sentence(prev) && starts_lowercase(line) {
                let stripped = strip_label_prefix(line);
                prev.push(' ');
                prev.push_str(stripped);
                continue;
            }
        }
        merged.push(line.to_string());
    }
    merged.join("\n")
}

async fn transcribe_chunk(
    app: AppHandle,
    note_id: String,
    source: ChunkSource,
    path: PathBuf,
    start_ms: u64,
) -> anyhow::Result<()> {
    // Resolve dispatch-time data: per-note-or-global language first
    // (drives per-language override resolution), then the resolved
    // provider config, then custom vocabulary + API key. The order
    // matters — provider_cfg depends on language for the resolve.
    let (language, vocabulary) = {
        let state: State<AppState> = app.state();
        let conn = state.db.lock();
        let global_language = db::get_setting(&conn, "language")?
            .unwrap_or_else(|| DEFAULT_LANGUAGE.to_string());
        let note_language = db::get_note(&conn, &note_id)
            .map(|n| n.language)
            .unwrap_or_default();
        let language = if note_language.trim().is_empty() {
            global_language
        } else {
            note_language
        };
        let vocabulary = db::get_setting(&conn, "custom_vocabulary")?
            .unwrap_or_default();
        (language, vocabulary)
    };
    let provider_cfg = {
        let state: State<AppState> = app.state();
        read_transcribe_config(&state)?.resolve(&language).clone()
    };
    // Look up the API key for the *active* provider, not just OpenAI.
    // Phase 1 had only one cloud provider so a single Keychain slot was
    // sufficient; Phase 2 added Deepgram/Groq with their own slots, and
    // sending OpenAI's key to Deepgram naturally returns 401.
    let api_key = match provider_cfg.provider_id() {
        "local" => None,
        provider_id => {
            let state: State<AppState> = app.state();
            Some(
                read_provider_api_key(&state, provider_id)
                    .map_err(|e| anyhow::anyhow!("{e}"))?
                    .ok_or_else(|| {
                        anyhow::anyhow!("no API key stored for provider '{provider_id}'")
                    })?,
            )
        }
    };

    // Skip near-silent chunks. Whisper and gpt-4o-transcribe both hallucinate
    // confident text (often in the wrong language) when fed silence. The WAV
    // chunks are 16kHz mono 16-bit PCM little-endian — read the data section
    // and compute RMS in [0, 1]. Threshold is user-tunable so noisy
    // environments (HVAC, mic hiss) can crank it up to drop borderline
    // chunks before they reach Whisper.
    let rms_floor = {
        let state: State<AppState> = app.state();
        let conn = state.db.lock();
        db::get_setting(&conn, "silence_rms_threshold")
            .ok()
            .flatten()
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or(DEFAULT_SILENCE_RMS_THRESHOLD)
    };
    if let Ok(rms) = wav::rms(&path).await {
        // Pure silence ~0.0001, room tone ~0.001, mic hiss ~0.003,
        // soft speech 0.005+. Threshold doc lives at the constant
        // (DEFAULT_SILENCE_RMS_THRESHOLD) — see top of file.
        if rms < rms_floor {
            return Ok(());
        }
    }

    // Serialize transcription per session: each chunk's initial_prompt must
    // see the *committed* trail of every prior chunk. With parallel
    // transcribes, two back-to-back chunks both grab the same stale snapshot
    // and the trail's quality benefit collapses. Sequential trades a little
    // throughput (chunks queue if inference is slow) for accurate context.
    let gate = {
        let state: State<AppState> = app.state();
        state.transcribe_gate.clone()
    };
    let _guard = gate.lock().await;

    // Whisper's `initial_prompt` slot conditions decoding on prior context.
    // We compose two parts: the user's custom vocabulary (proper-noun bias)
    // and a snapshot of the last ~150 committed words from THIS source's
    // stream. Per-source trails because the mic and system streams are
    // separate conversations — sharing one trail would pull a mic chunk's
    // decode toward remote-side vocabulary (or vice versa) and cause
    // language drift on bilingual calls.
    let trail_snapshot = {
        let state: State<AppState> = app.state();
        let session = state.recording.lock();
        let trail = match source {
            ChunkSource::Mic => session.mic_trail.lock(),
            ChunkSource::Sys => session.sys_trail.lock(),
        };
        trail.as_prompt()
    };
    // Vocabulary is stored as a newline-or-comma-separated string. Split
    // into individual terms for the bias_terms field. Drop short tokens
    // (< 3 chars) — they create false positives in every provider's
    // keyword/prompt biaser ("am" appearing where the user said "an"
    // etc.).
    let vocab_terms: Vec<&str> = vocabulary
        .split(|c: char| c == '\n' || c == ',')
        .map(str::trim)
        .filter(|s| s.len() >= 3)
        .collect();

    // Build the right STT adapter for this chunk. Local Whisper needs
    // runtime state (shared model context, resolved model file path, GPU
    // flag) that can't live in `ProviderConfig`; we resolve it here and
    // pass it to the registry. Both providers feed the same `Word` shape
    // downstream so the timeline serialiser can rebase chunk-relative ms
    // onto the playback clock the same way regardless of source.
    let local_deps = if let crate::stt::ProviderConfig::Local(local_cfg) = &provider_cfg {
        let model_path = local_model_path(&app, &language, &local_cfg.model_id)
            .map_err(|e| anyhow::anyhow!(e))?;
        let shared = {
            let state: State<AppState> = app.state();
            state.whisper.clone()
        };
        Some(crate::stt::LocalDeps {
            shared,
            model_path,
            use_gpu: local_cfg.use_gpu,
        })
    } else {
        None
    };
    let adapter = crate::stt::build_adapter(&provider_cfg, local_deps);
    let ctx = crate::stt::TranscribeCtx {
        model: provider_cfg.model(),
        language: &language,
        bias_terms: &vocab_terms,
        prior_context: trail_snapshot.as_deref(),
        api_key: api_key.as_deref(),
        base_url: provider_cfg.base_url(),
    };
    let crate::stt::TranscribeResult { text, words, detected_language } =
        adapter.transcribe(ctx, &path).await?;
    if is_likely_hallucination(&text, &language) {
        return Ok(());
    }
    // NOTE: no word-timing collapse check here, deliberately. Degenerate
    // timings are common in real speech from the local Whisper models, so the
    // signal is only usable together with "this stream is statistically noise"
    // — and mid-recording we don't yet know each stream's share of the whole.
    // That filter therefore runs post-stop, in
    // `drop_incidental_stream_hallucinations`.
    // Drop chunks dominated by N-gram repetition (Whisper collapse). Letting
    // them land in the transcript is bad on its own, but worse: the trail-
    // prompt feeds the loop forward into the next chunk's `initial_prompt`
    // and the loop self-sustains for the rest of the recording.
    if is_repetition_collapse(&text) {
        eprintln!("transcribe: dropping repetition-collapsed chunk");
        return Ok(());
    }
    // Whisper was trained on closed-caption data and frequently appends
    // subtitle attribution ("Undertekster av Ai-Media", "Subtitles by Amara",
    // "Thanks for watching") at the end of real speech. Trim those tails.
    let text = strip_attribution_tail(&text);
    let trimmed = text.trim().to_string();
    if trimmed.is_empty() {
        return Ok(());
    }

    // Cross-chunk hallucination loop guard — MIC ONLY. Whisper
    // occasionally locks onto a confident-sounding phrase when the
    // audio is low-SNR (e.g. an internal MacBook mic during silence
    // between sentences). Each chunk on its own passes the per-chunk
    // repetition-collapse filter because the phrase appears only
    // once internally — but the same phrase comes back chunk after
    // chunk, fed forward by the trail context.
    //
    // We use Jaccard similarity (|A ∩ B| / |A ∪ B|) at a strict 0.85
    // threshold so legitimate continuations survive: a chunk with
    // even a few new unique words drops well below 0.85, while exact
    // / near-exact repeats from a hallucination loop score ≥0.95.
    // Earlier iteration used containment with min-denominator and a
    // 0.7 threshold; that caught loops but also dropped real content
    // when chunk N+1 was a strict superset of chunk N.
    //
    // Sys excluded entirely: clean source audio rarely hallucinates,
    // and consecutive sys chunks legitimately share lots of
    // vocabulary on continuing topics.
    if source == ChunkSource::Mic {
        let state: State<AppState> = app.state();
        let session = state.recording.lock();
        let log = session.chunk_log.lock();
        let recent_same_source = log
            .iter()
            .rev()
            .find(|c| c.source == source)
            .map(|c| c.text.clone());
        drop(log);
        drop(session);
        if let Some(prev) = recent_same_source {
            let new_tokens = normalize_tokens(&trimmed);
            if new_tokens.len() >= 3 {
                let prev_tokens = normalize_tokens(&prev);
                if token_jaccard(&new_tokens, &prev_tokens) >= 0.85 {
                    eprintln!(
                        "transcribe: dropping cross-chunk repeat (likely hallucination loop): {trimmed}"
                    );
                    return Ok(());
                }
            }
        }
    }

    // Speaker prefixes are added by the offline diarization pass on
    // recording_stop, not here. Per-chunk live diarization performed
    // poorly on long recordings (clustering drifts as speaker memory
    // accumulates), so chunks are appended as plain text and the full
    // transcript is rewritten with proper labels after stop, when
    // FluidAudio can cluster across the entire audio at once and we can
    // assign "You" to mic chunks vs diarized speakers to system chunks.
    //
    // The live-display transcript appends in arrival order regardless of
    // source. Mic and sys chunks may interleave slightly out of strict
    // wall-clock order during recording, but `diarize_and_apply` rebuilds
    // the transcript from the chunk log sorted by (source, start_ms) at
    // stop time, so the saved transcript ends up properly ordered.
    let state: State<AppState> = app.state();

    // Push to chunk_log unconditionally — even when the session has
    // been cleared by recording_stop. The post-stop chain's
    // diarize_and_apply rebuilds the transcript from chunk_log, so a
    // tail chunk that finished decoding after recording_stop still gets
    // included. Without this, the last 5–20 s of audio (any in-flight
    // transcribe completing after stop) would silently vanish — the
    // bug user reported.
    //
    // Words come from local Whisper only and arrive with chunk-
    // relative timestamps (whisper timed against this chunk's WAV,
    // which starts at t=0 from its own perspective). The playback
    // view adds chunk.start_ms back when it needs absolute time.
    let chunk_words: Vec<crate::recording::ChunkWord> = words
        .into_iter()
        .map(|w| crate::recording::ChunkWord {
            text: w.text,
            start_ms: w.start_ms,
            end_ms: w.end_ms,
        })
        .collect();
    {
        let session = state.recording.lock();
        session.chunk_log.lock().push(ChunkRecord {
            source,
            start_ms,
            text: trimmed.clone(),
            words: chunk_words,
            detected_language,
        });
    }

    // Live-update guard. The provider call above (whisper / openai)
    // can take long enough that recording_stop fires while we're
    // still awaiting it. If the session has been cleared (note_id
    // taken in recording_stop) or replaced (user started a new
    // recording), skip the live DB append + trail update + UI emit
    // — diarize_and_apply will rebuild the saved transcript from
    // chunk_log shortly. Without this guard, a stale db::append
    // could land on top of the post-stop labelled transcript.
    {
        let session = state.recording.lock();
        if session.note_id.as_deref() != Some(&note_id) {
            eprintln!(
                "transcribe: session inactive, chunk preserved in log for post-stop"
            );
            return Ok(());
        }
    }
    let updated_transcript = {
        let conn = state.db.lock();
        db::append_transcript(&conn, &note_id, &trimmed, " ")?
    };
    {
        let session = state.recording.lock();
        let mut trail = match source {
            ChunkSource::Mic => session.mic_trail.lock(),
            ChunkSource::Sys => session.sys_trail.lock(),
        };
        trail.push(&trimmed);
    }
    let _ = app.emit(
        "transcript_replaced",
        TranscriptPayload {
            note_id: note_id.clone(),
            text: updated_transcript,
        },
    );
    Ok(())
}

pub(crate) fn emit_status(app: &AppHandle, note_id: Option<&str>, phase: Phase) {
    let _ = app.emit("recording_status", RecordingStatus {
        note_id: note_id.map(|s| s.to_string()),
        phase,
    });
    // The tray's icon and its Start/Stop items are driven from here rather than
    // from each call site: this is the one funnel every phase change already
    // goes through, so the menu bar can't drift out of step with the pipeline.
    crate::menubar::on_phase(app, phase);
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

// Whisper's training data contained millions of subtitle files, so it
// regularly appends "Subtitles by …" / "Undertekster av …" / "Thanks for
// watching" at the end of real speech. If we see one of these markers
// anywhere in the text, strip it back to the preceding sentence boundary.
fn strip_attribution_tail(text: &str) -> String {
    // Triggers are ASCII so to_ascii_lowercase keeps byte offsets aligned
    // with the original string for slicing.
    let lower = text.to_ascii_lowercase();
    const TRIGGERS: &[&str] = &[
        // Norwegian/Scandinavian subtitle credits. Whisper memorised whole
        // sign-off phrases from broadcast subtitles, so each verb form needs
        // its own trigger — past-participle ("tekstet"), gerund ("teksting"),
        // and noun form ("tekster") all show up in the wild.
        "undertekster av",
        "undertekstet av",
        "tekstet av",
        "tekster av",
        "teksting av",
        "norske tekster",
        "oversatt av",
        "oversettelse av",
        // English subtitle credits
        "subtitles by",
        "subtitled by",
        "captions by",
        "captioning by",
        "closed captions",
        "translation by",
        "translated by",
        "transcribed by",
        "amara.org",
        "ai-media",
        // YouTube-style sign-offs
        "thanks for watching",
        "thank you for watching",
        "subscribe to",
        "like and subscribe",
        "see you next time",
        "see you in the next",
    ];
    let mut cut: Option<usize> = None;
    for trigger in TRIGGERS {
        if let Some(pos) = lower.rfind(trigger) {
            // Back up to the nearest sentence boundary before the trigger so
            // we drop the whole offending phrase, not just the trigger word.
            let start = text[..pos]
                .rfind(|c: char| matches!(c, '.' | '!' | '?' | '\n'))
                .map(|p| p + 1)
                .unwrap_or(pos);
            cut = Some(cut.map_or(start, |c| c.min(start)));
        }
    }
    match cut {
        Some(c) => text[..c].trim_end().to_string(),
        None => text.to_string(),
    }
}

// Whisper produces a small set of stock English phrases when fed silence
// regardless of the `language` parameter. Drop them when:
//   - the chunk is short (≤120 chars, typical of a hallucinated standalone
//     phrase) AND
//   - the chosen target language is not English (so we don't eat a real
//     English meeting that happens to say "thanks for watching this demo").
// We err on the side of keeping content; the silence gate above is the
// primary defense.
fn is_likely_hallucination(text: &str, _language: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return true;
    }
    // Punctuation-only output is always Whisper hallucinating on
    // silence: ".", "...", "*", standalone emoji, etc.
    if !t.chars().any(|c| c.is_alphanumeric()) {
        return true;
    }
    // Long output is real speech (or a real loop, which the
    // repetition-collapse filter handles separately).
    if t.len() > 120 {
        return false;
    }
    let lower = t.to_lowercase();
    // Caption-attribution patterns. Substring match because these
    // sometimes appear glued to the tail of real speech.
    const ATTRIBUTION_FRAGMENTS: &[&str] = &[
        "thanks for watching",
        "thank you for watching",
        "subscribe to",
        "subtitles by",
        "subtitled by",
        "amara.org",
        "transcribed by",
    ];
    if ATTRIBUTION_FRAGMENTS.iter().any(|f| lower.contains(f)) {
        return true;
    }
    // Short single-utterance silence hallucinations across major
    // languages. Whisper falls back to high-prior greeting / thanks
    // tokens when fed low-SNR audio; the user reported "Hei.",
    // "Takk!", "Hi." appearing repeatedly between real speech with
    // no audio actually containing those words. Match the
    // punctuation-stripped, whitespace-collapsed lowercase form
    // exactly so longer sentences containing these words pass
    // through unaffected.
    //
    // Deliberately NOT in the drop list: "yes/no/ja/nei/oui/non/ok"
    // — those are common real one-word answers, and dropping them
    // is more disruptive than the rare hallucination they'd catch.
    let normalized: String = t
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    const SHORT_HALLUCINATIONS: &[&str] = &[
        // EN greetings / thanks
        "hi", "hello", "bye", "thanks", "thank you", "okay", "yeah",
        // EN backchannels Whisper hallucinates on noise
        "mhm", "mhmm", "uh huh", "uhhuh", "mm", "mmm", "hm", "hmm",
        "ah", "oh", "right", "you",
        // NO/DA/SV
        "hei", "hej", "hallo", "takk", "tak", "tack", "ha det",
        // NO backchannels
        "ja vel", "mhm",
        // DE
        "danke", "tschuss", "tschüss",
        // FR
        "merci", "bonjour", "au revoir", "salut",
        // ES
        "hola", "gracias", "adios",
        // JA
        "ありがとう", "こんにちは", "さようなら",
    ];
    SHORT_HALLUCINATIONS.contains(&normalized.as_str())
}

/// Detect a chunk whose output is dominated by N-gram repetition — Whisper's
/// well-known low-SNR failure mode where one phrase decodes ≥3 consecutive
/// times. The contaminated chunk should be dropped; if it lands in the
/// transcript, the trail-prompt mechanism then feeds the loop into the next
/// chunk and the recording's tail becomes unrecoverable.
///
/// Heuristic: scan phrase lengths 1..=7. For each, look for the longest
/// run of consecutive identical (case-insensitive, punctuation-stripped)
/// occurrences. Flag the chunk when:
///   - some phrase repeats ≥4 times in a row, OR
///   - some phrase repeats ≥3 times AND covers ≥60% of the chunk's words.
///
/// The double rule keeps "yes yes yes" or "ja ja ja" mid-conversation from
/// being dropped — a 3-rep tiny chunk is plausibly real speech, but a 3-rep
/// run dominating a longer chunk is collapse.
/// Minimum run of consecutive words pinned to a single zero-length instant for
/// `is_timing_collapse` to fire.
///
/// **This is a weak signal on its own — never use it alone to drop content.**
/// Measured against a real 45-minute Norwegian recording (local NB Whisper):
/// four separate mic chunks of perfectly ordinary speech contain pinned runs of
/// 3, 4, 4 and **7**, longer than the 4 in the hallucinated chunk this was
/// written to catch. Degenerate timings are just how this model reports short
/// words and trailing tokens. Only meaningful in conjunction with a prior that
/// already suggests hallucination — see
/// `drop_incidental_stream_hallucinations`.
const TIMING_COLLAPSE_RUN: usize = 3;

/// Whether a chunk's word timings show the aligner giving up: consecutive words
/// landing on the *same* instant with zero duration, which is what happens when
/// the decoder emits high-prior text it has no audio to align against.
///
/// Corroborating evidence, not proof. See `TIMING_COLLAPSE_RUN` for the measured
/// false-positive rate on real speech.
fn is_timing_collapse(spans: &[(u64, u64)]) -> bool {
    let mut run = 1usize;
    for pair in spans.windows(2) {
        let (prev, cur) = (pair[0], pair[1]);
        let pinned_together = prev.0 == prev.1 && cur.0 == cur.1 && prev.0 == cur.0;
        if pinned_together {
            run += 1;
            if run >= TIMING_COLLAPSE_RUN {
                return true;
            }
        } else {
            run = 1;
        }
    }
    false
}

/// Drop hallucinated chunks from a stream that is statistically noise.
///
/// Two weak signals in conjunction, because neither is sufficient alone:
///
///  1. **The stream is incidental** — it contributed a negligible share of the
///     recording's chunks. One sys chunk in 157 is a notification chime or a few
///     seconds of video during an in-person meeting, not a side of a
///     conversation.
///  2. **The chunk's word timings collapsed** — see `is_timing_collapse`. On its
///     own this fires on real speech (runs of 7 measured on genuine mic chunks),
///     which is why it's only consulted for a stream already known to be noise.
///
/// Restricting to the incidental stream is what makes this safe: the stream
/// carrying the meeting is never touched, so no amount of timing weirdness in
/// real speech can delete it.
///
/// Runs *before* the capture-mode decision, because a hallucinated chunk's
/// damage isn't limited to its own line — a single one on an otherwise silent
/// system stream flips the recording into the hybrid branch and then invents a
/// phantom speaker for itself. Removing it lets an in-person recording be
/// recognised as one.
///
/// Chunks without word timings pass through untouched: some providers (the
/// current OpenAI path) don't return them, and absence of evidence isn't
/// evidence of collapse.
fn drop_incidental_stream_hallucinations(chunks: Vec<ChunkRecord>) -> Vec<ChunkRecord> {
    let total = chunks.len();
    if total == 0 {
        return chunks;
    }
    let mic_count = chunks.iter().filter(|c| c.source == ChunkSource::Mic).count();
    let sys_count = total - mic_count;
    let is_incidental = |source: ChunkSource| -> bool {
        let n = match source {
            ChunkSource::Mic => mic_count,
            ChunkSource::Sys => sys_count,
        };
        n > 0 && (n as f32 / total as f32) < INCIDENTAL_STREAM_CHUNK_SHARE
    };
    chunks
        .into_iter()
        .filter(|c| {
            if c.words.is_empty() || !is_incidental(c.source) {
                return true;
            }
            let spans: Vec<(u64, u64)> = c.words.iter().map(|w| (w.start_ms, w.end_ms)).collect();
            if is_timing_collapse(&spans) {
                eprintln!(
                    "diarize: dropping hallucinated {:?} chunk at {}ms from incidental stream: {:?}",
                    c.source,
                    c.start_ms,
                    c.text.chars().take(60).collect::<String>()
                );
                return false;
            }
            true
        })
        .collect()
}

fn is_repetition_collapse(text: &str) -> bool {
    let words: Vec<String> = text
        .split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect();
    let n = words.len();
    if n < 6 {
        return false;
    }
    for phrase_len in 1..=7 {
        if n < phrase_len * 3 {
            continue;
        }
        let mut start = 0;
        while start + phrase_len <= n {
            let mut reps = 1;
            let mut pos = start + phrase_len;
            while pos + phrase_len <= n
                && words[pos..pos + phrase_len] == words[start..start + phrase_len]
            {
                reps += 1;
                pos += phrase_len;
            }
            if reps >= 4 {
                return true;
            }
            if reps >= 3 && (phrase_len * reps) * 5 >= n * 3 {
                return true;
            }
            start = pos.max(start + 1);
        }
    }
    false
}

/// Minimum share of the weighted vote the winning language must hold for
/// `majority_language` to answer at all.
const LANGUAGE_VOTE_MIN_SHARE: f64 = 0.6;

/// Decide what language a recording was actually in, from the per-chunk
/// detections the STT provider handed back (issue #167).
///
/// Votes are weighted by chunk text length, not counted one-per-chunk: a
/// 40-word chunk is far better evidence than a 3-word one, and unweighted
/// counting is exactly how a handful of "mm-hm" fillers — which detect as
/// anything — would outvote the meeting. Chunks with no detection are
/// ignored rather than counted as a language.
///
/// Returns `None` when nothing clears `LANGUAGE_VOTE_MIN_SHARE`. A
/// genuinely bilingual recording should decline to answer rather than pick
/// a side; the caller then falls back to the transcript-anchored `auto`
/// directive, which is the right behaviour for mixed audio anyway.
fn majority_language(chunks: &[ChunkRecord]) -> Option<String> {
    let mut weights: std::collections::HashMap<&str, u64> = std::collections::HashMap::new();
    let mut total: u64 = 0;
    for c in chunks {
        let Some(lang) = c.detected_language.as_deref() else { continue };
        if lang.is_empty() {
            continue;
        }
        // Length in words, floored at 1 so a detection on a short-but-real
        // chunk still carries some weight.
        let weight = c.text.split_whitespace().count().max(1) as u64;
        *weights.entry(lang).or_insert(0) += weight;
        total += weight;
    }
    if total == 0 {
        return None;
    }
    let (lang, weight) = weights.into_iter().max_by_key(|&(_, w)| w)?;
    if (weight as f64) / (total as f64) >= LANGUAGE_VOTE_MIN_SHARE {
        Some(lang.to_string())
    } else {
        None
    }
}

/// Persist the recording's detected language on the note, if this capture
/// produced one and the note doesn't already carry one (issue #167).
///
/// First detection wins — a resumed recording appends to an existing note
/// and shouldn't overwrite what the original take established.
///
/// The adapters already suppress a detection whenever we named a language
/// ourselves, so in principle a `Some(code)` chunk can only come from an
/// `auto` capture. The note's own language is re-checked here anyway: the
/// invariant lives in four separate adapters, and the cost of one of them
/// drifting is that we silently persist our own request echoed back and
/// then summarise against it.
fn record_detected_language(app: &AppHandle, note_id: &str, chunks: &[ChunkRecord]) {
    let Some(lang) = majority_language(chunks) else { return };
    let state: State<AppState> = app.state();
    let conn = state.db.lock();
    let note = match db::get_note(&conn, note_id) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("record_detected_language: {e}");
            return;
        }
    };
    if note.detected_language.is_some() {
        return;
    }
    // Same resolution rule as transcription and summary: the note's own
    // language wins, empty falls back to the global setting.
    let resolved = if note.language.trim().is_empty() {
        db::get_setting(&conn, "language")
            .ok()
            .flatten()
            .unwrap_or_else(|| DEFAULT_LANGUAGE.to_string())
    } else {
        note.language.clone()
    };
    if resolved != "auto" {
        return;
    }
    if let Err(e) = db::set_detected_language(&conn, note_id, &lang) {
        eprintln!("record_detected_language: {e}");
    } else {
        eprintln!("[stt] detected recording language: {lang}");
    }
}

#[cfg(test)]
mod language_vote_tests {
    use super::*;

    fn chunk(text: &str, lang: Option<&str>) -> ChunkRecord {
        ChunkRecord {
            source: ChunkSource::Mic,
            start_ms: 0,
            text: text.to_string(),
            words: Vec::new(),
            detected_language: lang.map(str::to_string),
        }
    }

    fn words(n: usize) -> String {
        vec!["ord"; n].join(" ")
    }

    #[test]
    fn no_chunks_and_no_detections_both_decline() {
        assert_eq!(majority_language(&[]), None);
        assert_eq!(majority_language(&[chunk("hello there", None)]), None);
    }

    #[test]
    fn unanimous_detection_wins() {
        let chunks = vec![chunk(&words(20), Some("en")), chunk(&words(30), Some("en"))];
        assert_eq!(majority_language(&chunks), Some("en".to_string()));
    }

    // The regression test for the whole feature: an English meeting with one
    // stray filler chunk misdetected as Norwegian must still resolve to `en`.
    #[test]
    fn a_short_stray_chunk_does_not_outvote_the_meeting() {
        let chunks = vec![
            chunk(&words(60), Some("en")),
            chunk(&words(40), Some("en")),
            chunk("mm", Some("no")),
            chunk("ja", Some("cy")),
        ];
        assert_eq!(majority_language(&chunks), Some("en".to_string()));
    }

    // Unweighted counting would call this 3–1 for Norwegian. Weighted, the
    // English body holds ~96% and wins — that's the point of the weighting.
    #[test]
    fn many_tiny_chunks_lose_to_one_long_one() {
        let mut chunks = vec![chunk(&words(200), Some("en"))];
        for _ in 0..8 {
            chunks.push(chunk("hm", Some("no")));
        }
        assert_eq!(majority_language(&chunks), Some("en".to_string()));
    }

    #[test]
    fn an_even_bilingual_split_declines_to_answer() {
        let chunks = vec![chunk(&words(50), Some("en")), chunk(&words(50), Some("no"))];
        assert_eq!(majority_language(&chunks), None);
    }

    #[test]
    fn chunks_without_a_detection_are_ignored_not_counted() {
        // The undetected bulk must not dilute the winner below the threshold —
        // otherwise a provider that only reports sometimes would never answer.
        let chunks = vec![chunk(&words(500), None), chunk(&words(10), Some("en"))];
        assert_eq!(majority_language(&chunks), Some("en".to_string()));
    }

    #[test]
    fn a_bare_majority_below_the_threshold_declines() {
        // 55/45 is a bilingual recording, not an English one.
        let chunks = vec![chunk(&words(55), Some("en")), chunk(&words(45), Some("no"))];
        assert_eq!(majority_language(&chunks), None);
    }
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
mod diarize_tests {
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
    fn ends_sentence_recognises_terminators() {
        assert!(ends_sentence("he said."));
        assert!(ends_sentence("really?"));
        assert!(ends_sentence("stop!"));
        assert!(ends_sentence("note: this matters;"));
        assert!(ends_sentence("\"done.\""));
        assert!(ends_sentence("(yes!)"));
        assert!(ends_sentence("trailing space.   "));
    }

    #[test]
    fn ends_sentence_rejects_open_endings() {
        assert!(!ends_sentence("he said"));
        assert!(!ends_sentence("I was thinking that"));
        assert!(!ends_sentence("comma, here"));
        assert!(!ends_sentence(""));
    }

    #[test]
    fn merge_glues_open_clause_to_lowercase_continuation() {
        let input = "Speaker 1: I was thinking that\nSpeaker 1: we should pivot.";
        assert_eq!(
            merge_run_on_sentences(input),
            "Speaker 1: I was thinking that we should pivot."
        );
    }

    #[test]
    fn merge_drops_label_prefix_from_absorbed_line() {
        // Even when the absorbed line is a different speaker, the
        // first speaker keeps the floor — Whisper's mid-utterance
        // chunk break or a borderline diarizer flip is what produced
        // this artefact, not a real speaker change.
        let input = "Speaker 1: anyway the point is\nSpeaker 2: that we are out of time.";
        assert_eq!(
            merge_run_on_sentences(input),
            "Speaker 1: anyway the point is that we are out of time."
        );
    }

    #[test]
    fn forward_move_relocates_long_fragment_trailing_word_to_next() {
        // Real-recording case: a 14-word line ends with "Not" (no
        // terminator) and the next 11-word line starts with "quite"
        // (lowercase). Whisper cut a chunk between "things." and "Not
        // quite 10". Forward-move detaches the trailing "Not" and
        // hands it to next as the start of next's sentence —
        // attribution is preserved on both sides instead of fusing
        // them into one mis-labelled line.
        let input = "Speaker 1: Like you were working probably for 10 years in a lot of different things. Not\nSpeaker 1: quite 10, but a solid six or seven years as a working actor.";
        let output = merge_run_on_sentences(input);
        assert_eq!(
            output,
            "Speaker 1: Like you were working probably for 10 years in a lot of different things.\nSpeaker 1: Not quite 10, but a solid six or seven years as a working actor."
        );
    }

    #[test]
    fn forward_move_relocates_trailing_sentence_start_to_next_speaker() {
        // Real-recording case from a tester transcript: the next
        // speaker's first word ("It") landed on the previous turn
        // because Whisper's chunk boundary fell mid-sentence. The
        // continuation ("does pay off") is lowercase, confirming
        // "It" starts a sentence the next speaker delivers.
        let input = "Speaker 1: starts to unravel in the perfect way. It\nSpeaker 2: does pay off. That's what's really nice.";
        let output = merge_run_on_sentences(input);
        assert_eq!(
            output,
            "Speaker 1: starts to unravel in the perfect way.\nSpeaker 2: It does pay off. That's what's really nice."
        );
    }

    #[test]
    fn forward_move_handles_multi_word_trailing_fragment() {
        // 1–3 trailing capitalised words still qualify; the first must
        // be a sentence start.
        let input = "Speaker 1: that closes the door. So we\nSpeaker 2: can move on now.";
        let output = merge_run_on_sentences(input);
        assert_eq!(
            output,
            "Speaker 1: that closes the door.\nSpeaker 2: So we can move on now."
        );
    }

    #[test]
    fn forward_move_skips_when_trailing_starts_lowercase() {
        // The trailing word "that" is lowercase — it's a continuation
        // of the previous clause, not the start of a new sentence.
        // Falls through to the full merge instead.
        let input = "Speaker 1: I think that\nSpeaker 2: we should do it.";
        let output = merge_run_on_sentences(input);
        assert_eq!(
            output,
            "Speaker 1: I think that we should do it."
        );
    }

    #[test]
    fn backward_move_pulls_leading_sentence_close_to_prev_speaker() {
        // Real-recording case: "AMC was a young company. Brand new.
        // Brand" + "new. Hadn't done anything..." — the diarizer cut
        // mid-utterance and gave "new." to the wrong speaker.
        let input = "Speaker 1: AMC was a young company. Brand new. Brand\nSpeaker 2: new. Hadn't done anything significant yet.";
        let output = merge_run_on_sentences(input);
        assert_eq!(
            output,
            "Speaker 1: AMC was a young company. Brand new. Brand new.\nSpeaker 2: Hadn't done anything significant yet."
        );
    }

    #[test]
    fn backward_move_skips_when_leading_has_no_trailing_content() {
        // If next is *just* the leading fragment (nothing after the
        // terminator), the backward-move would empty it. Fall through
        // to the full merge instead, which produces the cleaner
        // single-line result.
        let input = "Speaker 1: I think we should\nSpeaker 2: stop.";
        let output = merge_run_on_sentences(input);
        // Full merge fires (prev open + next lowercase): joined.
        assert_eq!(output, "Speaker 1: I think we should stop.");
    }

    #[test]
    fn backward_move_skips_when_leading_too_long() {
        // 4-word leading fragment exceeds BOUNDARY_FRAGMENT_MAX_WORDS.
        // The boundary stays put — wider moves risk shuffling real
        // content between speakers.
        let input = "Speaker 1: I was about to\nSpeaker 2: yes that is correct. Anyway moving on.";
        let output = merge_run_on_sentences(input);
        // Full merge fires (prev open + next lowercase "yes"). Acceptable
        // fallback — at least the sentence is intact even if attribution
        // collapses to prev.
        assert!(output.contains("Speaker 1: I was about to yes that is correct"), "got: {output}");
    }

    #[test]
    fn merge_keeps_em_dash_interruption_distinct() {
        // A speaker trailing off / interrupted ends with an em-dash —
        // the next speaker's lowercase reply must NOT fuse, or two
        // distinct turns become one nonsensical sentence.
        let input = "Speaker 1: But what about the —\nSpeaker 2: right but we have no time.";
        assert_eq!(merge_run_on_sentences(input), input);
    }

    #[test]
    fn merge_keeps_en_dash_interruption_distinct() {
        let input = "Speaker 1: I was going to say –\nSpeaker 2: yeah I know what you mean.";
        assert_eq!(merge_run_on_sentences(input), input);
    }

    #[test]
    fn merge_keeps_lines_that_already_end_in_terminator() {
        let input = "Speaker 1: hello there.\nSpeaker 2: yes hi.";
        assert_eq!(merge_run_on_sentences(input), input);
    }

    #[test]
    fn merge_keeps_lines_when_next_starts_uppercase() {
        // No terminator on prev, but next starts with a capital — most
        // likely a fresh sentence the speaker began. Keep separate.
        let input = "Speaker 1: I was thinking that\nSpeaker 2: We should pivot.";
        assert_eq!(merge_run_on_sentences(input), input);
    }

    #[test]
    fn merge_handles_norwegian_lowercase_continuation() {
        // The unicode lowercase check covers non-ASCII letters; common
        // Humla case for Norwegian recordings.
        let input = "Speaker 1: vi har snakket om\nSpeaker 1: økonomien lenge.";
        assert_eq!(
            merge_run_on_sentences(input),
            "Speaker 1: vi har snakket om økonomien lenge."
        );
    }

    #[test]
    fn merge_chains_multiple_consecutive_glues() {
        // Three lines, all open-ended, all start lowercase → collapse
        // into a single line. The merged line keeps growing under the
        // word-count guard because we measure prev's length on each
        // step (it's well past 8 words by line 3 — but next is short).
        let input = "Speaker 1: so I was thinking\nSpeaker 1: that we should\nSpeaker 1: pivot the strategy.";
        assert_eq!(
            merge_run_on_sentences(input),
            "Speaker 1: so I was thinking that we should pivot the strategy."
        );
    }

    #[test]
    fn merge_passes_through_unlabelled_lines() {
        // Defensive: lines without `Label:` prefix shouldn't gain one
        // from the merge pass, and the prefix-stripping helper must
        // not eat content that isn't actually a label.
        let input = "first fragment\nsecond piece.";
        assert_eq!(
            merge_run_on_sentences(input),
            "first fragment second piece."
        );
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

    /// Build a piece directly without going through chunk splitting,
    /// with synthetic word timings. Each word is given a 200 ms span
    /// at `start_ms + i * 250 ms` so the caller can construct short
    /// (backchannel-shaped) or long pieces by varying word count.
    /// Pass `words = false` to produce a piece with no timing data,
    /// modelling the OpenAI / older-diagnostic path.
    fn piece(label: Option<&str>, text: &str, words: bool) -> LabelledPiece {
        let word_vec = if words {
            text.split_whitespace()
                .enumerate()
                .map(|(i, w)| crate::recording::ChunkWord {
                    text: w.to_string(),
                    start_ms: (i as u64) * 250,
                    end_ms: (i as u64) * 250 + 200,
                })
                .collect()
        } else {
            Vec::new()
        };
        LabelledPiece {
            label: label.map(str::to_string),
            text: text.to_string(),
            words: word_vec,
        }
    }

    /// Build a piece with explicit word timings. Useful when the test
    /// needs a specific acoustic span (e.g. a slow 1-word reply that
    /// shouldn't bridge).
    fn piece_with_span(
        label: Option<&str>,
        text: &str,
        start_ms: u64,
        end_ms: u64,
    ) -> LabelledPiece {
        let words: Vec<&str> = text.split_whitespace().collect();
        let n = words.len().max(1);
        let step = (end_ms - start_ms) / n as u64;
        let word_vec = words
            .iter()
            .enumerate()
            .map(|(i, w)| crate::recording::ChunkWord {
                text: (*w).to_string(),
                start_ms: start_ms + step * i as u64,
                end_ms: start_ms + step * (i as u64 + 1),
            })
            .collect();
        LabelledPiece {
            label: label.map(str::to_string),
            text: text.to_string(),
            words: word_vec,
        }
    }

    #[test]
    fn bridge_absorbs_short_sandwiched_interjection() {
        // The motivating case: Speaker 2 emits one word ("yeah") while
        // Speaker 1 is talking on either side. Should collapse.
        let mut pieces = vec![
            piece(Some("Speaker 1"), "so what I was saying about", true),
            piece(Some("Speaker 2"), "yeah", true),
            piece(Some("Speaker 1"), "the migration plan", true),
        ];
        bridge_short_interjections(&mut pieces);
        assert_eq!(pieces[0].label.as_deref(), Some("Speaker 1"));
        assert_eq!(pieces[1].label.as_deref(), Some("Speaker 1"));
        assert_eq!(pieces[2].label.as_deref(), Some("Speaker 1"));
    }

    #[test]
    fn bridge_keeps_long_reply_distinct() {
        // A seven-word reply exceeds BRIDGE_MAX_WORDS (6), so the
        // speaker boundary survives even when sandwiched.
        let mut pieces = vec![
            piece(Some("Speaker 1"), "do you have time", true),
            piece(Some("Speaker 2"), "no I really do not have time", true),
            piece(Some("Speaker 1"), "ok let me try later", true),
        ];
        bridge_short_interjections(&mut pieces);
        assert_eq!(pieces[1].label.as_deref(), Some("Speaker 2"));
    }

    #[test]
    fn bridge_keeps_short_but_slow_reply_distinct() {
        // Same speaker on either side, a sandwich, but the middle
        // piece's acoustic span is 4 s — exceeds BRIDGE_MAX_DURATION_MS
        // (3500). The duration gate is what protects real short turns
        // delivered slowly ("absolutely" with a long pause-filled
        // delivery) from getting fused into the surrounding speaker.
        let mut pieces = vec![
            piece(Some("Speaker 1"), "what do you think", true),
            piece_with_span(Some("Speaker 2"), "no", 0, 4_000),
            piece(Some("Speaker 1"), "fair enough", true),
        ];
        bridge_short_interjections(&mut pieces);
        assert_eq!(pieces[1].label.as_deref(), Some("Speaker 2"));
    }

    #[test]
    fn bridge_skips_pieces_without_word_timings() {
        // OpenAI transcribe and older diagnostic JSONs don't carry
        // per-word timings. We can't tell backchannel from real reply
        // without duration, so we conservatively leave the boundary.
        let mut pieces = vec![
            piece(Some("Speaker 1"), "first turn", false),
            piece(Some("Speaker 2"), "second turn", false),
            piece(Some("Speaker 1"), "third turn", false),
        ];
        bridge_short_interjections(&mut pieces);
        assert_eq!(pieces[1].label.as_deref(), Some("Speaker 2"));
    }

    #[test]
    fn bridge_does_not_fuse_distinct_neighbours() {
        // A short piece between two *different* speakers is a real
        // turn ("ok" between A and B in a back-and-forth), not noise.
        // The structural sandwich check is what blocks the collapse.
        let mut pieces = vec![
            piece(Some("Speaker 1"), "see you tomorrow", true),
            piece(Some("Speaker 2"), "ok", true),
            piece(Some("Speaker 3"), "bye both", true),
        ];
        bridge_short_interjections(&mut pieces);
        assert_eq!(pieces[1].label.as_deref(), Some("Speaker 2"));
    }

    #[test]
    fn bridge_skips_first_and_last_pieces() {
        // The boundary pieces have no "prev" or "next" with a different
        // label, so even short ones stay put. Otherwise the very first
        // utterance ("hi") of a meeting would get glued onto the
        // following speaker's first turn.
        let mut pieces = vec![
            piece(Some("Speaker 2"), "hi", true),
            piece(Some("Speaker 1"), "hello there how are you doing", true),
            piece(Some("Speaker 2"), "ok", true),
        ];
        bridge_short_interjections(&mut pieces);
        assert_eq!(pieces[0].label.as_deref(), Some("Speaker 2"));
        assert_eq!(pieces[2].label.as_deref(), Some("Speaker 2"));
    }

    #[test]
    fn continuation_chain_uses_surrounding_label_when_pre_and_post_agree() {
        // The "Men det jeg kunne / gjort, var jo..." case: the
        // 16-word mid-sentence fragment exceeds bridge limits but is
        // clearly the surrounding speaker's continuation. pre and
        // post both Speaker 1 → Speaker 1 wins despite Speaker 3's
        // word count dominance inside the chain.
        let mut pieces = vec![
            piece(Some("Speaker 1"), "Tidligere noe annet.", true),
            piece(Some("Speaker 1"), "Men det jeg kunne", true),
            piece(
                Some("Speaker 3"),
                "gjort, var jo å endra det til å komme opp i en slags pop-opp",
                true,
            ),
            piece(Some("Speaker 1"), "Sånn som den legges i lesninga her.", true),
        ];
        absorb_text_continuation_chains(&mut pieces);
        assert_eq!(pieces[1].label.as_deref(), Some("Speaker 1"));
        assert_eq!(pieces[2].label.as_deref(), Some("Speaker 1"));
    }

    #[test]
    fn continuation_chain_picks_longest_when_pre_and_post_disagree() {
        // "Ja, jeg" (S2) / "skjønner" (S1) / "det. Men jeg så ..."
        // (S3, dominant): the surrounding labels disagree (pre=S3,
        // post=S2), so the longest piece's label wins → S3.
        let mut pieces = vec![
            piece(
                Some("Speaker 3"),
                "Det er så mye rundt at vi så det ikke.",
                true,
            ),
            piece(Some("Speaker 2"), "Ja, jeg", true),
            piece(Some("Speaker 1"), "skjønner", true),
            piece(
                Some("Speaker 3"),
                "det. Men jeg så vi egentlig snakka om opprinnelig,",
                true,
            ),
            piece(Some("Speaker 2"), "altså, så det er ikke", true),
        ];
        absorb_text_continuation_chains(&mut pieces);
        // The chain extends through pieces 1..=4 (piece 0 ends with
        // "." → not chain head). pre=S3 (piece 0), post=None (chain
        // reaches the end → no post). Longest wins: S3 has 9+5=14
        // words, S2 has 2+5=7, S1 has 1. S3 wins.
        assert_eq!(pieces[1].label.as_deref(), Some("Speaker 3"));
        assert_eq!(pieces[2].label.as_deref(), Some("Speaker 3"));
        assert_eq!(pieces[3].label.as_deref(), Some("Speaker 3"));
        assert_eq!(pieces[4].label.as_deref(), Some("Speaker 3"));
        // Outside the chain: untouched.
        assert_eq!(pieces[0].label.as_deref(), Some("Speaker 3"));
    }

    #[test]
    fn continuation_chain_does_nothing_when_single_label() {
        // Chain spans pieces 0,1 but both already Speaker 1 — no-op.
        let mut pieces = vec![
            piece(Some("Speaker 1"), "Men jeg syns det er rart at", true),
            piece(Some("Speaker 1"), "vi gjør det sånn.", true),
        ];
        let before: Vec<_> = pieces.iter().map(|p| p.label.clone()).collect();
        absorb_text_continuation_chains(&mut pieces);
        let after: Vec<_> = pieces.iter().map(|p| p.label.clone()).collect();
        assert_eq!(before, after);
    }

    #[test]
    fn continuation_chain_breaks_at_sentence_terminator() {
        // Prev ends with "." → no chain. Speaker labels untouched.
        let mut pieces = vec![
            piece(Some("Speaker 1"), "End of one thought.", true),
            piece(Some("Speaker 2"), "starts another with lowercase.", true),
        ];
        absorb_text_continuation_chains(&mut pieces);
        assert_eq!(pieces[0].label.as_deref(), Some("Speaker 1"));
        assert_eq!(pieces[1].label.as_deref(), Some("Speaker 2"));
    }

    #[test]
    fn continuation_chain_breaks_at_uppercase_start() {
        // Prev ends mid-clause, next starts uppercase → next is a new
        // sentence (a real speaker change). Don't fuse.
        let mut pieces = vec![
            piece(Some("Speaker 1"), "He said", true),
            piece(Some("Speaker 2"), "Hello world.", true),
        ];
        absorb_text_continuation_chains(&mut pieces);
        assert_eq!(pieces[0].label.as_deref(), Some("Speaker 1"));
        assert_eq!(pieces[1].label.as_deref(), Some("Speaker 2"));
    }

    #[test]
    fn continuation_chain_treats_trailing_ellipsis_as_unfinished() {
        // "I was going to ..." ends with "..." — Whisper's trailing-off
        // marker, NOT a terminator. The chain should extend.
        let mut pieces = vec![
            piece(Some("Speaker 1"), "I was going to ...", true),
            piece(Some("Speaker 2"), "say something here.", true),
        ];
        absorb_text_continuation_chains(&mut pieces);
        // Both should share a label after absorption.
        assert_eq!(pieces[0].label, pieces[1].label);
    }

    #[test]
    fn continuation_chain_idempotent() {
        let mut pieces = vec![
            piece(Some("Speaker 1"), "Hvis du", true),
            piece(Some("Speaker 2"), "bare klikker på testkurs oppå", true),
            piece(Some("Speaker 2"), "den gule igjen, da.", true),
        ];
        absorb_text_continuation_chains(&mut pieces);
        let once: Vec<_> = pieces.iter().map(|p| p.label.clone()).collect();
        absorb_text_continuation_chains(&mut pieces);
        let twice: Vec<_> = pieces.iter().map(|p| p.label.clone()).collect();
        assert_eq!(once, twice);
    }

    #[test]
    fn piece_ends_terminator_recognises_typical_endings() {
        assert!(piece_ends_terminator("Hello world."));
        assert!(piece_ends_terminator("Really?"));
        assert!(piece_ends_terminator("Stop!"));
        assert!(piece_ends_terminator("see this:"));
        assert!(piece_ends_terminator("done.\""));
        assert!(!piece_ends_terminator("I was going to"));
        assert!(!piece_ends_terminator("I was going to ..."));
        assert!(!piece_ends_terminator("Hvis du"));
        assert!(!piece_ends_terminator("trailing comma,"));
    }

    #[test]
    fn piece_starts_continuation_recognises_typical_openings() {
        assert!(piece_starts_continuation("bare klikker..."));
        assert!(piece_starts_continuation("...continuation"));
        assert!(piece_starts_continuation("…og så"));
        assert!(!piece_starts_continuation("Hello world"));
        assert!(!piece_starts_continuation("Men hva nå?"));
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

    #[test]
    fn hybrid_fallback_keeps_sys_chunks_distinct_from_mic() {
        // Reproduces the silent-merge bug: in the (mic+sys) branch when
        // diarize is unavailable for the sys stream, sys chunks must NOT
        // get a None label — that would glue their text onto the previous
        // `You:` line, hiding remote speech inside the user's transcript.
        // The single-speaker fallback labels them `Speaker 1` so the
        // boundary survives.
        let chunks = vec![
            mic(0, "Ok thanks."),
            sys(500, "You got it."),
            mic(2000, "See you tomorrow."),
        ];
        let labeller = |c: &ChunkRecord| match c.source {
            ChunkSource::Mic => Some("You".to_string()),
            ChunkSource::Sys => Some("Speaker 1".to_string()),
        };
        assert_eq!(
            build_labelled_transcript(&chunks, &whole_chunk_pieces(labeller)),
            "You: Ok thanks.\nSpeaker 1: You got it.\nYou: See you tomorrow."
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
    fn timing_collapse_catches_the_real_hallucinated_chunk() {
        // Verbatim word timings from the K2 recording's single sys chunk —
        // "Enhver likhet med virkelige hendelser og personer er tilfeldig."
        // hallucinated over a two-second notification chime. Four words pinned
        // to 3050-3050.
        let spans = vec![
            (760, 760),
            (760, 1200),
            (1500, 1500),
            (1570, 2260),
            (2260, 3050),
            (3050, 3050),
            (3050, 3050),
            (3050, 3050),
            (3050, 3050),
        ];
        assert!(is_timing_collapse(&spans));
    }

    #[test]
    fn timing_collapse_ignores_clean_timings() {
        // Monotonic, non-degenerate timings — ordinary transcribed speech.
        let spans = vec![(0, 260), (260, 520), (520, 860), (890, 1380), (1400, 1560)];
        assert!(!is_timing_collapse(&spans));
        // A lone zero-duration word (quantisation) is not collapse.
        assert!(!is_timing_collapse(&[(0, 260), (300, 300), (400, 900)]));
        // Two adjacent zeros at the same instant still isn't enough.
        assert!(!is_timing_collapse(&[(0, 260), (300, 300), (300, 300), (400, 900)]));
        // Zeros at *different* instants aren't a pinned run.
        assert!(!is_timing_collapse(&[(100, 100), (200, 200), (300, 300)]));
        assert!(!is_timing_collapse(&[]));
    }

    #[test]
    fn timing_collapse_alone_would_flag_real_speech_so_never_use_it_alone() {
        // Tripwire. This pattern is from a genuine mic chunk in the K2
        // recording — "Det var den. Det var den, ja." — whose local-Whisper
        // timings contain a pinned run of SEVEN, longer than the hallucinated
        // chunk's four. `is_timing_collapse` fires on it, which is exactly why
        // `drop_incidental_stream_hallucinations` consults it only for a stream
        // that is already statistically noise. Anyone tempted to promote this
        // into a standalone hallucination filter deletes real speech.
        let real_speech_with_pinned_run = vec![
            (0, 300),
            (300, 620),
            (900, 900),
            (900, 900),
            (900, 900),
            (900, 900),
            (900, 900),
            (900, 900),
            (900, 900),
        ];
        assert!(
            is_timing_collapse(&real_speech_with_pinned_run),
            "documents the false positive this signal has on its own"
        );
    }

    #[test]
    fn dominant_stream_keeps_its_chunks_however_odd_the_timings() {
        // The safety property. 156 mic chunks to 1 sys chunk: the mic carries
        // the meeting, so even a mic chunk with a pinned run of 7 survives.
        // Without this the K2 note would have lost four chunks of real speech.
        let mut chunks: Vec<ChunkRecord> = (0..156)
            .map(|i| {
                mic_with_words(
                    i * 15_000,
                    vec![("Det", 0, 300), ("var", 900, 900), ("den", 900, 900), ("ja", 900, 900)],
                )
            })
            .collect();
        chunks.push(sys_with_words(
            110_019,
            vec![
                ("Enhver", 760, 760),
                ("og", 3050, 3050),
                ("personer", 3050, 3050),
                ("er", 3050, 3050),
            ],
        ));

        let kept = drop_incidental_stream_hallucinations(chunks);
        assert_eq!(kept.len(), 156, "every mic chunk survives");
        assert!(kept.iter().all(|c| c.source == ChunkSource::Mic));
    }

    #[test]
    fn a_stream_carrying_real_share_is_not_pruned() {
        // A genuine two-sided call: sys is well above the incidental share, so
        // its chunks are never dropped on timing evidence alone.
        let chunks = vec![
            mic_with_words(0, vec![("me", 0, 300)]),
            sys_with_words(
                1_000,
                vec![("a", 500, 500), ("b", 500, 500), ("c", 500, 500), ("d", 500, 500)],
            ),
            mic_with_words(2_000, vec![("me", 0, 300)]),
            sys_with_words(3_000, vec![("them", 0, 400)]),
        ];
        assert_eq!(drop_incidental_stream_hallucinations(chunks).len(), 4);
    }

    #[test]
    fn dropping_the_collapsed_chunk_makes_the_recording_mic_only_again() {
        // The whole point of filtering before the capture-mode decision: with
        // the hallucinated sys chunk gone, this is an in-person recording, so it
        // takes the mic-only branch and the note's head-count goes straight to
        // the mic instead of being split with a phantom remote side.
        // 40 clean mic chunks to 1 collapsed sys chunk — a 2.4% sys share, well
        // inside "incidental", matching the real 1-in-157.
        let mut chunks: Vec<ChunkRecord> = (0..40)
            .map(|i| mic_with_words(i * 15_000, vec![("Jeg", 0, 260), ("snakker", 260, 900)]))
            .collect();
        chunks.push(sys_with_words(
            110_019,
            vec![
                ("Enhver", 760, 760),
                ("likhet", 760, 1200),
                ("og", 3050, 3050),
                ("personer", 3050, 3050),
                ("er", 3050, 3050),
                ("tilfeldig.", 3050, 3050),
            ],
        ));
        assert!(chunks.iter().any(|c| c.source == ChunkSource::Sys));

        let kept = drop_incidental_stream_hallucinations(chunks);
        assert_eq!(kept.len(), 40, "the collapsed sys chunk is gone");
        assert!(
            !kept.iter().any(|c| c.source == ChunkSource::Sys),
            "no sys chunks left → mic-only branch, no phantom speaker"
        );
    }

    #[test]
    fn chunks_without_word_timings_survive_the_filter() {
        // Providers that don't return word timings (the current OpenAI path)
        // must not have their chunks dropped for lack of evidence.
        let chunks = vec![mic(0, "no word timings here"), sys(5_000, "nor here")];
        assert_eq!(drop_incidental_stream_hallucinations(chunks).len(), 2);
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
    fn label_returning_none_glues_to_previous_label_dont_use_for_distinct_speakers() {
        // Documents the underlying behavior the fallback above protects
        // against. With the buggy labeller (sys → None) the remote text
        // appears inside the user's `You:` line — silent data loss for
        // the reader. Locked into a test so a future "simplification" of
        // the fallback that goes back to None gets caught here.
        let chunks = vec![
            mic(0, "Ok thanks."),
            sys(500, "You got it."),
        ];
        let buggy = |c: &ChunkRecord| match c.source {
            ChunkSource::Mic => Some("You".to_string()),
            ChunkSource::Sys => None,
        };
        let result = build_labelled_transcript(&chunks, &whole_chunk_pieces(buggy));
        // This is the pathological output we DO NOT want from the
        // production code; it's only here as a tripwire on the helper.
        assert_eq!(result, "You: Ok thanks. You got it.");
    }

    #[test]
    fn build_transcript_orders_by_start_ms_with_mic_priority_on_tie() {
        // Mic and sys chunks at the same start_ms — mic is emitted first.
        // Reflects the typical UX assumption that the user speaks before
        // they hear a response, and stabilises ordering on tie.
        let chunks = vec![
            sys(0, "From sys."),
            mic(0, "From mic."),
        ];
        let labeller = |c: &ChunkRecord| match c.source {
            ChunkSource::Mic => Some("You".to_string()),
            ChunkSource::Sys => Some("Speaker 1".to_string()),
        };
        assert_eq!(
            build_labelled_transcript(&chunks, &whole_chunk_pieces(labeller)),
            "You: From mic.\nSpeaker 1: From sys."
        );
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

        let combined = combine_with_snapshot(&snapshot, &build_labelled_transcript(&chunks, &split));
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

    #[test]
    fn split_label_matches_the_readers_parse() {
        assert_eq!(split_label("Speaker 1: hello"), (Some("Speaker 1"), "hello"));
        assert_eq!(split_label("  Michael: hi there"), (Some("Michael"), "hi there"));
        // No label: an unlabelled line, a colon inside the words, or a "label"
        // longer than the reader would accept.
        assert_eq!(split_label("just words"), (None, "just words"));
        let long = format!("{}: x", "a".repeat(41));
        assert_eq!(split_label(&long).0, None);
        assert_eq!(split_label("10:30: the standup").0, None);
    }

    #[test]
    fn comparable_words_ignores_labels_case_and_punctuation() {
        assert_eq!(
            comparable_words("Speaker 1: Hello, world!"),
            comparable_words("Michael: hello world"),
        );
        // A cross-note rename rewrites the transcript and leaves the timeline
        // alone; that must not read as a gap.
        assert_eq!(comparable_words("Speaker 2: a b"), vec!["a", "b"]);
    }

    #[test]
    fn projection_covers_is_directional() {
        let t = "Speaker 1: a b c\nYou: d e";
        assert!(projection_covers(t, t));
        // Grouped differently, same words — the two rules differ on purpose.
        assert!(projection_covers(t, "Michael: a b c d e"));
        // The projection carrying more than the transcript hides nothing: the
        // reader renders the timeline, so there is no fallback to make.
        assert!(projection_covers(t, "Speaker 1: a b c\nYou: d e f g"));
        // Words the transcript has and the timeline lacks are the invisible
        // ones, wherever they sit.
        assert!(!projection_covers(t, "Speaker 1: a b c"));
        assert!(!projection_covers("A: one\nB: two\nC: three", "A: one\nC: three"));
    }

    #[test]
    fn orphaned_prefix_returns_the_uncovered_leading_lines() {
        let transcript = "we kicked off by agreeing the deadline slips\nSpeaker 1: so where did we land\nYou: the following Friday";
        let projection = "Speaker 1: so where did we land\nYou: the following Friday";
        assert_eq!(
            orphaned_prefix(transcript, projection),
            Some(vec!["we kicked off by agreeing the deadline slips"]),
        );
    }

    #[test]
    fn orphaned_prefix_is_none_when_the_projection_covers_the_transcript() {
        let t = "Speaker 1: a b c\nYou: d e";
        assert_eq!(orphaned_prefix(t, t), None);
        // Same words, different grouping — the two rules differ on purpose.
        assert_eq!(orphaned_prefix(t, "Michael: a b c d e"), None);
    }

    #[test]
    fn orphaned_prefix_is_none_when_the_gap_is_not_a_clean_prefix() {
        // Missing from the middle: not the combine_with_snapshot shape, so a
        // prefix repair would put the words back in the wrong place. The
        // render-time guard shows all the text instead.
        assert_eq!(
            orphaned_prefix("A: one\nB: two\nC: three", "A: one\nC: three"),
            None,
        );
        // Gap ends mid-line: don't split a turn to make it fit.
        assert_eq!(orphaned_prefix("A: one two three", "A: three"), None);
        // Nothing to compare against — a note with no timeline is left alone.
        assert_eq!(orphaned_prefix("A: one", ""), None);
    }

    /// Two devices open the same shared note, both see the same orphan, and
    /// both repair it. With random ids that made two sessions and projected
    /// the text twice; the id has to be a function of what it repairs.
    #[test]
    fn the_repair_session_id_is_the_same_on_every_device() {
        let lines = vec!["we kicked off by agreeing the deadline slips", "a second line"];
        assert_eq!(repair_session_id("note-a", &lines), repair_session_id("note-a", &lines));
        // Leading/trailing space on a line is not a difference — the entries
        // are written trimmed, so the id must not see it either.
        let padded = vec!["  we kicked off by agreeing the deadline slips  ", "a second line"];
        assert_eq!(repair_session_id("note-a", &lines), repair_session_id("note-a", &padded));
        // Different note, or different orphan, is a different session.
        assert_ne!(repair_session_id("note-a", &lines), repair_session_id("note-b", &lines));
        assert_ne!(
            repair_session_id("note-a", &lines),
            repair_session_id("note-a", &lines[..1]),
        );
        // And it stays inside the shape the server pins `client_id` to, which
        // rules out a readable sentinel like `__orphan__`.
        let id = repair_session_id("note-a", &lines);
        assert!(id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
        assert!(sessions::is_safe_session_id(&id));
    }

    #[test]
    fn synthesized_entries_keep_labels_and_carry_no_timings() {
        let lines = vec!["Michael: hei der", "unlabelled tail"];
        let values = jsonl_values(&synthesize_orphan_timeline(&lines));
        assert_eq!(values.len(), 2);
        assert_eq!(values[0]["label"], "Michael");
        assert_eq!(values[0]["text"], "hei der");
        assert_eq!(values[1]["label"], "");
        assert_eq!(values[1]["text"], "unlabelled tail");
        assert!(values[0]["end_ms"].as_u64() <= values[1]["start_ms"].as_u64());
        for v in &values {
            assert_eq!(v["words"].as_array().unwrap().len(), 0);
        }
        // The projection of the synthesized session reproduces the lines it
        // was built from, which is what makes the repair a fixed point.
        assert_eq!(
            comparable_words(&group_values_to_transcript(&values)),
            comparable_words("Michael: hei der\nunlabelled tail"),
        );
    }

    // ---- Per-turn transcript editing (#170) -----------------------------

    /// A timeline line as it sits on disk: bounds, label, text, word timings.
    fn tl(start_ms: u64, end_ms: u64, label: &str, text: &str) -> serde_json::Value {
        let words: Vec<serde_json::Value> = text
            .split_whitespace()
            .enumerate()
            .map(|(i, w)| {
                serde_json::json!({
                    "text": w,
                    "start_ms": start_ms + i as u64 * 100,
                    "end_ms": start_ms + i as u64 * 100 + 100,
                })
            })
            .collect();
        serde_json::json!({
            "start_ms": start_ms,
            "end_ms": end_ms,
            "label": label,
            "text": text,
            "words": words,
        })
    }

    fn text_of(v: &serde_json::Value) -> &str {
        v.get("text").and_then(|t| t.as_str()).unwrap_or("")
    }

    fn word_count(v: &serde_json::Value) -> usize {
        v.get("words").and_then(|w| w.as_array()).map_or(0, |a| a.len())
    }

    #[test]
    fn apply_group_text_writes_the_lowest_index_and_clears_the_rest() {
        // A rendered turn is a run of same-label entries; the edit is one call
        // over all of them. The replacement lands in the first, the others go
        // empty so the transcript builder skips them — the same mechanism a
        // chunk delete relies on, and what keeps the rebuilt transcript free of
        // a blank line where the tail entries were.
        let mut entries = vec![
            tl(0, 1_000, "Michael", "so where did we"),
            tl(1_000, 2_000, "Michael", "land on the freeze"),
            tl(2_000, 3_000, "Hege", "next Friday"),
        ];
        apply_group_text(&mut entries, &[0, 1], "so where did we land on the freeze?").unwrap();

        assert_eq!(text_of(&entries[0]), "so where did we land on the freeze?");
        assert_eq!(text_of(&entries[1]), "");
        assert_eq!(text_of(&entries[2]), "next Friday");
        // Entry count is untouched: chunk indices are line positions, and a
        // shifted index would misroute the next label cycle or delete.
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn apply_group_text_drops_word_timings_but_keeps_bounds() {
        // The old word timings no longer describe the new text, and a stale
        // mapping is worse than none. The entry bounds stay, so the turn still
        // highlights as playback passes it — only per-word karaoke is lost.
        let mut entries = vec![
            tl(0, 1_000, "Michael", "so where did we"),
            tl(1_000, 2_500, "Michael", "land on the freeze"),
        ];
        apply_group_text(&mut entries, &[0, 1], "rewritten").unwrap();

        assert_eq!(word_count(&entries[0]), 0);
        assert_eq!(word_count(&entries[1]), 0);
        assert_eq!(entries[0]["start_ms"], 0);
        assert_eq!(entries[0]["end_ms"], 1_000);
        assert_eq!(entries[1]["start_ms"], 1_000);
        assert_eq!(entries[1]["end_ms"], 2_500);
        // Labels are #170-out-of-scope: editing text never reassigns a speaker.
        assert_eq!(entries[0]["label"], "Michael");
        assert_eq!(entries[1]["label"], "Michael");
    }

    #[test]
    fn apply_group_text_rejects_out_of_bounds_without_mutating() {
        let mut entries = vec![tl(0, 1_000, "Michael", "original")];
        let err = apply_group_text(&mut entries, &[0, 4], "rewritten").unwrap_err();
        assert!(err.contains("out of bounds"), "unexpected error: {err}");
        // Bounds are checked before any write, so a bad index in a multi-entry
        // group can't leave the turn half-rewritten.
        assert_eq!(text_of(&entries[0]), "original");
        assert_eq!(word_count(&entries[0]), 1);
    }

    #[test]
    fn apply_group_text_rejects_a_non_object_entry() {
        // `read_timeline_values` keeps any line that parses as JSON, so a
        // malformed timeline can hold a bare array. Assigning a key into one
        // panics, and a panic in a command takes the app down.
        let mut entries = vec![serde_json::json!(["not", "an", "object"])];
        assert!(apply_group_text(&mut entries, &[0], "rewritten").is_err());
    }

    #[test]
    fn apply_group_text_rejects_an_empty_index_list() {
        let mut entries = vec![tl(0, 1_000, "Michael", "original")];
        assert!(apply_group_text(&mut entries, &[], "rewritten").is_err());
        assert_eq!(text_of(&entries[0]), "original");
    }

    #[test]
    fn transcript_rederived_after_an_edit_matches_the_timeline() {
        // The acceptance criterion: after an edit, the derived transcript is
        // exactly what re-deriving from the timeline produces — so a later
        // label cycle, delete, re-diarize or unify neither resurrects the
        // pre-edit text nor discards the edit. And no blank line survives
        // where the emptied tail entries sit.
        let mut entries = vec![
            tl(0, 1_000, "Michael", "so where did we"),
            tl(1_000, 2_000, "Michael", "land on the freeze"),
            tl(2_000, 3_000, "Hege", "next Friday"),
        ];
        apply_group_text(&mut entries, &[0, 1], "so where did we land on the freeze?").unwrap();

        assert_eq!(
            group_values_to_transcript(&entries),
            "Michael: so where did we land on the freeze?\nHege: next Friday"
        );
    }

    #[test]
    fn apply_group_text_trims_and_collapses_the_replacement() {
        let mut entries = vec![tl(0, 1_000, "Michael", "original")];
        apply_group_text(&mut entries, &[0], "  padded on both sides \n").unwrap();
        assert_eq!(text_of(&entries[0]), "padded on both sides");

        // A newline typed into the turn's textarea would otherwise become a
        // transcript line the timeline has no entry for — the very drift #170
        // closes. Fold it into a space.
        apply_group_text(&mut entries, &[0], "first line\nsecond line").unwrap();
        assert_eq!(text_of(&entries[0]), "first line second line");
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

    #[test]
    fn group_values_to_transcript_merges_same_label_runs() {
        let vals: Vec<serde_json::Value> = [
            ("Speaker 1", "hello"),
            ("Speaker 1", "there"),
            ("Speaker 2", "hi back"),
        ]
        .iter()
        .map(|(l, t)| serde_json::json!({ "label": l, "text": t }))
        .collect();
        assert_eq!(
            group_values_to_transcript(&vals),
            "Speaker 1: hello there\nSpeaker 2: hi back"
        );
    }

    #[test]
    fn dedup_drops_mic_chunk_echoing_overlapping_sys() {
        // The bug: speakers play remote voice, mic re-captures it, both
        // streams transcribe the same words. The mic version must drop.
        let chunks = vec![
            sys(0, "we should ship the migration on Friday before the freeze"),
            mic(200, "we should ship the migration on Friday"),
            sys(8000, "agreed sounds good to me"),
        ];
        let labeller = |c: &ChunkRecord| match c.source {
            ChunkSource::Mic => Some("You".to_string()),
            ChunkSource::Sys => Some("Speaker 1".to_string()),
        };
        let out = build_labelled_transcript(&chunks, &whole_chunk_pieces(labeller));
        // Mic echo dropped; only the sys content remains.
        assert_eq!(
            out,
            "Speaker 1: we should ship the migration on Friday before the freeze agreed sounds good to me"
        );
    }

    #[test]
    fn dedup_keeps_mic_chunk_with_distinct_user_speech() {
        // User actually talking over the remote — different words, low
        // containment, mic must survive.
        let chunks = vec![
            sys(0, "we should ship the migration on Friday before the freeze"),
            mic(200, "actually I want to push back on that timeline"),
        ];
        let labeller = |c: &ChunkRecord| match c.source {
            ChunkSource::Mic => Some("You".to_string()),
            ChunkSource::Sys => Some("Speaker 1".to_string()),
        };
        let out = build_labelled_transcript(&chunks, &whole_chunk_pieces(labeller));
        assert!(out.contains("actually I want to push back"));
        assert!(out.contains("Speaker 1: we should ship"));
    }

    #[test]
    fn dedup_keeps_short_mic_acks_even_if_words_appear_in_sys() {
        // "yeah" / "ok" alone shouldn't drop just because those tokens
        // appear in any sys window; they're valid backchannels. Sys
        // chunk ends with a terminator so the post-build heuristic
        // merge doesn't dissolve the speaker boundary.
        let chunks = vec![
            sys(0, "So the ship date is yeah ok confirmed for Friday."),
            mic(500, "yeah"),
            mic(1000, "ok"),
        ];
        let labeller = |c: &ChunkRecord| match c.source {
            ChunkSource::Mic => Some("You".to_string()),
            ChunkSource::Sys => Some("Speaker 1".to_string()),
        };
        let out = build_labelled_transcript(&chunks, &whole_chunk_pieces(labeller));
        assert!(out.contains("You: yeah ok"));
    }

    #[test]
    fn dedup_keeps_mic_when_sys_window_is_far_away() {
        // Mic chunk's text matches sys text 30 seconds later — outside the
        // overlap window, must NOT dedup. The remote echoing the user
        // later is genuine new content. Sentence terminators on each
        // chunk keep the post-build heuristic merge from dissolving the
        // speaker boundary (the merge rule needs prev to lack a
        // terminator, which it doesn't here).
        let chunks = vec![
            mic(0, "The proposal is to extend the deadline to Friday."),
            sys(30_000, "The proposal is to extend the deadline to Friday."),
        ];
        let labeller = |c: &ChunkRecord| match c.source {
            ChunkSource::Mic => Some("You".to_string()),
            ChunkSource::Sys => Some("Speaker 1".to_string()),
        };
        let out = build_labelled_transcript(&chunks, &whole_chunk_pieces(labeller));
        assert!(out.contains("You: The proposal is to extend"));
        assert!(out.contains("Speaker 1: The proposal is to extend"));
    }

    #[test]
    fn dedup_drops_all_mic_when_sys_mirrors_them() {
        // The intended remote-meeting / podcast-through-speakers case:
        // mic and sys carry near-identical text because the speakers
        // are leaking system audio into the mic. Dedup must drop every
        // mic chunk so the user doesn't see "You: foo / Speaker 1: foo"
        // duplicate-turn pairs in the rendered transcript.
        let chunks = vec![
            mic(0, "this is the first thing I am saying about politics"),
            sys(20, "this is the first thing I am saying about politics"),
            mic(3000, "and now the second thing about media coverage"),
            sys(3020, "and now the second thing about media coverage"),
            mic(6000, "third point on commentary today"),
            sys(6020, "third point on commentary today"),
            mic(9000, "fourth and final remark on the topic"),
            sys(9020, "fourth and final remark on the topic"),
        ];
        let labeller = |c: &ChunkRecord| match c.source {
            ChunkSource::Mic => Some("You".to_string()),
            ChunkSource::Sys => Some("Speaker 1".to_string()),
        };
        let out = build_labelled_transcript(&chunks, &whole_chunk_pieces(labeller));
        // No "You:" turns survive — only the sys content is shown.
        assert!(!out.contains("You:"));
        assert!(out.contains("Speaker 1: this is the first thing"));
    }

    #[test]
    fn dedup_noop_when_only_mic_chunks_present() {
        // In-person mode: only mic chunks. Dedup must not touch anything.
        let chunks = vec![
            mic(0, "this is the first thing I am saying"),
            mic(3000, "and now the second thing"),
        ];
        let labeller = |c: &ChunkRecord| match c.source {
            ChunkSource::Mic => Some("Speaker 1".to_string()),
            ChunkSource::Sys => Some("Speaker 2".to_string()),
        };
        let out = build_labelled_transcript(&chunks, &whole_chunk_pieces(labeller));
        assert_eq!(
            out,
            "Speaker 1: this is the first thing I am saying and now the second thing"
        );
    }

    #[test]
    fn token_containment_perfect_subset() {
        let a: Vec<String> = ["ship", "the", "migration"].iter().map(|s| s.to_string()).collect();
        let b: Vec<String> = ["we", "should", "ship", "the", "migration", "on", "friday"]
            .iter().map(|s| s.to_string()).collect();
        // a ⊆ b → containment 1.0
        assert!((token_containment(&a, &b) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn token_containment_no_overlap() {
        let a: Vec<String> = ["completely", "different", "words"].iter().map(|s| s.to_string()).collect();
        let b: Vec<String> = ["nothing", "in", "common"].iter().map(|s| s.to_string()).collect();
        assert!(token_containment(&a, &b) < 1e-6);
    }
}

#[cfg(test)]
mod repetition_tests {
    use super::*;

    #[test]
    fn collapse_detects_long_phrase_loop() {
        // The exact pattern from the user-reported screenshot: "Er det en
        // bok?" repeated dozens of times.
        let s = "Er det en bok? ".repeat(20);
        assert!(is_repetition_collapse(&s));
    }

    #[test]
    fn collapse_detects_single_word_loop() {
        let s = "yes yes yes yes yes yes yes yes";
        assert!(is_repetition_collapse(s));
    }

    #[test]
    fn collapse_passes_normal_speech() {
        // Real-world Norwegian sample with no repetition collapse.
        let s = "Vi har en avtale i morgen klokken ti om prosjektet vi diskuterte forrige uke.";
        assert!(!is_repetition_collapse(s));
    }

    #[test]
    fn collapse_passes_natural_three_rep_short() {
        // Three short reps in a 6-word total chunk are below the dominance
        // threshold (need ≥4 reps OR ≥60% coverage). 6 words, 3 reps × 1
        // word = 50% coverage — passes.
        let s = "ja ja ja det stemmer mhm";
        assert!(!is_repetition_collapse(s));
    }

    #[test]
    fn collapse_detects_partial_loop_dominating_chunk() {
        // 12-word chunk, 4 reps of a 2-word phrase = 8 words = 66% coverage.
        // Should be flagged.
        let s = "noe annet skjedde okay test test test test okay noe annet";
        assert!(is_repetition_collapse(s));
    }

    #[test]
    fn collapse_handles_punctuation_and_case() {
        // Same phrase but mixed case + punctuation differences should still
        // be matched as identical reps.
        let s = "Er det en bok? er det en bok! Er Det En Bok? er det en bok.";
        assert!(is_repetition_collapse(s));
    }
}

#[cfg(test)]
mod hallucination_tests {
    use super::*;

    #[test]
    fn drops_punctuation_only() {
        assert!(is_likely_hallucination(".", "no"));
        assert!(is_likely_hallucination("...", "no"));
        assert!(is_likely_hallucination(" . ", "no"));
        assert!(is_likely_hallucination("***", "en"));
    }

    #[test]
    fn drops_norwegian_silence_greetings() {
        assert!(is_likely_hallucination("Hei.", "no"));
        assert!(is_likely_hallucination("Hei!", "no"));
        assert!(is_likely_hallucination("Takk!", "no"));
        assert!(is_likely_hallucination("Hallo.", "no"));
        assert!(is_likely_hallucination("Ha det.", "no"));
    }

    #[test]
    fn drops_english_silence_greetings() {
        assert!(is_likely_hallucination("Hi.", "en"));
        assert!(is_likely_hallucination("Hello!", "en"));
        assert!(is_likely_hallucination("Thanks!", "en"));
        assert!(is_likely_hallucination("Thank you.", "en"));
    }

    #[test]
    fn keeps_real_one_word_answers() {
        // Yes / no / ja / nei are real responses too often to drop.
        assert!(!is_likely_hallucination("Yes.", "en"));
        assert!(!is_likely_hallucination("No.", "en"));
        assert!(!is_likely_hallucination("Ja.", "no"));
        assert!(!is_likely_hallucination("Nei.", "no"));
        assert!(!is_likely_hallucination("OK", "en"));
    }

    #[test]
    fn keeps_real_speech_containing_greeting_words() {
        assert!(!is_likely_hallucination(
            "Hei, hvordan går det med deg i dag?",
            "no"
        ));
        assert!(!is_likely_hallucination(
            "I just wanted to say thanks for the help yesterday.",
            "en"
        ));
    }

    #[test]
    fn drops_caption_attribution_inside_real_speech() {
        // Substring match — caption attribution glued to a real
        // sentence should still trigger the drop.
        assert!(is_likely_hallucination(
            "And that was the meeting. Subtitles by Amara.org community.",
            "en"
        ));
    }
}

#[cfg(test)]
mod import_tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn title_from_filename_strips_extension() {
        assert_eq!(title_from_filename(Path::new("/tmp/meeting.m4a")), "meeting");
        assert_eq!(title_from_filename(Path::new("Voice Memo 3.mp3")), "Voice Memo 3");
    }

    #[test]
    fn title_from_filename_keeps_stem_verbatim() {
        // Underscores / dashes / casing are the user's — preserved as-is so the
        // note title matches the file they picked.
        assert_eq!(
            title_from_filename(Path::new("standup_2026-07-09.wav")),
            "standup_2026-07-09"
        );
    }

    #[test]
    fn title_from_filename_handles_no_extension() {
        assert_eq!(title_from_filename(Path::new("/x/recording")), "recording");
    }

    #[test]
    fn title_from_filename_falls_back_when_empty() {
        assert_eq!(title_from_filename(Path::new("")), "Imported audio");
        assert_eq!(title_from_filename(Path::new("/")), "Imported audio");
    }

    #[test]
    fn title_from_filename_trims_whitespace() {
        assert_eq!(title_from_filename(Path::new("  spaced  .mp3")), "spaced");
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
}
