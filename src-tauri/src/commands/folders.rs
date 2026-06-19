//! Folder CRUD commands. Thin wrappers over `db::*`, surfaced to the
//! frontend via `commands::folders_*` (re-exported from the parent module).

use super::err;
use crate::db;
use crate::AppState;
use tauri::State;

#[tauri::command]
pub fn folders_list(state: State<AppState>) -> Result<Vec<db::Folder>, String> {
    let conn = state.db.lock();
    db::list_folders(&conn).map_err(err)
}

#[tauri::command]
pub fn folders_create(state: State<AppState>, name: String) -> Result<db::Folder, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Folder name cannot be empty".into());
    }
    let folder = {
        let conn = state.db.lock();
        db::create_folder(&conn, trimmed).map_err(err)?
    };
    state.sync.folder_upserted(&folder.id);
    Ok(folder)
}

#[tauri::command]
pub fn folders_rename(state: State<AppState>, id: String, name: String) -> Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Folder name cannot be empty".into());
    }
    {
        let conn = state.db.lock();
        db::rename_folder(&conn, &id, trimmed).map_err(err)?;
    }
    state.sync.folder_upserted(&id);
    Ok(())
}

#[tauri::command]
pub fn folders_delete(state: State<AppState>, id: String) -> Result<(), String> {
    let reparented = {
        let conn = state.db.lock();
        db::delete_folder(&conn, &id).map_err(err)?
    };
    state.sync.folder_deleted(&id);
    // delete_folder reparents this folder's notes to root and bumps their
    // updated_at; re-push each so the move propagates to other devices.
    for note_id in &reparented {
        state.sync.note_upserted(note_id);
    }
    Ok(())
}
