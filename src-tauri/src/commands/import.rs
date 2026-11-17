//! Importing an audio file into a new note: the title seeded from its filename,
//! the sidecar's `--import` replay through the live chunk pipeline, and the
//! completion path into the shared post-stop chain.

use super::{
    dispatch_sidecar_event, drain_inflight, emit_status, ensure_provider_ready, err, ms_since,
    redact_sidecar_line, run_post_stop_chain, sidecar_path, stop_stale_child,
    take_post_stop_snapshot, StopClock, DEFAULT_LANGUAGE, IMPORT_BACKLOG_PERMITS,
};
use crate::recording::{CaptureSink, ErrorPayload, Inflight, Phase, SidecarEvent, SinkMode, StartClaim};
use crate::{db, AppState};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::Semaphore;

/// The title an import falls back to when the path had no usable stem. Named
/// because it is not user-owned: `menubar::is_replaceable_title` has to
/// recognise it as Humla's own, and a second copy of the literal would be a
/// second place for the two to drift apart (#90).
pub(crate) const IMPORTED_AUDIO_TITLE: &str = "Imported audio";

/// Seed a note title from an imported audio file's name: the file stem with no
/// extension, trimmed. Falls back to [`IMPORTED_AUDIO_TITLE`] for a path with
/// no usable stem. Kept deliberately literal — the user's filename is
/// meaningful, so we don't reflow or re-case it.
pub(crate) fn title_from_filename(path: &std::path::Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.trim())
        .unwrap_or("");
    if stem.is_empty() {
        IMPORTED_AUDIO_TITLE.to_string()
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

    // Import and live recording share the one capture slot, claimed the same
    // way `recording_start` claims it.
    let (claim, stale_child) =
        StartClaim::take(&state.recording, "already recording or importing")?;
    if let Some(c) = stale_child {
        stop_stale_child(c).await;
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
        if let Some(recorded) = crate::import_date::recording_date(&source_path, db::now_ms()) {
            db::set_note_created_at(&conn, &note.id, recorded).map_err(err)?;
            note.created_at = recorded;
        }
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
        // Import always transcribes as it replays — the user picked a file and
        // is waiting for its text, so there is nothing to defer (#146).
        s.sink = Arc::new(CaptureSink::new(SinkMode::Live));
        // Fresh note → no prior transcript to prepend on diarize.
        *s.transcript_at_start.lock() = String::new();
        // Import holds no cloud recording lock.
        s.lock_id = None;
    }

    let app_clone = app.clone();
    let note_id_clone = note_id.clone();
    let inflight_for_reader = inflight.clone();
    let sink_for_reader = state.recording.lock().sink.clone();
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
                        &sink_for_reader,
                    )
                    .await
                    {
                        break;
                    }
                }
                Err(e) => eprintln!("bad sidecar line: {e} -- {}", redact_sidecar_line(&line)),
            }
        }
        // Replay finished (Stopped, or the sidecar closed its pipe). Drive the
        // same post-stop chain the live path runs on recording_stop.
        finish_import(&app_clone, &note_id_clone).await;
    });

    // Committed only with the reader in place: until then a start would read
    // this capture's live child with no reader as an orphan and stop it.
    {
        let mut s = state.recording.lock();
        s.reader = Some(reader_handle);
        claim.commit(&mut s);
    }

    Ok(note)
}

/// Completion path for an import: the sidecar replayed the whole file and closed
/// its pipe. Drain the outstanding transcribes, free the capture slot, then run
/// the shared post-stop chain (mic-only diarize → playback assets →
/// summary-ready). No SIGTERM/child-wait — the import sidecar exits on its own.
async fn finish_import(app: &AppHandle, note_id: &str) {
    let replay_started = std::time::Instant::now();
    let state: State<AppState> = app.state();
    // Bail if a newer capture already claimed the slot (shouldn't happen —
    // import holds it until here). Then take the same `Draining → Finishing`
    // lifecycle the live stop takes: this path's child and reader are already
    // finished, so without it `recording_start`'s self-heal reads a zombie,
    // takes over, and replaces the sink — clearing `chunk_log` before the
    // snapshot below (#182).
    let inflight = {
        let mut s = state.recording.lock();
        if s.note_id.as_deref() != Some(note_id) || s.begin_stop().is_err() {
            return;
        }
        s.inflight.clone()
    };
    // No more chunks are coming (reader loop exited), so drain what's queued.
    let pending = inflight.lock().len() as u32;
    let t_drain = std::time::Instant::now();
    drain_inflight(&inflight).await;
    // An import has no sidecar shutdown or reader wait of its own — the sidecar
    // exits when the file runs out — so those rows stay zero.
    let clock = StopClock::new(replay_started);
    clock.record(|t| {
        t.drain_ms = ms_since(t_drain);
        t.drain_pending_chunks = pending;
    });

    let (temp_dir, post_stop) = {
        let mut s = state.recording.lock();
        let temp_dir = s.temp_dir.take();
        let post_stop = take_post_stop_snapshot(&mut s);
        s.child = None;
        s.reader = None;
        s.inflight = Arc::new(parking_lot::Mutex::new(Vec::new()));
        (temp_dir, post_stop)
    };

    run_post_stop_chain(
        app.clone(),
        note_id.to_string(),
        temp_dir,
        post_stop,
        clock,
    )
    .await;
}

#[cfg(test)]
mod tests {
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
}
