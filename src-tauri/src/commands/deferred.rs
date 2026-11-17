//! Transcribing a note's retained takes after the fact: the `transcribe_note`
//! command, the replay of each take's streams and the per-take bookkeeping.

use super::capture::{
    check_provider_ready, discard_empty_take, dispatch_sidecar_event, drain_inflight, ensure_provider_ready,
    finalize_session, ms_since, redact_sidecar_line, NotReady,
    session_write_dir, write_chunks_json,
};
#[cfg(feature = "cloud")]
use super::cloud;
use super::diarization::{note_diarize_settings, rediarize_apply_to_chunks, LabelFallback};
use super::playback::PlaybackAudio;
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
    if replay_owed(&note_id) || !state.transcribing.lock().insert(note_id.clone()) {
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

/// One take to replay and where its streams are.
struct TakeSource {
    entry: sessions::SessionEntry,
    mic: Option<std::path::PathBuf>,
    sys: Option<std::path::PathBuf>,
    /// Set for a take not yet in the manifest: it is added once it has text.
    pending: Option<PendingTake>,
}

impl TakeSource {
    /// A retained take, from its session directory.
    fn retained(entry: sessions::SessionEntry, dir: &std::path::Path) -> Self {
        let stream = |name: &str| Some(dir.join(name)).filter(|p| p.exists());
        TakeSource { entry, mic: stream("mic.wav"), sys: stream("sys.wav"), pending: None }
    }
}

/// What the manifest row of a take that isn't in it yet will hold.
#[derive(Clone, Debug)]
pub(super) struct PendingTake {
    pub session_id: String,
    pub started_at: String,
    pub streams: Vec<String>,
    pub captured_duration_ms: u64,
}

/// A capture's streams moved out of its temp dir into one of their own in the
/// system temp dir, for a replay that outlives the post-stop chain. The
/// directory is deleted when this is dropped, however the replay ends.
#[derive(Debug)]
pub(super) struct UnretainedStreams {
    dir: std::path::PathBuf,
    pub mic: Option<std::path::PathBuf>,
    pub sys: Option<std::path::PathBuf>,
}

impl UnretainedStreams {
    /// `None` when there is no stream to move.
    pub(super) async fn take(
        mic: Option<&std::path::Path>,
        sys: Option<&std::path::Path>,
    ) -> std::io::Result<Option<Self>> {
        Self::take_into(&std::env::temp_dir(), mic, sys).await
    }

    async fn take_into(
        parent: &std::path::Path,
        mic: Option<&std::path::Path>,
        sys: Option<&std::path::Path>,
    ) -> std::io::Result<Option<Self>> {
        let mic = mic.filter(|p| p.exists());
        let sys = sys.filter(|p| p.exists());
        if mic.is_none() && sys.is_none() {
            return Ok(None);
        }
        let dir = parent.join(format!("{UNRETAINED_PREFIX}{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await?;
        let mut out = UnretainedStreams { dir, mic: None, sys: None };
        if let Some(src) = mic {
            out.mic = Some(move_file(src, &out.dir.join("mic.wav")).await?);
        }
        if let Some(src) = sys {
            out.sys = Some(move_file(src, &out.dir.join("sys.wav")).await?);
        }
        Ok(Some(out))
    }
}

impl Drop for UnretainedStreams {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn move_file(src: &std::path::Path, dest: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    if tokio::fs::rename(src, dest).await.is_err() {
        tokio::fs::copy(src, dest).await?;
        let _ = tokio::fs::remove_file(src).await;
    }
    Ok(dest.to_path_buf())
}

/// A note's place in `AppState.transcribing`, given back when dropped.
pub(super) struct TranscribeSlot {
    set: Arc<parking_lot::Mutex<std::collections::HashSet<String>>>,
    note_id: String,
}

impl TranscribeSlot {
    pub(super) fn try_take(app: &AppHandle, note_id: &str) -> Option<Self> {
        Self::try_take_in(&app.state::<AppState>().transcribing, note_id)
    }

    fn try_take_in(set: &Arc<parking_lot::Mutex<std::collections::HashSet<String>>>, note_id: &str) -> Option<Self> {
        set.lock()
            .insert(note_id.to_string())
            .then(|| TranscribeSlot { set: set.clone(), note_id: note_id.to_string() })
    }

    /// Wait for whatever is transcribing the note to finish, then take its place.
    async fn wait_for(app: &AppHandle, note_id: &str) -> Self {
        loop {
            if let Some(slot) = Self::try_take(app, note_id) {
                return slot;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }
}

impl Drop for TranscribeSlot {
    fn drop(&mut self) {
        self.set.lock().remove(&self.note_id);
    }
}

/// Transcribe a "Transcribe manually" take whose audio may not be kept: the
/// audio gate closed while it was recording. Spawned by the post-stop chain, so
/// it holds no capture slot and reports on `transcribe_status`. `slot` is the
/// note's transcribe slot when the chain could take it; otherwise this waits
/// for the run holding it. The streams are deleted when it returns.
pub(super) async fn transcribe_unretained_take(
    app: AppHandle,
    note_id: String,
    take: PendingTake,
    streams: UnretainedStreams,
    slot: Option<TranscribeSlot>,
    owed: OwedReplay,
) {
    let _slot = match slot {
        Some(slot) => slot,
        None => TranscribeSlot::wait_for(&app, &note_id).await,
    };
    let _status = TranscribeStatus::start(&app, &note_id);
    let result = transcribe_pending_take(&app, &note_id, take, &streams).await;
    drop(streams);
    drop(owed);
    if let Err(e) = result {
        emit_error(&app, Some(&note_id), &e);
    }
    notes::purge_note_assets_if_gone(&app, &note_id);
}

/// `transcribe_status` for one run: on when made, off when dropped, a panic
/// included.
struct TranscribeStatus {
    app: AppHandle,
    note_id: String,
}

impl TranscribeStatus {
    fn start(app: &AppHandle, note_id: &str) -> Self {
        emit_transcribe_status(app, note_id, true);
        TranscribeStatus { app: app.clone(), note_id: note_id.to_string() }
    }
}

impl Drop for TranscribeStatus {
    fn drop(&mut self) {
        emit_transcribe_status(&self.app, &self.note_id, false);
    }
}

/// Notes whose unretained take is still to be replayed, with how many. While a
/// note is here, `recording_start` and `transcribe_note` refuse it.
static OWED_REPLAYS: parking_lot::Mutex<std::collections::BTreeMap<String, usize>> =
    parking_lot::Mutex::new(std::collections::BTreeMap::new());

/// One owed replay of a note, released when dropped.
pub(super) struct OwedReplay(String);

impl OwedReplay {
    pub(super) fn mark(note_id: &str) -> Self {
        *OWED_REPLAYS.lock().entry(note_id.to_string()).or_insert(0) += 1;
        OwedReplay(note_id.to_string())
    }
}

impl Drop for OwedReplay {
    fn drop(&mut self) {
        let mut owed = OWED_REPLAYS.lock();
        if let Some(n) = owed.get_mut(&self.0) {
            *n -= 1;
            if *n == 0 {
                owed.remove(&self.0);
            }
        }
    }
}

pub(super) fn replay_owed(note_id: &str) -> bool {
    OWED_REPLAYS.lock().contains_key(note_id)
}

/// The prefix of the directories [`UnretainedStreams`] makes.
const UNRETAINED_PREFIX: &str = "humla-unretained-";

/// Delete unretained streams a quit or crash left behind. Run once at launch,
/// before any replay can have started.
pub fn sweep_unretained_streams() -> usize {
    sweep_unretained_streams_in(&std::env::temp_dir())
}

fn sweep_unretained_streams_in(parent: &std::path::Path) -> usize {
    let Ok(entries) = std::fs::read_dir(parent) else { return 0 };
    entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(UNRETAINED_PREFIX))
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| std::fs::remove_dir_all(e.path()).is_ok())
        .count()
}

async fn transcribe_pending_take(
    app: &AppHandle,
    note_id: &str,
    take: PendingTake,
    streams: &UnretainedStreams,
) -> Result<(), String> {
    let state: State<AppState> = app.state();
    check_provider_ready(app, &state, note_id).await.map_err(NotReady::message)?;
    let entry = sessions::SessionEntry {
        id: take.session_id.clone(),
        index: 0,
        started_at: take.started_at.clone(),
        duration_ms: take.captured_duration_ms,
        streams: take.streams.clone(),
        transcribed: false,
    };
    let source = TakeSource { entry, mic: streams.mic.clone(), sys: streams.sys.clone(), pending: Some(take) };
    transcribe_take_sources(app, note_id, vec![source]).await
}

async fn transcribe_takes(
    app: &AppHandle,
    note_id: &str,
    scope: sessions::TranscribeScope,
) -> Result<(), String> {
    let app_dir = app.path().app_data_dir().map_err(err)?;
    let recordings = sessions::recordings_dir(&app_dir, note_id);
    let takes: Vec<TakeSource> = sessions::takes_to_transcribe(&recordings, scope)
        .into_iter()
        .map(|(entry, dir)| TakeSource::retained(entry, &dir))
        .collect();
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
    transcribe_take_sources(app, note_id, takes).await
}

/// Replay, diarize and commit `takes`. The caller has checked the provider.
async fn transcribe_take_sources(app: &AppHandle, note_id: &str, takes: Vec<TakeSource>) -> Result<(), String> {
    let state: State<AppState> = app.state();

    // The run's shape, settled before a single chunk decodes: each take's
    // retained streams are replayed one after the other, so a take that kept
    // both costs twice its duration. Every stream of every take reports into
    // this one handle, since the fraction is over the whole run.
    let progress = Arc::new(parking_lot::Mutex::new(ReplayProgress::new(
        takes
            .iter()
            .map(|t| TakeWork {
                streams: t.mic.is_some() as u32 + t.sys.is_some() as u32,
                duration_ms: t.entry.duration_ms,
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
    for (take_index, TakeSource { mut entry, mic: mic_wav, sys: sys_wav, pending }) in takes.into_iter().enumerate() {
        // Emitted, not just recorded: this is where the step goes back to
        // transcribing after the previous take's diarize.
        let snapshot = {
            let mut p = progress.lock();
            p.begin_take(take_index);
            p.snapshot()
        };
        emit_transcribe_progress(app, note_id, snapshot);
        let t_transcribe = std::time::Instant::now();

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
            if pending_take_step(pending.is_some(), true) == PendingStep::Discard {
                // Never in the manifest, and nothing of it to keep.
                discard_empty_take(app, note_id, &entry.id).await;
                failures.push("The recording produced no text.".to_string());
            } else {
                failures.push(format!(
                    "Recording {} produced no text. Its audio is still saved — try again.",
                    entry.index
                ));
            }
            continue;
        }
        let is_pending = pending.is_some();
        if let (PendingStep::Finalize, Some(p)) = (pending_take_step(is_pending, false), pending) {
            finalize_session(
                app,
                note_id,
                &p.session_id,
                &p.started_at,
                p.streams,
                p.captured_duration_ms,
                sessions::Transcribed::No,
            )
            .await;
            if let Some(index) = take_index_of(app, note_id, &entry.id) {
                entry.index = index;
            }
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
            replay_audio(is_pending),
        )
        .await;
        take_timings.diarize_ms = ms_since(t_diarize);
        if let Ok(costs) = &diarized {
            take_timings.echo_ms = costs.echo_ms;
            take_timings.playback_ms = costs.playback_ms;
        }
        timings.takes.push(take_timings);
        if let Err(e) = diarized {
            if is_pending {
                // Nothing of it reached the transcript, so it leaves no row.
                forget_pending_take(app, note_id, &entry.id).await;
            }
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

#[derive(Debug, PartialEq, Eq)]
enum PendingStep {
    /// Already in the manifest, or nothing to decide yet.
    None,
    /// Not in the manifest and produced no text: drop what was written for it.
    Discard,
    /// Not in the manifest and has text: add it before its timeline is written.
    Finalize,
}

fn pending_take_step(pending: bool, produced_no_text: bool) -> PendingStep {
    match (pending, produced_no_text) {
        (false, _) => PendingStep::None,
        (true, true) => PendingStep::Discard,
        (true, false) => PendingStep::Finalize,
    }
}

/// What a replayed take's playback may hold: a take that was never retained
/// keeps no audio, even if the gate reopened during its replay.
fn replay_audio(pending: bool) -> PlaybackAudio {
    if pending {
        PlaybackAudio::Never
    } else {
        PlaybackAudio::FollowGate
    }
}

/// Take a pending take back out: its manifest row and whatever its failed
/// diarize wrote. The transcript is only committed on success, so it holds
/// nothing of this take.
async fn forget_pending_take(app: &AppHandle, note_id: &str, session_id: &str) {
    let Ok(app_dir) = app.path().app_data_dir() else { return };
    let recordings = sessions::recordings_dir(&app_dir, note_id);
    let lock = app.state::<AppState>().manifest_lock.clone();
    let _manifest_guard = lock.lock().await;
    let dir = sessions::session_dir(&recordings, session_id);
    let id = session_id.to_string();
    let removed = tokio::task::spawn_blocking(move || {
        let row = sessions::remove_session(&recordings, &id);
        let _ = std::fs::remove_dir_all(&dir);
        row
    })
    .await;
    if let Ok(Err(e)) = removed {
        eprintln!("transcribe: couldn't remove take {session_id} from the manifest: {e}");
    }
}

/// A take's number in the note's manifest.
fn take_index_of(app: &AppHandle, note_id: &str, session_id: &str) -> Option<u32> {
    let app_dir = app.path().app_data_dir().ok()?;
    sessions::resolve_sessions(&sessions::recordings_dir(&app_dir, note_id))
        .into_iter()
        .find(|(e, _)| e.id == session_id)
        .map(|(e, _)| e.index)
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

#[cfg(test)]
mod tests {
    use super::*;

    async fn wav(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
        let p = dir.join(name);
        crate::wav::write_i16_mono_16k(&p, &[1i16; 1600]).await.unwrap();
        p
    }

    #[tokio::test]
    async fn streams_move_out_of_the_capture_dir_and_are_deleted_on_drop() {
        let capture = tempfile::tempdir().unwrap();
        let parent = tempfile::tempdir().unwrap();
        let mic = wav(capture.path(), "mic-full.wav").await;
        let sys = wav(capture.path(), "sys-full.wav").await;
        let streams = UnretainedStreams::take_into(parent.path(), Some(&mic), Some(&sys)).await.unwrap().unwrap();
        assert!(!mic.exists() && !sys.exists(), "moved, not copied");
        let (moved_mic, moved_sys) = (streams.mic.clone().unwrap(), streams.sys.clone().unwrap());
        assert!(moved_mic.exists() && moved_sys.exists());
        assert!(moved_mic.starts_with(parent.path()));
        drop(streams);
        assert!(!moved_mic.exists() && !moved_sys.exists());
        assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 0, "its directory is gone too");
    }

    #[tokio::test]
    async fn streams_are_deleted_when_the_replay_fails() {
        let capture = tempfile::tempdir().unwrap();
        let parent = tempfile::tempdir().unwrap();
        let mic = wav(capture.path(), "mic-full.wav").await;
        async fn failing_replay(streams: UnretainedStreams) -> Result<(), String> {
            let _held = streams;
            Err("provider refused".into())
        }
        let streams = UnretainedStreams::take_into(parent.path(), Some(&mic), None).await.unwrap().unwrap();
        assert!(streams.sys.is_none());
        assert!(failing_replay(streams).await.is_err());
        assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn no_stream_means_nothing_to_replay() {
        let parent = tempfile::tempdir().unwrap();
        let gone = parent.path().join("missing.wav");
        assert!(UnretainedStreams::take_into(parent.path(), None, None).await.unwrap().is_none());
        assert!(UnretainedStreams::take_into(parent.path(), Some(&gone), None).await.unwrap().is_none());
        assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_notes_transcribe_slot_is_held_once_and_given_back_on_drop() {
        let set = Arc::new(parking_lot::Mutex::new(std::collections::HashSet::new()));
        let held = TranscribeSlot::try_take_in(&set, "n1").unwrap();
        assert!(TranscribeSlot::try_take_in(&set, "n1").is_none(), "a second run waits");
        assert!(TranscribeSlot::try_take_in(&set, "n2").is_some(), "other notes are free");
        drop(held);
        assert!(TranscribeSlot::try_take_in(&set, "n1").is_some());
    }

    #[test]
    fn a_pending_take_enters_the_manifest_only_with_text() {
        assert_eq!(pending_take_step(true, true), PendingStep::Discard);
        assert_eq!(pending_take_step(true, false), PendingStep::Finalize);
        assert_eq!(pending_take_step(false, true), PendingStep::None, "a retained take keeps its row and audio");
        assert_eq!(pending_take_step(false, false), PendingStep::None);
    }

    #[test]
    fn the_post_stop_chain_hands_the_replay_off_instead_of_awaiting_it() {
        let capture = include_str!("capture.rs");
        let chain = &capture[capture.find("async fn post_stop_chain_inner").unwrap()..];
        let call = chain.find("transcribe_unretained_take(").expect("the chain hands the take off");
        let before = &chain[..call];
        assert!(before.trim_end().ends_with("spawn(super::deferred::"), "spawned, so the capture slot is released");
        let after = &chain[call..];
        let close = after.find("));").unwrap();
        assert!(!after[..close + 3].contains(".await"));
    }
    #[test]
    fn the_launch_sweep_deletes_only_leftover_unretained_streams() {
        let parent = tempfile::tempdir().unwrap();
        let left = parent.path().join(format!("{UNRETAINED_PREFIX}abc"));
        std::fs::create_dir_all(&left).unwrap();
        std::fs::write(left.join("mic.wav"), b"RIFF").unwrap();
        let other = parent.path().join("humla-replay-n1-xyz");
        std::fs::create_dir_all(&other).unwrap();
        let file = parent.path().join(format!("{UNRETAINED_PREFIX}not-a-dir"));
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(sweep_unretained_streams_in(parent.path()), 1);
        assert!(!left.exists());
        assert!(other.exists() && file.exists());
        assert_eq!(sweep_unretained_streams_in(parent.path()), 0);
    }

    #[tokio::test]
    async fn the_launch_sweep_finds_what_a_replay_leaves_behind() {
        let capture = tempfile::tempdir().unwrap();
        let parent = tempfile::tempdir().unwrap();
        let mic = wav(capture.path(), "mic-full.wav").await;
        let streams = UnretainedStreams::take_into(parent.path(), Some(&mic), None).await.unwrap().unwrap();
        std::mem::forget(streams);
        assert_eq!(sweep_unretained_streams_in(parent.path()), 1);
        assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 0);
    }

    #[test]
    fn an_owed_replay_holds_the_note_until_every_mark_is_dropped() {
        let note = format!("owed-{}", uuid::Uuid::new_v4());
        assert!(!replay_owed(&note));
        let first = OwedReplay::mark(&note);
        let second = OwedReplay::mark(&note);
        assert!(replay_owed(&note));
        drop(first);
        assert!(replay_owed(&note), "one replay still owed");
        drop(second);
        assert!(!replay_owed(&note));
    }

    #[test]
    fn an_unretained_replay_never_writes_audio() {
        assert_eq!(replay_audio(true), PlaybackAudio::Never, "even if the gate reopened during the replay");
        assert_eq!(replay_audio(false), PlaybackAudio::FollowGate);
    }

    #[test]
    fn the_chain_marks_the_replay_owed_before_handing_it_off() {
        let capture = include_str!("capture.rs");
        let chain = &capture[capture.find("async fn post_stop_chain_inner").unwrap()..];
        let mark = chain.find("OwedReplay::mark(&note_id)").expect("marked in the chain");
        let spawn = chain.find("spawn(super::deferred::transcribe_unretained_take(").unwrap();
        assert!(mark < spawn);
        let start = &capture[capture.find("pub async fn recording_start").unwrap()..];
        assert!(start.contains("super::deferred::replay_owed(&note_id)"), "recording_start refuses an owed note");
    }
    #[test]
    fn a_pending_take_whose_diarize_fails_is_taken_back_out() {
        let source = include_str!("deferred.rs");
        let fail = source.find("if let Err(e) = diarized {").unwrap();
        let branch = &source[fail..fail + source[fail..].find("break;").unwrap()];
        assert!(branch.contains("if is_pending") && branch.contains("forget_pending_take(app, note_id, &entry.id)"));
    }
}
