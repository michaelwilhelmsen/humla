//! Note CRUD commands. Thin wrappers over `db::*`, surfaced to the
//! frontend via `commands::notes_*` (re-exported from the parent module).

use super::{err, DEFAULT_LANGUAGE};
use crate::db::{self, Note, NotePatch};
use crate::AppState;
use std::path::Path;
use tauri::{AppHandle, Manager, State};

#[tauri::command]
pub fn notes_list(state: State<AppState>) -> Result<Vec<Note>, String> {
    let conn = state.db.lock();
    // Scope to the active workspace ("" = Personal) so one workspace's notes
    // never bleed into another's view.
    let workspace = super::cloud::active_workspace(&conn);
    db::list_notes(&conn, &workspace).map_err(err)
}

/// Every distinct speaker label in the active workspace, with usage counters —
/// the suggestion source for the rename picker (#116 part 1).
///
/// Fetched once when the speaker strip mounts; the picker then filters and ranks
/// in TS per keystroke, so there is no IPC per keystroke.
#[tauri::command]
pub fn speaker_label_stats(state: State<AppState>) -> Result<Vec<db::SpeakerLabelStat>, String> {
    let conn = state.db.lock();
    let workspace = super::cloud::active_workspace(&conn);
    db::speaker_label_stats(&conn, &workspace).map_err(err)
}

#[tauri::command]
pub fn notes_get(state: State<AppState>, id: String) -> Result<Note, String> {
    let conn = state.db.lock();
    db::get_note(&conn, &id).map_err(err)
}

/// Where the sticky visibility default is stored, per workspace (#191).
///
/// Per workspace and not globally: "private" is the honest answer in a client's
/// shared workspace and the wrong one in your own, and a single global setting
/// would carry one workspace's habit into the next.
pub(crate) fn visibility_default_key(workspace: &str) -> String {
    format!("note_private_default:{workspace}")
}

/// Whether a new note in `workspace` starts private.
///
/// Shared unless the user's last explicit choice in THIS workspace was private —
/// today's behaviour for anyone who never touches the chip, and a habit that
/// carries for anyone who does. Personal notes are private by definition and the
/// flag means nothing there, so the question is never asked.
pub(crate) fn default_private(conn: &rusqlite::Connection, workspace: &str) -> bool {
    if workspace.is_empty() {
        return false;
    }
    db::get_setting(conn, &visibility_default_key(workspace))
        .ok()
        .flatten()
        .as_deref()
        == Some("1")
}

#[tauri::command]
pub fn notes_create(state: State<AppState>) -> Result<Note, String> {
    // New notes inherit the user's defaults for language + summary preset.
    // Both are overridable per-note from the note view; pre-feature notes
    // (empty language) fall back at transcribe / summary time.
    let note = {
        let conn = state.db.lock();
        let default_language = db::get_setting(&conn, "language")
            .map_err(err)?
            .unwrap_or_else(|| DEFAULT_LANGUAGE.to_string());
        let default_preset = db::get_setting(&conn, "default_summary_preset")
            .map_err(err)?
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "meeting".to_string());
        // Stamp the note with the active workspace so it syncs there (and only
        // shows there). Personal ("") notes stay local.
        let workspace = super::cloud::active_workspace(&conn);
        let note =
            db::create_note(&conn, &default_language, &default_preset, &workspace).map_err(err)?;
        if default_private(&conn, &workspace) {
            db::set_note_private(&conn, &note.id, true).map_err(err)?;
            db::get_note(&conn, &note.id).map_err(err)?
        } else {
            note
        }
    }; // drop the db guard before pinging sync (see SyncObserver contract)
    state.sync.note_upserted(&note.id);
    Ok(note)
}

#[tauri::command]
pub fn notes_update(state: State<AppState>, id: String, patch: NotePatch) -> Result<(), String> {
    {
        let conn = state.db.lock();
        db::update_note(&conn, &id, &patch).map_err(err)?;
    }
    state.sync.note_upserted(&id);
    Ok(())
}

#[tauri::command]
pub fn notes_delete(state: State<AppState>, id: String) -> Result<(), String> {
    {
        let conn = state.db.lock();
        db::delete_note(&conn, &id).map_err(err)?;
    }
    // Soft-delete (Trash): the row stays for restore, but the sync layer records
    // a tombstone so the deletion still propagates to other devices.
    state.sync.note_deleted(&id);
    Ok(())
}

