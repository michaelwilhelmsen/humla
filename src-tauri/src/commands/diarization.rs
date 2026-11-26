//! Speaker labels for a take: diarizing each stream on stop (with the engine
//! fallback and the echo pass), re-diarizing a saved note, numbering speakers
//! across streams and takes, and the per-pass diagnostics dumps.

use super::playback::{write_playback_assets, PlaybackAudio};
use super::timeline::{
    commit_rebuilt_transcript, group_values_to_transcript, read_timeline_values,
    rebuild_note_transcript, serialize_timeline,
};
use super::transcribe::drop_incidental_stream_hallucinations;
use super::transcript::{
    build_labelled_transcript, build_unrefined_pieces, refine_pieces, single_piece,
    split_by_labels, split_by_segments, LabelledPiece,
};
use super::unify::unify_note_speakers;
use super::capture::{ms_since, record_timing, PostStopSnapshot, StopClock};
use super::{
    chat, emit_error, err, notes, selected_diarize_engine, session_changed_for_sync,
    ChainProgress, StreamSteps, DEFAULT_COMMUNITY1_THRESHOLD,
};
use crate::recording::{ChunkRecord, ChunkSource, Step, TranscriptPayload};
use crate::{db, diarize, sessions, wav, AppState};
use std::path::PathBuf;
use tauri::{AppHandle, Emitter, Manager, State};

const DIARIZE_MODEL_MISSING: &str = "Diarize model isn't downloaded. Download it in Settings → Transcription → Speaker labels, then try again.";

const DIARIZE_MODEL_MISSING_UNLABELLED: &str = "Speaker diarization model isn't downloaded — transcript saved without speaker labels. Download it from Settings → Transcription → Speaker labels.";

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
    // The rebuilt timeline lost the speaker changes a model moved, and the
    // clean read keyed on the old entries.
    if result.is_ok() {
        tauri::async_runtime::spawn(super::clean::clean_note_quietly(app.clone(), note_id.clone()));
    }
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
        PlaybackAudio::FollowGate,
    )
    .await
    .map(|_| ())
    .map_err(err)
}

/// Parse chunk records from a single JSON file matching the shape
/// `{ "chunks": [{source, start_ms, text, words?}, ...] }`. Used for both
/// the standalone `chunks.json` written next to retained audio and the
/// per-engine diagnostic dumps under `diagnostics/<note_id>/`.
pub(super) fn parse_chunks_json(path: &std::path::Path) -> anyhow::Result<Vec<ChunkRecord>> {
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

/// What the named steps inside one take's (re)diarize cost, in ms.
pub(super) struct TakeStepCosts {
    /// `None` when the take had no system stream beside its mic.
    pub(super) echo_ms: Option<u64>,
    pub(super) playback_ms: u64,
}

/// Mirror of [`diarize_and_apply`]'s branching over caller-supplied paths and
/// chunks instead of recording-session state. No snapshot — the transcript is
/// rebuilt from the chunk timings rather than appended to an in-flight session.
pub(super) async fn rediarize_apply_to_chunks(
    app: AppHandle,
    note_id: String,
    session_id: String,
    mic_wav: Option<PathBuf>,
    sys_wav: Option<PathBuf>,
    chunks: Vec<ChunkRecord>,
    settings: NoteDiarizeSettings,
    fallback: LabelFallback,
    progress: &ChainProgress,
    audio: PlaybackAudio,
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
    let pieces_pre = build_unrefined_pieces(&chunks, stage.splitter.as_ref());
    let mut pieces_post = pieces_pre.clone();
    refine_pieces(&mut pieces_post);
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
        audio,
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

/// How one note is diarized.
#[derive(Clone, Copy)]
pub(super) struct NoteDiarizeSettings {
    /// Chosen from the selected engine, the note's speaker count and which
    /// models are downloaded (`diarize::choose_engine`), for the whole note.
    pub(super) choice: diarize::EngineChoice,
    pub(super) thresholds: diarize::Thresholds,
    pub(super) expected_speakers: Option<i64>,
}

pub(super) async fn note_diarize_settings(app: &AppHandle, note_id: &str) -> NoteDiarizeSettings {
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
pub(super) async fn diarize_and_maybe_clean(
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
/// captured before and after `refine_pieces` runs, with the label changes
/// between them.
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

    let pieces_pre_json = pieces.map(|(pre, _)| pieces_to_json(pre));
    let pieces_post_json = pieces.map(|(_, post)| pieces_to_json(post));
    let label_changes = match pieces {
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
        "pieces_before_refine": pieces_pre_json,
        "pieces_after_refine": pieces_post_json,
        "label_changes": label_changes,
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
pub(super) struct EchoPass {
    pub(super) analysis: Option<crate::echo::TakeAnalysis>,
    /// The mic with the echo cancelled out, for the mic diarize to read.
    pub(super) cancelled: Option<CancelledMicWav>,
    /// What the step cost, when it ran.
    pub(super) ms: Option<u64>,
}

impl EchoPass {
    /// `announce` names the step, and runs only when the pass does.
    pub(super) async fn run(
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
    pub(super) fn mic_input<'a>(&'a self, captured: &'a std::path::Path) -> &'a std::path::Path {
        self.cancelled.as_ref().map_or(captured, |wav| wav.path())
    }

    /// The mic diarize has read the cancelled mic, so nothing keeps it.
    pub(super) fn release_cancelled_mic(&mut self) {
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
pub(super) struct CancelledMicWav(PathBuf);

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

pub(super) fn log_echo_check(check: &crate::echo::VoiceCheck) {
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
pub(super) async fn diarize_and_apply(
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
        PlaybackAudio::FollowGate,
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
pub(super) const INCIDENTAL_STREAM_CHUNK_SHARE: f32 = 0.05;

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
pub(super) fn distinct_speaker_count(segments: &[diarize::Segment]) -> usize {
    segments
        .iter()
        .map(|s| s.speaker_id.as_str())
        .collect::<std::collections::HashSet<_>>()
        .len()
}

/// Final display labels for a *hybrid* recording — one where both the mic
/// and the system stream produced speech.
pub(super) struct HybridLabels {
    pub(super) mic: std::collections::HashMap<String, String>,
    pub(super) sys: std::collections::HashMap<String, String>,
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
pub(super) fn finalise_stream_labels(
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
pub(super) fn combine_with_snapshot(snapshot: &str, new_session: &str) -> String {
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
pub(super) fn session_speaker_offset(snapshot: &str) -> u32 {
    max_speaker_number(snapshot.trim_end())
}

/// Offset a *bare* speaker label (no trailing colon): `"Speaker 2"` + 3 →
/// `"Speaker 5"`. Non-numbered labels (`"You"`, `"Michael"`) and the empty
/// label pass through untouched, matching `offset_speaker_numbers`.
pub(super) fn offset_speaker_label(label: &str, offset: u32) -> String {
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
pub(super) fn assign_speaker(chunk_start_ms: u64, segments: &[diarize::Segment]) -> Option<&str> {
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

#[cfg(test)]
mod tests {
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
}
