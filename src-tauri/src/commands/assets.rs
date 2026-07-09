//! Path + filesystem commands for per-note assets (audio, diagnostics,
//! playback). Pure path resolution + directory listing over the app's
//! data directory, surfaced to the frontend via `commands::*`
//! (re-exported from the parent module).

use super::err;
use tauri::{AppHandle, Manager};

#[tauri::command]
pub fn app_data_dir(app: AppHandle) -> Result<String, String> {
    let path = app.path().app_data_dir().map_err(err)?;
    path.to_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "non-utf8 path".to_string())
}

/// CPU architecture of the running process (`std::env::consts::ARCH`,
/// e.g. "aarch64" or "x86_64"). The onboarding Transcription step reads
/// this to flip the "recommended" badge from on-device to cloud on Intel
/// Macs, where local Whisper inference is markedly slower.
#[tauri::command]
pub fn system_arch() -> String {
    std::env::consts::ARCH.to_string()
}

/// Path to the per-note diagnostics directory. Always returns a valid
/// path (whether or not files actually exist there yet); the frontend
/// uses it to open the directory in Finder so the user can inspect the
/// JSON dumps directly.
#[tauri::command]
pub fn note_diagnostics_dir(app: AppHandle, note_id: String) -> Result<String, String> {
    let path = app
        .path()
        .app_data_dir()
        .map_err(err)?
        .join("diagnostics")
        .join(&note_id);
    path.to_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "non-utf8 path".to_string())
}

/// Path to the per-note retained-audio directory. The directory only
/// exists when `keep_audio` was on at recording time; the frontend
/// checks via `note_audio_files` first before offering the open
/// affordance.
#[tauri::command]
pub fn note_audio_dir(app: AppHandle, note_id: String) -> Result<String, String> {
    let path = app
        .path()
        .app_data_dir()
        .map_err(err)?
        .join("recordings")
        .join(&note_id);
    path.to_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "non-utf8 path".to_string())
}

/// Lists which retained audio files exist for a note. Empty vec when
/// keep_audio was off at recording time, or after a manual cleanup.
#[tauri::command]
pub fn note_audio_files(app: AppHandle, note_id: String) -> Result<Vec<String>, String> {
    // Retained WAVs now live inside per-session subdirs (or flat for legacy
    // notes). Scan every resolved session dir so the "Audio" Finder affordance
    // still lights up when any take kept its source audio.
    let app_dir = app.path().app_data_dir().map_err(err)?;
    let recordings = crate::sessions::recordings_dir(&app_dir, &note_id);
    let mut out = std::collections::BTreeSet::<String>::new();
    for (_, dir) in crate::sessions::resolve_sessions(&recordings) {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    if name.ends_with(".wav") {
                        out.insert(name.to_string());
                    }
                }
            }
        }
    }
    Ok(out.into_iter().collect())
}

/// Open a local path (file or directory) in Finder via macOS's `open`
/// command. The shell plugin's frontend `open()` is scoped to a few
/// allowed HTTPS URLs by default; opening arbitrary user-data paths
/// from the renderer is rejected silently. We bypass that by invoking
/// the system `open` directly from the backend, matching the pattern
/// used by `permissions_open_settings`.
///
/// Constrained to paths under the app's data directory: a compromised
/// renderer otherwise gets a generic file/URL opener via macOS `open`,
/// which can hand off to arbitrary URL schemes. Both real callers
/// (`note_diagnostics_dir`, `note_audio_dir`) live under app_data_dir,
/// so the constraint costs nothing at the seams.
#[tauri::command]
pub fn open_in_finder(app: AppHandle, path: String) -> Result<(), String> {
    let app_dir = app.path().app_data_dir().map_err(err)?;
    let app_dir_canon = std::fs::canonicalize(&app_dir).map_err(err)?;
    // canonicalize() fails with a cryptic raw io error ("No such file or
    // directory (os error 2)") if the path is gone — which can happen in the
    // TOCTOU window between the frontend listing the folder and the user
    // clicking (e.g. a cleanup removed it). Surface a clear message instead.
    if !std::path::Path::new(&path).exists() {
        return Err("folder no longer exists".into());
    }
    let requested = std::fs::canonicalize(&path).map_err(err)?;
    if !requested.starts_with(&app_dir_canon) {
        return Err("path outside app data dir".into());
    }
    std::process::Command::new("open")
        .arg(&requested)
        .spawn()
        .map_err(|e| format!("open: {e}"))?;
    Ok(())
}

/// Path to the per-note `playback.wav` if it exists. The frontend
/// converts this to a `tauri://` URL via `convertFileSrc` and feeds it
/// to an `<audio>` element. Always present for recordings made on
/// builds that include the playback feature; missing for older notes.
#[tauri::command]
pub fn note_playback_path(app: AppHandle, note_id: String) -> Result<Option<String>, String> {
    // Back-compat single-file resolver: the latest session's playback.wav
    // (falling back to the flat legacy path for pre-#16 notes). The
    // session-aware player prefers `note_session_playback_path`.
    let app_dir = app.path().app_data_dir().map_err(err)?;
    let recordings = crate::sessions::recordings_dir(&app_dir, &note_id);
    let path = crate::sessions::latest_session_dir(&recordings).join("playback.wav");
    if !path.exists() {
        return Ok(None);
    }
    Ok(path.to_str().map(|s| s.to_string()))
}

/// Lists which diagnostic dumps exist for a note (e.g. ["community1-mic.json",
/// "sortformer-sys.json"]). Empty vec when no diarize has run yet.
#[tauri::command]
pub fn note_diagnostics_files(app: AppHandle, note_id: String) -> Result<Vec<String>, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(err)?
        .join("diagnostics")
        .join(&note_id);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                if name.ends_with(".json") {
                    out.push(name.to_string());
                }
            }
        }
    }
    out.sort();
    Ok(out)
}