/// Notes in the Trash for the active workspace (soft-deleted, recoverable).
#[tauri::command]
pub fn notes_list_trash(state: State<AppState>) -> Result<Vec<Note>, String> {
    let conn = state.db.lock();
    let workspace = super::cloud::active_workspace(&conn);
    db::list_trashed_notes(&conn, &workspace).map_err(err)
}

/// Restore a note from the Trash. Re-pushes it (un-tombstone) so it also
/// reappears for teammates.
#[tauri::command]
pub fn notes_restore(state: State<AppState>, id: String) -> Result<Note, String> {
    let note = {
        let conn = state.db.lock();
        db::restore_note(&conn, &id).map_err(err)?;
        db::get_note(&conn, &id).map_err(err)?
    };
    state.sync.note_upserted(&id);
    Ok(note)
}

/// Permanently delete a note from the Trash (hard delete, not recoverable). The
/// server copy is already tombstoned from the soft-delete, so this is local-only.
///
/// Purge is the point of no return, so every file the note keeps on disk goes
/// with it ([`purge_note_assets`]). Soft delete deliberately does NOT touch these
/// — a Trash-restore must keep playback intact.
#[tauri::command]
pub fn notes_purge(app: AppHandle, state: State<AppState>, id: String) -> Result<(), String> {
    {
        let conn = state.db.lock();
        db::purge_note(&conn, &id).map_err(err)?;
    } // drop the db guard before touching the filesystem
    purge_note_assets_best_effort(&app, &id);
    Ok(())
}

/// For a note whose row is already gone: the purge has happened, so a failure to
/// remove its files is logged, not returned.
pub(crate) fn purge_note_assets_best_effort(app: &AppHandle, note_id: &str) {
    let Ok(base) = app.path().app_data_dir() else { return };
    if let Err(e) = purge_note_assets(&base, note_id) {
        eprintln!("purge: could not remove note {note_id}'s files: {e}");
    }
}

/// Remove both directories a note keeps under the app data dir:
/// `recordings/<note_id>/` (retained audio, playback assets, timelines) and
/// `diagnostics/<note_id>/` (the diarize dumps, which hold the transcript text).
pub(crate) fn purge_note_assets(app_data_dir: &Path, note_id: &str) -> std::io::Result<()> {
    if !crate::sessions::is_safe_session_id(note_id) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "unsafe note id"));
    }
    let mut first_err = None;
    for root in ["recordings", "diagnostics"] {
        if let Err(e) = std::fs::remove_dir_all(app_data_dir.join(root).join(note_id)) {
            if e.kind() != std::io::ErrorKind::NotFound {
                first_err.get_or_insert(e);
            }
        }
    }
    first_err.map_or(Ok(()), Err)
}

/// Saved content revisions for a note, newest first (local version history).
#[tauri::command]
pub fn notes_revisions(state: State<AppState>, id: String) -> Result<Vec<db::NoteRevision>, String> {
    let conn = state.db.lock();
    db::list_note_revisions(&conn, &id).map_err(err)
}

/// Restore a note to a saved revision; the current state is snapshotted first so
/// it's undoable. Syncs the restored content.
#[tauri::command]
pub fn notes_restore_revision(
    state: State<AppState>,
    id: String,
    revision_id: String,
) -> Result<Note, String> {
    let note = {
        let conn = state.db.lock();
        db::restore_note_revision(&conn, &id, &revision_id).map_err(err)?;
        db::get_note(&conn, &id).map_err(err)?
    };
    state.sync.note_upserted(&id);
    Ok(note)
}

#[tauri::command]
pub fn notes_move(
    state: State<AppState>,
    id: String,
    folder_id: Option<String>,
) -> Result<(), String> {
    {
        let conn = state.db.lock();
        db::move_note(&conn, &id, folder_id.as_deref()).map_err(err)?;
    }
    state.sync.note_upserted(&id); // move bumps updated_at; treat as an upsert
    Ok(())
}

