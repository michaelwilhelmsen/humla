mod db;
mod openai;
mod speechmatics;
mod local_whisper;
mod presets;
mod wav;
mod recording;
mod commands;

use std::sync::Arc;
use parking_lot::Mutex;
use tauri::Manager;

pub struct AppState {
    pub db: Arc<Mutex<rusqlite::Connection>>,
    pub recording: Arc<Mutex<recording::RecordingSession>>,
    pub whisper: local_whisper::SharedContext,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            let app_dir = app
                .path()
                .app_data_dir()
                .expect("app data dir");
            std::fs::create_dir_all(&app_dir).ok();
            let db_path = app_dir.join("notes.sqlite");
            let conn = db::open(&db_path).expect("open db");
            app.manage(AppState {
                db: Arc::new(Mutex::new(conn)),
                recording: Arc::new(Mutex::new(recording::RecordingSession::default())),
                whisper: local_whisper::new_shared(),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::notes_list,
            commands::notes_get,
            commands::notes_create,
            commands::notes_update,
            commands::notes_delete,
            commands::settings_get,
            commands::settings_set,
            commands::api_key_get,
            commands::api_key_set,
            commands::api_key_test,
            commands::speechmatics_api_key_get,
            commands::speechmatics_api_key_set,
            commands::speechmatics_api_key_test,
            commands::local_whisper_status,
            commands::local_whisper_download,
            commands::local_whisper_delete,
            commands::recording_start,
            commands::recording_stop,
            commands::recording_pause,
            commands::recording_resume,
            commands::recording_state,
            commands::summarize_note,
            commands::permissions_status,
            commands::permissions_request,
            commands::permissions_open_settings,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri app");
}
