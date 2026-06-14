//! Note CRUD commands. Thin wrappers over `db::*`, surfaced to the
//! frontend via `commands::notes_*` (re-exported from the parent module).

use super::{err, DEFAULT_LANGUAGE};
use crate::db::{self, Note, NotePatch};
use crate::AppState;
use tauri::State;

#[tauri::command]
pub fn notes_list(state: State<AppState>) -> Result<Vec<Note>, String> {
    let conn = state.db.lock();
    db::list_notes(&conn).map_err(err)
}

#[tauri::command]
pub fn notes_get(state: State<AppState>, id: String) -> Result<Note, String> {
    let conn = state.db.lock();
    db::get_note(&conn, &id).map_err(err)
}

#[tauri::command]
pub fn notes_create(state: State<AppState>) -> Result<Note, String> {
    let conn = state.db.lock();
    // New notes inherit the user's defaults for language + summary preset.
    // Both are overridable per-note from the note view; pre-feature notes
    // (empty language) fall back at transcribe / summary time.
    let default_language = db::get_setting(&conn, "language")
        .map_err(err)?
        .unwrap_or_else(|| DEFAULT_LANGUAGE.to_string());
    let default_preset = db::get_setting(&conn, "default_summary_preset")
        .map_err(err)?
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "meeting".to_string());
    db::create_note(&conn, &default_language, &default_preset).map_err(err)
}

#[tauri::command]
pub fn notes_update(state: State<AppState>, id: String, patch: NotePatch) -> Result<(), String> {
    let conn = state.db.lock();
    db::update_note(&conn, &id, &patch).map_err(err)
}

#[tauri::command]
pub fn notes_delete(state: State<AppState>, id: String) -> Result<(), String> {
    let conn = state.db.lock();
    db::delete_note(&conn, &id).map_err(err)
}

#[tauri::command]
pub fn notes_move(
    state: State<AppState>,
    id: String,
    folder_id: Option<String>,
) -> Result<(), String> {
    let conn = state.db.lock();
    db::move_note(&conn, &id, folder_id.as_deref()).map_err(err)
}
