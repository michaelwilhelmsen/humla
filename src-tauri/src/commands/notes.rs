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

#[tauri::command]
pub fn notes_get(state: State<AppState>, id: String) -> Result<Note, String> {
    let conn = state.db.lock();
    db::get_note(&conn, &id).map_err(err)
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
        db::create_note(&conn, &default_language, &default_preset, &workspace).map_err(err)?
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
/// Cascade (issue #19): purge is the point of no return, so the note's retained
/// audio / playback assets under `recordings/<note_id>/` go with it. Soft delete
/// deliberately does NOT touch these — a Trash-restore must keep playback intact.
#[tauri::command]
pub fn notes_purge(app: AppHandle, state: State<AppState>, id: String) -> Result<(), String> {
    {
        let conn = state.db.lock();
        db::purge_note(&conn, &id).map_err(err)?;
    } // drop the db guard before touching the filesystem
    // Best-effort asset cleanup: the DB row is already gone, which is the
    // primary effect. A missing directory is expected (notes without retained
    // audio) and is not an error; any other IO failure is swallowed so a
    // filesystem hiccup can't leave the note un-purgeable.
    if let Ok(base) = app.path().app_data_dir() {
        let _ = purge_note_assets(&base, &id);
    }
    Ok(())
}

/// Remove a note's `recordings/<note_id>/` directory (retained audio + the
/// always-written playback assets). Returns `Ok` when the directory never
/// existed — purging a note that had no assets is not an error. Any other IO
/// error propagates so callers/tests can observe a genuine failure.
pub(crate) fn purge_note_assets(app_data_dir: &Path, note_id: &str) -> std::io::Result<()> {
    let dir = app_data_dir.join("recordings").join(note_id);
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
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

#[cfg(test)]
mod tests {
    use super::purge_note_assets;
    use std::fs;

    #[test]
    fn purge_removes_the_notes_recordings_dir() {
        let base = tempfile::tempdir().unwrap();
        let note_id = "note-123";
        let dir = base.path().join("recordings").join(note_id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("playback.wav"), b"fake").unwrap();
        fs::write(dir.join("mic-full.wav"), b"fake").unwrap();
        assert!(dir.exists());

        purge_note_assets(base.path(), note_id).unwrap();

        assert!(!dir.exists(), "recordings/<note_id> should be gone after purge");
        // A sibling note's assets must be untouched.
        assert!(base.path().join("recordings").exists());
    }

    #[test]
    fn purge_is_ok_when_no_assets_exist() {
        let base = tempfile::tempdir().unwrap();
        // No recordings/ dir at all — a note that never retained audio.
        purge_note_assets(base.path(), "never-recorded").unwrap();
        // Idempotent: purging again is still fine.
        purge_note_assets(base.path(), "never-recorded").unwrap();
    }
}
