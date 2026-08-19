//! Menu-bar commands (#21). Thin wrappers over `crate::menubar`; the tray, the
//! hotkey registration and the headless recording path all live there.

use crate::menubar;
use tauri::AppHandle;

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
