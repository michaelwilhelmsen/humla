//! Client CRUD commands (issue #43), plus the note→client assignment. Thin
//! wrappers over `db::*`, surfaced via `commands::clients_*` / `notes_set_client`.
//!
//! Clients sync workspace-scoped, mirroring folders (issue #49): create/rename
//! ping `client_upserted`, delete pings `client_deleted` (a hard local delete →
//! the observer must tombstone the remote record). Un-tag/assign also re-push
//! the affected notes, since a note's `updated_at` is bumped and its
//! `client_id` reference travels with it.

use super::err;
use crate::db;
use crate::AppState;
use tauri::State;

#[tauri::command]
pub fn clients_list(state: State<AppState>) -> Result<Vec<db::Client>, String> {
    let conn = state.db.lock();
    let workspace = super::cloud::active_workspace(&conn);
    db::list_clients(&conn, &workspace).map_err(err)
}

#[tauri::command]
pub fn clients_create(state: State<AppState>, name: String) -> Result<db::Client, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Client name cannot be empty".into());
    }
    let client = {
        let conn = state.db.lock();
        let workspace = super::cloud::active_workspace(&conn);
        db::create_client(&conn, trimmed, &workspace).map_err(err)?
    };
    state.sync.client_upserted(&client.id);
    Ok(client)
}

#[tauri::command]
pub fn clients_rename(state: State<AppState>, id: String, name: String) -> Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Client name cannot be empty".into());
    }
    {
        let conn = state.db.lock();
        db::rename_client(&conn, &id, trimmed).map_err(err)?;
    }
    state.sync.client_upserted(&id);
    Ok(())
}

#[tauri::command]
pub fn clients_delete(state: State<AppState>, id: String) -> Result<(), String> {
    let untagged = {
        let conn = state.db.lock();
        db::delete_client(&conn, &id).map_err(err)?
    };
    state.sync.client_deleted(&id);
    // delete_client un-tags this client's notes and bumps their updated_at;
    // re-push each so the un-tag propagates once notes sync.
    for note_id in &untagged {
        state.sync.note_upserted(note_id);
    }
    Ok(())
}

#[tauri::command]
pub fn notes_set_client(
    state: State<AppState>,
    id: String,
    client_id: Option<String>,
) -> Result<(), String> {
    {
        let conn = state.db.lock();
        db::set_note_client(&conn, &id, client_id.as_deref()).map_err(err)?;
    }
    state.sync.note_upserted(&id); // tagging bumps updated_at; treat as an upsert
    Ok(())
}