/// Move a note to a different workspace (`""` = Personal/local-only). Lets a
/// note recorded in Personal be shared to a team, or moved between workspaces.
/// The sync layer tombstones it in the old workspace and recreates it in the new
/// one (they're distinct remote records keyed on `(workspace, client_id)`).
#[tauri::command]
pub fn notes_set_workspace(
    state: State<AppState>,
    id: String,
    workspace_id: String,
) -> Result<(), String> {
    let from = {
        let conn = state.db.lock();
        let note = db::get_note(&conn, &id).map_err(err)?;
        if note.workspace_id == workspace_id {
            return Ok(()); // already there — nothing to do
        }
        db::set_note_workspace(&conn, &id, &workspace_id).map_err(err)?;
        note.workspace_id
    };
    state.sync.note_moved(&id, &from, &workspace_id);
    Ok(())
}

/// Whether `me` may change the visibility of a note owned by `owner`.
///
/// Only the author, and a workspace owner is not an exception — anything else
/// makes the word a lie. An empty `owner` is a note that has never synced, which
/// is this device's own; an unknown `me` (no cached session) can only act on one
/// of those. Mirrored by the client, which hides the control rather than offering
/// one that fails.
pub(crate) fn may_change_visibility(owner: &str, me: &str) -> bool {
    owner.is_empty() || (!me.is_empty() && owner == me)
}

