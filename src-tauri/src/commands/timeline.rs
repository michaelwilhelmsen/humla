//! A note's timeline: the per-take `timeline.jsonl` the reader shows and its
//! serialization from a take's chunks, the session list, repairing a transcript
//! the timeline no longer covers, and the per-chunk label/text edits that
//! rewrite it.

use super::diarization::offset_speaker_label;
use super::transcript::{dedup_mic_against_sys, estimated_speech_ms, refine_pieces, LabelledPiece};
use super::{err, note_changed_for_sync, session_changed_for_sync};
use crate::recording::{ChunkRecord, ChunkSource, TranscriptPayload};
use crate::{db, sessions, AppState};
use tauri::{AppHandle, Emitter, Manager, State};

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
            // Recovered text, already in the note — there is nothing left to
            // transcribe here (#146).
            transcribed: true,
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
    /// Whether pressing Transcribe would do anything for this take (#146): it
    /// is untranscribed *and* still holds the raw streams a replay reads.
    ///
    /// Both halves matter, and both are answered here so the frontend never
    /// has to reason about retention rules. A take whose audio was swept away
    /// by Settings → "Delete stored audio" can never produce text, so offering
    /// the action for it would leave a button that fails forever — silently,
    /// since the deferred pass skips a take it can't replay.
    ///
    /// The raw-stream half is [`sessions::session_has_replayable_audio`], not
    /// `session_has_audio`: a take reduced to its mixed `playback.wav` still
    /// has audio on disk and still can't be replayed.
    pub can_transcribe: bool,
    /// Whether pressing **Re-transcribe** would do anything for this take: its
    /// raw streams are still on disk, whether or not it already has text.
    ///
    /// Implied by [`Self::can_transcribe`] and deliberately still its own
    /// field: the two drive different controls, and having the note view derive
    /// one from the other would put the "which takes does a run cover" rule in
    /// two languages. `sessions::takes_to_transcribe` is the single answer both
    /// of these mirror.
    pub can_retranscribe: bool,
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
            has_playback: sessions::playback_file(&dir).is_some(),
            can_transcribe: !e.transcribed && sessions::session_has_replayable_audio(&dir),
            can_retranscribe: sessions::session_has_replayable_audio(&dir),
        })
        .collect();
    Ok(out)
}

/// Note ids that hold at least one recording session.
///
/// One sweep of the recordings root rather than a `note_sessions` call per
/// note, so the library views can tell a note somebody recorded from one
/// somebody typed. A manifest entry answers this, not the audio beside it: a
/// take swept by "Delete stored audio" was still recorded, and a take captured
/// with **Transcribe manually** on has no text to be recognised by.
#[tauri::command]
pub fn notes_with_recordings(app: AppHandle) -> Result<Vec<String>, String> {
    let app_dir = app.path().app_data_dir().map_err(err)?;
    let Ok(entries) = std::fs::read_dir(sessions::recordings_root(&app_dir)) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Some(note_id) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if !sessions::resolve_sessions(&dir).is_empty() {
            out.push(note_id);
        }
    }
    Ok(out)
}

/// Path to a specific session's playback audio, or `None` when absent. The
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
    Ok(sessions::playback_file(&dir).and_then(|p| p.to_str().map(str::to_string)))
}

