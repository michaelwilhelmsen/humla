//! Transcription-routing config commands + reader. The typed
//! `TranscribeConfig` (default provider + per-language overrides) is the
//! single source of truth for STT dispatch. Commands are surfaced to the
//! frontend via `commands::*_transcribe_config` (re-exported from the
//! parent module). `read_transcribe_config` is also called by the
//! recording / transcribe paths that stay in the parent module, hence
//! `pub(crate)`.

use super::err;
use crate::db;
use crate::AppState;
use tauri::State;

/// Read the active transcription config (default + per-language
/// overrides). The Settings UI calls this on mount as the source of
/// truth for the Transcription tab and uses it to drive both the
/// Default provider section and the Per-language overrides list.
#[tauri::command]
pub fn get_transcribe_config(
    state: State<AppState>,
) -> Result<crate::stt::TranscribeConfig, String> {
    read_transcribe_config(&state).map_err(|e| e.to_string())
}

/// Persist a typed `TranscribeConfig` to settings. Frontend writes the
/// whole shape on every change so the choice (default + every per-
/// language entry) is atomic — no partial drift from one path
/// half-updating.
#[tauri::command]
pub fn set_transcribe_config(
    state: State<AppState>,
    config: crate::stt::TranscribeConfig,
) -> Result<(), String> {
    let json = serde_json::to_string(&config).map_err(err)?;
    let conn = state.db.lock();
    db::set_setting(&conn, "transcribe_config", &json).map_err(err)
}

/// Read the active transcription config (default + per-language
/// overrides) from the typed `transcribe_config` JSON. Falls back to a
/// hardcoded default when the row is absent or corrupt — defensive only;
/// `db::migrate_per_language_v4` ensures the row is always present after
/// one launch under v0.24+.
pub(crate) fn read_transcribe_config(
    state: &State<AppState>,
) -> anyhow::Result<crate::stt::TranscribeConfig> {
    let conn = state.db.lock();
    if let Some(json) = db::get_setting(&conn, "transcribe_config")? {
        if let Ok(cfg) = serde_json::from_str::<crate::stt::TranscribeConfig>(&json) {
            return Ok(cfg);
        }
        // Corrupted JSON — fall through to the default rather than locking
        // the user out over a malformed cache. Settings UI overwrites
        // this when the user opens the Transcription tab.
    }
    Ok(crate::stt::TranscribeConfig::default_fallback())
}