/// Set a note's visibility inside its workspace (#191).
///
/// Two things happen on the way to private and only one of them is the note
/// itself: teammates who already synced it hold a complete copy, and a record
/// that leaves their view produces no pull event — so the sync layer is told
/// about the TRANSITION, not just the new value.
///
/// Also remembers the choice as this workspace's default for the next note.
#[tauri::command]
pub fn notes_set_private(
    state: State<AppState>,
    id: String,
    private: bool,
) -> Result<(), String> {
    {
        let conn = state.db.lock();
        let note = db::get_note(&conn, &id).map_err(err)?;
        // A Personal note is private already and has no workspace to be private
        // from; letting the flag be set there would only bank a default that the
        // next Personal note ignores.
        if note.workspace_id.is_empty() {
            return Err("A note outside a workspace is already private.".into());
        }
        // The server refuses a non-author's write too; refusing here keeps the local
        // row from disagreeing with it until the next pull, and keeps a revocation
        // the server will reject out of the outbox, where it would retry forever.
        if !may_change_visibility(&note.owner, &super::cloud::current_user_id()) {
            return Err("Only the note's author can change who can read it.".into());
        }
        // Banked BEFORE the no-op check: the default follows what the user CHOSE,
        // and choosing the answer a note already has is still choosing it. Behind
        // the check, picking Shared on an already-shared note recorded nothing and
        // the next note stayed private.
        db::set_setting(
            &conn,
            &visibility_default_key(&note.workspace_id),
            if private { "1" } else { "0" },
        )
        .map_err(err)?;
        if note.private == private {
            return Ok(());
        }
        db::set_note_private(&conn, &id, private).map_err(err)?;
    }
    if private {
        state.sync.note_withdrawn(&id);
    } else {
        state.sync.note_upserted(&id);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::purge_note_assets;
    use std::fs;
    use std::io::ErrorKind;

    #[test]
    fn purge_removes_the_notes_recordings_and_diagnostics_dirs() {
        let base = tempfile::tempdir().unwrap();
        let note_id = "note-123";
        let dir = base.path().join("recordings").join(note_id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("playback.wav"), b"fake").unwrap();
        fs::write(dir.join("mic-full.wav"), b"fake").unwrap();
        // The diarize dump holds every chunk's text: the transcript, in other words.
        let diag = base.path().join("diagnostics").join(note_id);
        fs::create_dir_all(&diag).unwrap();
        fs::write(diag.join("community1-mic.json"), br#"{"chunks":[{"text":"the price"}]}"#).unwrap();
        fs::write(diag.join("replay-timings.json"), b"{}").unwrap();
        let sibling = ["recordings", "diagnostics"].map(|root| base.path().join(root).join("note-456"));
        for d in &sibling {
            fs::create_dir_all(d).unwrap();
            fs::write(d.join("kept.json"), b"{}").unwrap();
        }

        purge_note_assets(base.path(), note_id).unwrap();

        assert!(!dir.exists(), "recordings/<note_id> should be gone after purge");
        assert!(!diag.exists(), "diagnostics/<note_id> should be gone after purge");
        for d in &sibling {
            assert!(d.join("kept.json").exists(), "a sibling note's {} must be untouched", d.display());
        }
    }

    #[test]
    fn purge_clears_every_root_before_reporting_a_failure() {
        let base = tempfile::tempdir().unwrap();
        // A plain file where the recordings dir should be, which remove_dir_all refuses.
        fs::create_dir_all(base.path().join("recordings")).unwrap();
        fs::write(base.path().join("recordings").join("note-123"), b"not a dir").unwrap();
        let diag = base.path().join("diagnostics").join("note-123");
        fs::create_dir_all(&diag).unwrap();
        fs::write(diag.join("community1-mic.json"), b"{}").unwrap();

        let e = purge_note_assets(base.path(), "note-123").unwrap_err();

        assert_ne!(e.kind(), ErrorKind::NotFound);
        assert!(!diag.exists(), "the transcript text goes even when the audio can't");
    }

    /// The id becomes a path segment under both roots. An empty id names each root
    /// itself and `..` the whole app data dir, so anything but a plain segment is
    /// refused before a single directory is removed.
    #[test]
    fn purge_refuses_an_id_that_is_not_a_plain_path_segment() {
        let base = tempfile::tempdir().unwrap();
        let app_data = base.path().join("app");
        let kept = ["recordings", "diagnostics"].map(|root| app_data.join(root).join("note-456"));
        for d in &kept {
            fs::create_dir_all(d).unwrap();
        }
        let outside = base.path().join("outside");
        fs::create_dir_all(&outside).unwrap();

        for id in ["", ".", "..", "note-456/..", "../../outside"] {
            let e = purge_note_assets(&app_data, id).unwrap_err();
            assert_eq!(e.kind(), ErrorKind::InvalidInput, "{id:?}");
        }

        for d in &kept {
            assert!(d.exists(), "{} must survive", d.display());
        }
        assert!(outside.exists(), "nothing outside the app data dir is reachable");
    }

    /// #191 — visibility is sticky per workspace, and shared until the user says
    /// otherwise. The default has to stay "shared" for anyone who never touches
    /// the chip, in a workspace they have never expressed an opinion about.
    #[test]
    fn visibility_default_is_shared_until_chosen_and_is_per_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open(&dir.path().join("visibility.sqlite")).unwrap();
        assert!(!super::default_private(&conn, "wsA"));

        crate::db::set_setting(&conn, &super::visibility_default_key("wsA"), "1").unwrap();
        assert!(super::default_private(&conn, "wsA"));
        // A habit in one workspace is not a habit in the next.
        assert!(!super::default_private(&conn, "wsB"));
        // Personal has no workspace to be private from, so it never asks.
        assert!(!super::default_private(&conn, ""));

        crate::db::set_setting(&conn, &super::visibility_default_key("wsA"), "0").unwrap();
        assert!(!super::default_private(&conn, "wsA"));
    }

    /// #191 — the author decides, and a workspace owner is not an exception.
    #[test]
    fn only_the_author_may_change_a_notes_visibility() {
        assert!(super::may_change_visibility("u-me", "u-me"));
        assert!(!super::may_change_visibility("u-bob", "u-me"), "not even as workspace owner");
        // Never synced → no owner → this device's own note.
        assert!(super::may_change_visibility("", "u-me"));
        assert!(super::may_change_visibility("", ""));
        // A synced note with no cached session can't be proven ours.
        assert!(!super::may_change_visibility("u-me", ""));
    }

    #[test]
    fn purge_is_ok_when_no_assets_exist() {
        let base = tempfile::tempdir().unwrap();
        // No recordings/ or diagnostics/ dir at all — a note that was never recorded.
        purge_note_assets(base.path(), "never-recorded").unwrap();
        // Idempotent: purging again is still fine.
        purge_note_assets(base.path(), "never-recorded").unwrap();

        // A note can have diagnostics and no recordings dir; the missing one must
        // not stop the other going.
        let diag = base.path().join("diagnostics").join("diagnosed-only");
        fs::create_dir_all(&diag).unwrap();
        fs::write(diag.join("sortformer-mic.json"), b"{}").unwrap();
        purge_note_assets(base.path(), "diagnosed-only").unwrap();
        assert!(!diag.exists());
    }
}
