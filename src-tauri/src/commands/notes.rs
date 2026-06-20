//! Note CRUD commands. Thin wrappers over `db::*`, surfaced to the
//! frontend via `commands::notes_*` (re-exported from the parent module).

use super::{err, DEFAULT_LANGUAGE};
use crate::db::{self, Note, NotePatch};
use crate::AppState;
use tauri::State;

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
    // delete_note is a hard DELETE — the sync layer records a tombstone so the
    // deletion propagates instead of the row silently reappearing on next pull.
    state.sync.note_deleted(&id);
    Ok(())
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
