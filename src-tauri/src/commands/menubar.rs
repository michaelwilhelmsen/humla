//! Menu-bar commands (#21). Thin wrappers over `crate::menubar`; the tray, the
//! hotkey registration and the headless recording path all live there.

use crate::menubar;
use tauri::AppHandle;
use tauri_plugin_autostart::ManagerExt;

/// The accelerator in force — the stored row, or the default when the user has
/// never set one. `""` means the hotkey is off.
#[tauri::command]
pub fn record_hotkey_get(app: AppHandle) -> Result<String, String> {
    Ok(menubar::stored_hotkey(&app))
}

/// Register `accel` and persist it. `""` turns the hotkey off.
///
/// Deliberately `async`: registering goes through the plugin's
/// `run_on_main_thread` round-trip, and an async command runs off the main
/// thread, so the answer comes back instead of the call waiting on itself.
/// It also registers *before* it writes, so a combination another app already
/// owns leaves the working shortcut in place rather than persisting a dead one.
#[tauri::command]
pub async fn record_hotkey_set(app: AppHandle, accel: String) -> Result<(), String> {
    menubar::set_hotkey(&app, &accel)
}

/// Whether Humla's own login item is installed (#207). The LaunchAgent on disk
/// is the record; nothing is stored in the settings table.
#[tauri::command]
pub fn launch_at_login_get(app: AppHandle) -> Result<bool, String> {
    app.autolaunch().is_enabled().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn launch_at_login_set(app: AppHandle, on: bool) -> Result<(), String> {
    if on {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        if let Some(problem) = menubar::login_item_path_problem(&exe) {
            return Err(problem.to_string());
        }
    }
    let manager = app.autolaunch();
    if on { manager.enable() } else { manager.disable() }.map_err(|e| e.to_string())
}
