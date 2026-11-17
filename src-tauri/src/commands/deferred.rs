//! Transcribing a note's retained takes after the fact: the `transcribe_note`
//! command, the replay of each take's streams and the per-take bookkeeping.

use super::capture::{
    dispatch_sidecar_event, drain_inflight, ensure_provider_ready, ms_since, redact_sidecar_line,
    session_write_dir, write_chunks_json,
};
#[cfg(feature = "cloud")]
use super::cloud;
use super::diarization::{note_diarize_settings, rediarize_apply_to_chunks, LabelFallback};
use super::transcribe::record_detected_language;
use super::unify::unify_note_speakers;
use super::{
    chat, emit_error, emit_transcribe_progress, err, notes, session_changed_for_sync,
    sidecar_path, title, ChainProgress, IMPORT_BACKLOG_PERMITS,
};
use crate::recording::{
    CaptureSink, ChunkRecord, ChunkSource, ErrorPayload, Inflight, ReplayProgress, ReplayTakeTimings,
    ReplayTimings, SidecarEvent, SinkMode, TakeWork,
};
use crate::{db, sessions, AppState};
use std::process::Stdio;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::Semaphore;

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
