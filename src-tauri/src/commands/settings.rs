//! Settings key-value commands. Thin wrappers over `db::*`, surfaced to the
//! frontend via `commands::settings_*` (re-exported from the parent module).

use super::err;
use crate::db;
use crate::AppState;
use tauri::State;

#[tauri::command]
pub fn settings_get(state: State<AppState>, key: String) -> Result<Option<String>, String> {
    let conn = state.db.lock();
    db::get_setting(&conn, &key).map_err(err)
}

#[tauri::command]
pub fn settings_set(state: State<AppState>, key: String, value: String) -> Result<(), String> {
    let conn = state.db.lock();
    db::set_setting(&conn, &key, &value).map_err(err)
}
