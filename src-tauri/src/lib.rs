mod db;
mod html_text;
mod openai;
mod local_whisper;
mod diarize;
mod languages;
mod presets;
mod wav;
mod recording;
mod sessions;
mod commands;
mod stt;
mod chat;
mod embed;
pub mod sync;

use std::sync::Arc;
use parking_lot::Mutex;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{Emitter, Manager};

pub struct AppState {
    pub db: Arc<Mutex<rusqlite::Connection>>,
    pub recording: Arc<Mutex<recording::LiveCapture>>,
    pub whisper: local_whisper::SharedContext,
    // Held for the duration of one chunk's transcription so back-to-back
    // chunks don't both read a stale trail snapshot. Sequential transcribes
    // mean each chunk's initial_prompt sees the *committed* output of every
    // prior chunk in this session.
    pub transcribe_gate: Arc<tokio::sync::Mutex<()>>,
    // Per-provider Keychain cache, keyed by provider_id ("openai",
    // "deepgram", "groq"). Each provider's first read triggers exactly
    // one OS-level Keychain prompt; subsequent reads return from this
    // map. Populated lazily by `read_provider_api_key` and updated by
    // `set_provider_api_key` so settings UI writes take effect
    // immediately.
    pub api_key_cache: stt::ApiKeyCache,
    /// Notified after each successful local write. `NoopSync` in the
    /// open-source build; a commercial build installs an observer that
    /// enqueues the change for upload.
    pub sync: Arc<dyn sync::SyncObserver>,
    /// Serializes every read-modify-write of any note's `sessions.json`.
    /// The post-stop chain (`migrate_flat_if_needed` + `append_session`) and
    /// the cloud pull worker (`reconcile_manifest` + `write_manifest`) both
    /// rewrite the manifest with no other coordination; without this a
    /// concurrent pull + finalize would lose a session (last-write-wins over a
    /// stale in-memory copy). A `tokio::sync::Mutex` (not `parking_lot`) so the
    /// guard is `Send` and may be held across the `spawn_blocking` / network
    /// `.await`s inside those critical sections. One global lock (not per-note):
    /// manifest writes are rare and sub-millisecond, so the contention cost is
    /// nil and a single lock is impossible to mis-key.
    pub manifest_lock: Arc<tokio::sync::Mutex<()>>,
    /// Per-note re-entrancy guard for cross-session speaker unification
    /// (`unify_note_speakers`). Auto-unify in the post-stop chain can race a
    /// user-triggered `rediarize_note` (or two rapid Re-diarize clicks) for the
    /// *same* note; without serialization both passes clobber each other's
    /// scratch WAVs and session timelines. Each note gets its own
    /// `tokio::sync::Mutex` so a second unify for that note waits for the first,
    /// while different notes stay fully concurrent. Held only inside
    /// `unify_note_speakers` (a leaf — never across another unify), so it can't
    /// deadlock the post-stop chain.
    pub unify_locks: Arc<Mutex<std::collections::HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
    /// Stop signals for in-flight chat turns (issue #80), keyed by the pane the
    /// turn belongs to — today the anchor note id, since a note's Chat tab can
    /// have at most one turn running. `chat_send` registers a flag for the
    /// duration of the turn and removes it after; `chat_cancel` sets it, and
    /// no-ops when the key is absent (nothing in flight to stop).
    pub chat_cancels: Arc<Mutex<std::collections::HashMap<String, Arc<chat::CancelFlag>>>>,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // With the `cloud` feature (default), install the live cloud-sync observer:
    // it starts the worker once configured + signed in and restarts it on
    // login / workspace switch. Without the feature, sync is a no-op and the
    // app never touches the network for sync.
    #[cfg(feature = "cloud")]
    run_with_sync(commands::cloud_worker::install);

    #[cfg(not(feature = "cloud"))]
    run_with_sync(|_ctx| Arc::new(sync::NoopSync));
}