/// Parse a `timeline.jsonl` into raw JSON values, skipping blank/malformed
/// lines. Shared by the per-chunk edit commands.
pub(super) fn read_timeline_values(path: &std::path::Path) -> Vec<serde_json::Value> {
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
pub(super) fn group_values_to_transcript(values: &[serde_json::Value]) -> String {
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
pub(super) fn rebuild_note_transcript(app: &AppHandle, note_id: &str) -> Result<String, String> {
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
pub(super) fn commit_rebuilt_transcript(app: &AppHandle, note_id: &str, transcript: String) -> Result<(), String> {
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

/// Serialise the chunk log as a per-chunk JSONL — one entry per
/// utterance, not per speaker turn. The saved transcript still groups
/// same-label runs (for summary readability), but the playback view
/// wants finer granularity: each ~5–15 s VAD chunk is the natural
/// click-to-seek and highlight unit, so glueing them by label hides
/// internal sentence boundaries from the player and makes the active-
/// turn highlight feel sluggish on long monologues. Runs dedup first
/// so echoed mic chunks don't generate phantom entries.
pub(super) fn serialize_timeline(
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

    // First flatten chunks → pieces and run the same corrections the DB
    // transcript gets (`refine_pieces`): without them the transcript string
    // collapses correctly while the playback view (driven by timeline.jsonl)
    // still renders per-chunk speaker flicker and stray sentence fragments.
    // Each piece carries its stream and the base its word timings are
    // relative to; a boundary move can hand a piece words from an earlier
    // chunk, so the base is the piece's own, not its chunk's.
    struct Entry {
        source: ChunkSource,
        start_ms: u64,
        text: String,
        label: String,
        words: Vec<serde_json::Value>,
        max_word_end_abs: Option<u64>,
    }
    let mut pieces: Vec<LabelledPiece> = Vec::new();
    for chunk in &sorted {
        for mut piece in split_chunk(chunk) {
            if !piece.text.trim().is_empty() {
                // A splitter may look labels up against a shifted copy of the
                // chunk (unify does, in concat time), but word timings are
                // always relative to this chunk's own start.
                piece.base_ms = chunk.start_ms;
                piece.source = chunk.source;
                pieces.push(piece);
            }
        }
    }
    refine_pieces(&mut pieces);
    let entries: Vec<Entry> = pieces
        .iter()
        .filter_map(|piece| {
            let trimmed = piece.text.trim();
            if trimmed.is_empty() {
                return None;
            }
            // Convert word timings to stream-absolute by adding the piece's
            // base. The playback view's audio element runs in the merged
            // playback timeline; word timestamps inside a chunk are
            // chunk-relative until we rebase here.
            let words_abs: Vec<serde_json::Value> = piece
                .words
                .iter()
                .map(|w| {
                    serde_json::json!({
                        "text": w.text,
                        "start_ms": piece.base_ms.saturating_add(w.start_ms),
                        "end_ms": piece.base_ms.saturating_add(w.end_ms),
                    })
                })
                .collect();
            // Piece start: first word's absolute start when present,
            // else the chunk's start (whole-chunk fallback piece).
            let start_ms = piece
                .words
                .first()
                .map(|w| piece.base_ms.saturating_add(w.start_ms))
                .unwrap_or(piece.base_ms);
            let max_word_end_abs = piece
                .words
                .iter()
                .map(|w| piece.base_ms.saturating_add(w.end_ms))
                .max();
            Some(Entry {
                source: piece.source,
                start_ms,
                text: trimmed.to_string(),
                label: piece.label.clone().unwrap_or_default(),
                words: words_abs,
                max_word_end_abs,
            })
        })
        .collect();

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::diarization::{combine_with_snapshot, session_speaker_offset};
    use crate::commands::transcript::{build_labelled_transcript, single_piece};

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
    fn the_timeline_carries_the_same_speaker_boundaries_as_the_transcript() {
        // The boundary moves used to run on the transcript string alone, so
        // the reader (drawn from the timeline) still showed "It" on Speaker
        // 1, and the first rebuild from the timeline put it back in the
        // transcript too. Both now come out of `refine_pieces`.
        let chunks = vec![
            sys_with_words(
                0,
                vec![
                    ("starts", 0, 300), ("to", 300, 500), ("unravel", 500, 900), ("in", 900, 1_000),
                    ("the", 1_000, 1_100), ("perfect", 1_100, 1_500), ("way.", 1_500, 1_900),
                    ("It", 2_100, 2_300),
                ],
            ),
            sys_with_words(
                2_400,
                vec![
                    ("does", 0, 200), ("pay", 200, 400), ("off.", 400, 700), ("That's", 900, 1_200),
                    ("what's", 1_200, 1_500), ("really", 1_500, 1_800), ("nice.", 1_800, 2_200),
                ],
            ),
        ];
        let segments = vec![
            crate::diarize::Segment { start_ms: 0, end_ms: 2_350, speaker_id: "a".into() },
            crate::diarize::Segment { start_ms: 2_350, end_ms: 5_000, speaker_id: "b".into() },
        ];
        let labels: std::collections::HashMap<String, String> = [
            ("a".to_string(), "Speaker 1".to_string()),
            ("b".to_string(), "Speaker 2".to_string()),
        ]
        .into();
        let split = |c: &ChunkRecord| {
            crate::commands::transcript::split_by_labels(c, &segments, &labels)
        };

        let transcript = build_labelled_transcript(&chunks, &split);
        let values = jsonl_values(&serialize_timeline(&chunks, &split, 0));
        assert_eq!(
            transcript,
            "Speaker 1: starts to unravel in the perfect way.\nSpeaker 2: It does pay off. That's what's really nice."
        );
        assert_eq!(group_values_to_transcript(&values), transcript);

        // "It" moved with its timing: the second entry starts when it is
        // spoken, and its first word is still lit at 2.1 s.
        assert_eq!(values[1]["start_ms"].as_u64(), Some(2_100));
        assert_eq!(values[1]["words"][0]["text"].as_str(), Some("It"));
        assert_eq!(values[1]["words"][0]["start_ms"].as_u64(), Some(2_100));
        assert_eq!(values[0]["end_ms"].as_u64(), Some(1_900));
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
