//! Custom summary-prompt CRUD commands. Thin wrappers over `db::*`, surfaced
//! to the frontend via `commands::summary_prompts_*` (re-exported from the
//! parent module).

use super::err;
use crate::db;
use crate::AppState;
use tauri::State;

#[tauri::command]
pub fn summary_prompts_list(
    state: State<AppState>,
) -> Result<Vec<db::SummaryPrompt>, String> {
    let conn = state.db.lock();
    db::list_summary_prompts(&conn).map_err(err)
}

#[tauri::command]
pub fn summary_prompts_create(
    state: State<AppState>,
    name: String,
    content: String,
) -> Result<db::SummaryPrompt, String> {
    let trimmed_name = name.trim();
    if trimmed_name.is_empty() {
        return Err("Prompt name cannot be empty".into());
    }
    let prompt = {
        let conn = state.db.lock();
        db::create_summary_prompt(&conn, trimmed_name, &content).map_err(err)?
    };
    state.sync.summary_prompt_upserted(&prompt.id);
    Ok(prompt)
}

#[tauri::command]
pub fn summary_prompts_update(
    state: State<AppState>,
    id: String,
    name: String,
    content: String,
) -> Result<db::SummaryPrompt, String> {
    let trimmed_name = name.trim();
    if trimmed_name.is_empty() {
        return Err("Prompt name cannot be empty".into());
    }
    let prompt = {
        let conn = state.db.lock();
        db::update_summary_prompt(&conn, &id, trimmed_name, &content).map_err(err)?
    };
    state.sync.summary_prompt_upserted(&prompt.id);
    Ok(prompt)
}

#[tauri::command]
pub fn summary_prompts_delete(state: State<AppState>, id: String) -> Result<(), String> {
    {
        let conn = state.db.lock();
        db::delete_summary_prompt(&conn, &id).map_err(err)?;
    }
    state.sync.summary_prompt_deleted(&id);
    Ok(())
}
