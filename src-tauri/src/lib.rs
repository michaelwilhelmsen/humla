mod db;
mod openai;
mod local_whisper;
mod diarize;
mod languages;
mod presets;
mod wav;
mod recording;
mod commands;

use std::sync::Arc;
use parking_lot::Mutex;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{Emitter, Manager};

pub struct AppState {
    pub db: Arc<Mutex<rusqlite::Connection>>,
    pub recording: Arc<Mutex<recording::RecordingSession>>,
    pub whisper: local_whisper::SharedContext,
    // Held for the duration of one chunk's transcription so back-to-back
    // chunks don't both read a stale trail snapshot. Sequential transcribes
    // mean each chunk's initial_prompt sees the *committed* output of every
    // prior chunk in this session.
    pub transcribe_gate: Arc<tokio::sync::Mutex<()>>,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            // Point GGML at our prebuilt default.metallib BEFORE any
            // whisper.cpp init runs. whisper-rs 0.13 ships a vendored
            // ggml whose runtime Metal-source compile path silently
            // breaks on some macOS setups (whisper.cpp#3009 — sed-
            // based ggml-common.h embed misfires under cmake), causing
            // shader compile to fail with `undeclared identifier
            // 'block_q5_1'` and falling back to BLAS (CPU). With
            // GGML_METAL_PATH_RESOURCES set to a directory containing
            // a precompiled `default.metallib`, GGML's loader uses
            // that instead and skips the broken source path entirely
            // — restoring full Metal GPU acceleration.
            //
            // Tauri's bundle config maps `resources/default.metallib`
            // to <Resources>/default.metallib in production, but in
            // dev mode the path layout is different — try a couple
            // of plausible locations and log whichever wins so a
            // missing metallib is visible in stderr instead of just
            // silently degrading to BLAS.
            if let Ok(resource_dir) = app.path().resource_dir() {
                let candidates = [
                    resource_dir.clone(),
                    resource_dir.join("resources"),
                    resource_dir.join("Resources"),
                ];
                let found = candidates
                    .iter()
                    .find(|p| p.join("default.metallib").exists())
                    .cloned();
                match found {
                    Some(dir) => {
                        eprintln!(
                            "ggml metal: GGML_METAL_PATH_RESOURCES={}",
                            dir.display()
                        );
                        std::env::set_var("GGML_METAL_PATH_RESOURCES", &dir);
                    }
                    None => {
                        eprintln!(
                            "ggml metal: default.metallib not found near {} — Metal will try runtime shader compile and likely fall back to BLAS (CPU). Tried: {}",
                            resource_dir.display(),
                            candidates.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "),
                        );
                    }
                }
            }

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
                transcribe_gate: Arc::new(tokio::sync::Mutex::new(())),
            });

            let menu = build_menu(app.handle())?;
            app.set_menu(menu)?;
            app.on_menu_event(|app, event| {
                if event.id().as_ref() == "check-for-updates" {
                    let _ = app.emit("menu://check-for-updates", ());
                }
            });

            // One-shot cleanup of pre-v0.8.0 streaming diarizer files
            // (pyannote_segmentation.mlmodelc + wespeaker_v2.mlmodelc).
            // Replaced by the community-1 offline set in v0.8.0 — same dir,
            // different filenames, so the old files would otherwise sit
            // there forever as dead weight. Gated on a settings flag so
            // this only runs once per install, never re-deleting files
            // upstream might legitimately reintroduce later.
            {
                let state: tauri::State<AppState> = app.state();
                let conn = state.db.lock();
                diarize::cleanup_legacy_streaming_models(app.handle(), &conn);
            }

            // One-shot migration of the legacy single-custom-prompt setting
            // into the summary_prompts table. Same flag-guarded shape as the
            // diarize cleanup above.
            {
                let state: tauri::State<AppState> = app.state();
                let conn = state.db.lock();
                if let Err(e) = db::migrate_summary_prompts(&conn) {
                    eprintln!("migrate_summary_prompts: {e}");
                }
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::notes_list,
            commands::notes_get,
            commands::notes_create,
            commands::notes_update,
            commands::notes_delete,
            commands::notes_move,
            commands::folders_list,
            commands::folders_create,
            commands::folders_rename,
            commands::folders_delete,
            commands::settings_get,
            commands::settings_set,
            commands::app_data_dir,
            commands::note_diagnostics_dir,
            commands::note_audio_dir,
            commands::note_audio_files,
            commands::note_diagnostics_files,
            commands::note_playback_path,
            commands::note_timeline,
            commands::note_timeline_rename,
            commands::note_timeline_set_chunk_label,
            commands::note_timeline_delete_chunk,
            commands::open_in_finder,
            commands::rediarize_note,
            commands::summary_prompts_list,
            commands::summary_prompts_create,
            commands::summary_prompts_update,
            commands::summary_prompts_delete,
            commands::api_key_get,
            commands::api_key_set,
            commands::api_key_test,
            commands::local_whisper_models,
            commands::local_whisper_download,
            commands::local_whisper_delete,
            commands::local_llm_list_models,
            commands::diarize_status,
            commands::diarize_download,
            commands::diarize_delete,
            commands::recording_start,
            commands::recording_stop,
            commands::recording_pause,
            commands::recording_resume,
            commands::recording_state,
            commands::summarize_note,
            commands::polish_note,
            commands::permissions_status,
            commands::permissions_request,
            commands::permissions_open_settings,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri app");
}

fn build_menu(app: &tauri::AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let app_name = app
        .config()
        .product_name
        .clone()
        .unwrap_or_else(|| "Humla".to_string());

    let about = PredefinedMenuItem::about(app, None, None)?;
    let check_for_updates = MenuItem::with_id(
        app,
        "check-for-updates",
        "Check for Updates…",
        true,
        None::<&str>,
    )?;
    let services = PredefinedMenuItem::services(app, None)?;
    let hide = PredefinedMenuItem::hide(app, None)?;
    let hide_others = PredefinedMenuItem::hide_others(app, None)?;
    let show_all = PredefinedMenuItem::show_all(app, None)?;
    let quit = PredefinedMenuItem::quit(app, None)?;
    let sep = || PredefinedMenuItem::separator(app);

    let app_submenu = Submenu::with_items(
        app,
        &app_name,
        true,
        &[
            &about,
            &sep()?,
            &check_for_updates,
            &sep()?,
            &services,
            &sep()?,
            &hide,
            &hide_others,
            &show_all,
            &sep()?,
            &quit,
        ],
    )?;

    let edit_submenu = Submenu::with_items(
        app,
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(app, None)?,
            &PredefinedMenuItem::redo(app, None)?,
            &sep()?,
            &PredefinedMenuItem::cut(app, None)?,
            &PredefinedMenuItem::copy(app, None)?,
            &PredefinedMenuItem::paste(app, None)?,
            &PredefinedMenuItem::select_all(app, None)?,
        ],
    )?;

    let window_submenu = Submenu::with_items(
        app,
        "Window",
        true,
        &[
            &PredefinedMenuItem::minimize(app, None)?,
            &PredefinedMenuItem::close_window(app, None)?,
        ],
    )?;

    Menu::with_items(app, &[&app_submenu, &edit_submenu, &window_submenu])
}