/// Entry point that lets a caller inject a sync implementation. `make_sync`
/// receives a [`sync::SyncContext`] (database handle + `AppHandle`) the moment
/// those exist during setup, and returns the observer to install on
/// [`AppState`]. The open-source [`run`] passes a factory that yields
/// [`sync::NoopSync`].
pub fn run_with_sync<F>(make_sync: F)
where
    F: FnOnce(sync::SyncContext) -> Arc<dyn sync::SyncObserver> + Send + 'static,
{
    tauri::Builder::default()
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
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
            let db = Arc::new(Mutex::new(conn));
            // Build the sync observer now that the db handle and AppHandle
            // exist. In the open-source build this is `NoopSync` and the
            // factory ignores the context.
            let sync = make_sync(sync::SyncContext {
                db: db.clone(),
                app: app.handle().clone(),
            });
            app.manage(AppState {
                db,
                recording: Arc::new(Mutex::new(recording::LiveCapture::default())),
                whisper: local_whisper::new_shared(),
                transcribe_gate: Arc::new(tokio::sync::Mutex::new(())),
                api_key_cache: stt::new_cache(),
                sync,
                manifest_lock: Arc::new(tokio::sync::Mutex::new(())),
                unify_locks: Arc::new(Mutex::new(std::collections::HashMap::new())),
                chat_cancels: Arc::new(Mutex::new(std::collections::HashMap::new())),
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

            // v0.23 — collapse the legacy flat transcription settings keys
            // into the typed `transcribe_config` JSON and delete the orphan
            // rows. Same flag-guarded shape as the migrations above. Safe to
            // ship indefinitely; the flag check makes it a no-op after
            // first run.
            {
                let state: tauri::State<AppState> = app.state();
                let conn = state.db.lock();
                if let Err(e) = db::migrate_transcribe_config(&conn) {
                    eprintln!("migrate_transcribe_config: {e}");
                }
            }

            // v0.24 — wrap the v0.23 bare-ProviderConfig transcribe_config row
            // into the new TranscribeConfig { default, per_language } shape.
            // Idempotent via parse check; runs after migrate_transcribe_config
            // so v0.21 → v0.24 users get both legacy collapse AND wrap in one
            // launch.
            {
                let state: tauri::State<AppState> = app.state();
                let conn = state.db.lock();
                if let Err(e) = db::migrate_per_language_v4(&conn) {
                    eprintln!("migrate_per_language_v4: {e}");
                }
            }

            // v0.31 — grandfather existing installs past the new first-run
            // onboarding wizard. Marks `onboarding_completed=true` when the DB
            // has notes or a local Whisper model is present on disk, so nobody
            // already using Humla ever sees the wizard. Deliberately does NOT
            // read the Keychain (a startup Keychain read can trigger a macOS
            // prompt). Idempotent via the presence of the setting.
            {
                let state: tauri::State<AppState> = app.state();
                let conn = state.db.lock();
                if let Err(e) =
                    db::migrate_grandfather_onboarding(&conn, &app_dir.join("models"))
                {
                    eprintln!("migrate_grandfather_onboarding: {e}");
                }
            }

            // One-time retrieval backfill (issue #47): index any Notes that
            // predate the chunk substrate so agentic chat can search them.
            // Off the main thread — a large library shouldn't delay launch.
            // `async_runtime::spawn` (not bare tokio::spawn) — see the setup
            // main-thread caveat in CLAUDE.md.
            {
                let db = {
                    let state: tauri::State<AppState> = app.state();
                    state.db.clone()
                };
                let app_handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    // 1. Re-chunk (CPU-bound) off the async pool.
                    let db2 = db.clone();
                    let _ = tauri::async_runtime::spawn_blocking(move || {
                        commands::backfill_note_chunks(&db2);
                    })
                    .await;
                    // 2. Embed the (re)chunked notes so cross-note semantic
                    //    search works day-one (issue #48). Best-effort + cached.
                    commands::embed_backfill(app_handle).await;
                });
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::notes_list,
            commands::speaker_label_stats,
            commands::speaker_default_name,
            commands::notes_get,
            commands::notes_create,
            commands::notes_update,
            commands::notes_delete,
            commands::notes_move,
            commands::notes_set_workspace,
            commands::notes_list_trash,
            commands::notes_restore,
            commands::notes_purge,
            commands::notes_revisions,
            commands::notes_restore_revision,
            commands::folders_list,
            commands::folders_create,
            commands::folders_rename,
            commands::folders_delete,
            commands::clients_list,
            commands::clients_create,
            commands::clients_rename,
            commands::clients_delete,
            commands::notes_set_client,
            commands::settings_get,
            commands::settings_set,
            commands::cloud_status,
            commands::cloud_configure,
            commands::cloud_login,
            commands::cloud_signup,
            commands::cloud_resend_verification,
            commands::cloud_request_password_reset,
            commands::cloud_logout,
            commands::cloud_create_workspace,
            commands::cloud_select_workspace,
            commands::cloud_rename_workspace,
            commands::cloud_delete_workspace,
            commands::cloud_leave_workspace,
            commands::cloud_transfer_workspace,
            commands::cloud_workspace_members,
            commands::cloud_speaker_roster,
            commands::cloud_pending_note_ids,
            commands::cloud_upload_note_audio,
            commands::cloud_download_note_audio,
            commands::cloud_upload_note_sessions,
            commands::cloud_repair_note_sessions,
            commands::cloud_download_note_sessions,
            commands::cloud_note_recording_status,
            commands::cloud_add_member,
            commands::cloud_invite_member,
            commands::cloud_remove_member,
            commands::cloud_set_member_role,
            commands::cloud_billing_checkout,
            commands::cloud_billing_portal,
            commands::app_data_dir,
            commands::system_arch,
            commands::note_diagnostics_dir,
            commands::note_audio_dir,
            commands::note_audio_files,
            commands::stored_audio_stats,
            commands::delete_stored_audio,
            commands::note_diagnostics_files,
            commands::note_playback_path,
            commands::note_sessions,
            commands::note_session_playback_path,
            commands::note_timeline,
            commands::note_timeline_rename,
            commands::note_timeline_set_chunk_label,
            commands::note_timeline_set_chunk_text,
            commands::note_timeline_delete_chunk,
            commands::open_in_finder,
            commands::export_note,
            commands::rediarize_note,
            commands::summary_prompts_list,
            commands::summary_prompts_create,
            commands::summary_prompts_update,
            commands::summary_prompts_delete,
            commands::get_transcribe_config,
            commands::set_transcribe_config,
            commands::provider_key_get,
            commands::provider_key_set,
            commands::provider_key_test,
            commands::local_whisper_models,
            commands::local_whisper_download,
            commands::local_whisper_delete,
            commands::local_llm_list_models,
            commands::diarize_status,
            commands::diarize_download,
            commands::diarize_delete,
            commands::recording_start,
            commands::recording_stop,
            commands::import_audio,
            commands::recording_pause,
            commands::recording_resume,
            commands::recording_state,
            commands::summarize_note,
            commands::chat_send,
            commands::chat_cancel,
            commands::chat_history,
            commands::chat_list_conversations,
            commands::chat_new_conversation,
            commands::chat_delete_conversation,
            commands::chat_rename_conversation,
            commands::chat_set_breadth,
            commands::chat_set_owner_filter,
            commands::chat_get_breadth,
            commands::chat_get_owner_filter,
            commands::chat_usage,
            commands::chat_index_state,
            commands::chat_key_meta,
            commands::chat_key_set,
            commands::chat_key_set_from_keychain,
            commands::chat_key_delete,
            commands::chat_reindex_note,
            commands::chat_rebuild_index,
            commands::chat_stale_note_count,
            commands::permissions_status,
            commands::permissions_request,
            commands::permissions_open_settings,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri app")
        .run(|_app_handle, event| {
            // Bypass C++ static destructors on quit. whisper.cpp's GGML
            // Metal backend dispatches a background block during init that
            // polls via usleep; on app exit, the unique_ptr<ggml_metal_device>
            // destructor (called from atexit via __cxa_finalize_ranges)
            // runs ggml_metal_rsets_free, which aborts when those rsets are
            // still owned by the in-flight init block. Result: every
            // Cmd+Q after a recording reliably crashes inside ggml_abort
            // and produces a SIGABRT crash dialog. We don't actually need
            // C++ destructors to run at exit — the OS reclaims Metal
            // resources, file descriptors, and process memory anyway, and
            // our DB writes are synced before this point. _exit(0) skips
            // atexit entirely and lets the process terminate cleanly.
            if let tauri::RunEvent::Exit = event {
                unsafe { libc::_exit(0) };
            }
        });
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
