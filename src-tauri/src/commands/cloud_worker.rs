//! Live cloud-sync worker glue (`cloud` feature only).
//!
//! This is the composition root that links the framework-agnostic `cloud-sync`
//! engine to the core. It does two things:
//!
//!  1. Implements [`crate::sync::SyncObserver`] by forwarding each per-id
//!     mutation ping to a `cloud_sync::CloudSync` handle (a non-blocking channel
//!     send), so local writes get queued for upload.
//!  2. Owns the worker's **lifecycle** — starting it only when the app is fully
//!     configured (server URL + credentials + a selected workspace all present)
//!     and *restarting* it whenever that changes (login, logout, workspace
//!     switch). That restart-on-change is the difference between "teams UI" and
//!     "notes actually replicate": the worker can't be a boot-only singleton
//!     because the user logs in *after* boot.
//!
//! Kept behind the `cloud` Cargo feature and isolated in its own module so the
//! OSS build drops it entirely (and so the cloud layer stays cleanly
//! separable). The settings/credential plumbing is reused from [`super::cloud`]
//! — this module only adds the `cloud_sync::Config` assembly + worker plumbing,
//! which is the sole place the core references the `cloud_sync` crate.

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tauri::{AppHandle, Emitter};

use crate::sync::{SyncContext, SyncObserver};

/// How often the worker pulls remote changes. Pushes are immediate (driven by
/// the observer); this only bounds pull latency.
const POLL_INTERVAL: Duration = Duration::from_secs(15);

/// The observer installed on `AppState`. Forwards mutations to the active sync
/// handle (if the worker is running) and (re)starts the worker on config
/// changes via [`SyncObserver::config_changed`].
pub struct CloudObserver {
    mgr: Arc<Manager>,
}

/// Owns the running worker + the inputs needed to (re)build it. The handle and
/// task are swapped under a mutex on every restart.
struct Manager {
    db: cloud_sync::Db,
    app: AppHandle,
    running: Mutex<Running>,
}

#[derive(Default)]
struct Running {
    /// `Some` while a worker is live; forwards no-op when `None`.
    handle: Option<Arc<cloud_sync::CloudSync>>,
    /// The spawned worker future, kept so a restart can abort it.
    task: Option<tauri::async_runtime::JoinHandle<()>>,
}

/// Build the observer and attempt an initial start (a no-op until the user has
/// configured + signed in + selected a workspace). Wired into `run()` as the
/// `run_with_sync` factory when the `cloud` feature is on.
pub fn install(ctx: SyncContext) -> Arc<dyn SyncObserver> {
    let mgr = Arc::new(Manager {
        db: ctx.db,
        app: ctx.app,
        running: Mutex::new(Running::default()),
    });
    mgr.restart();
    Arc::new(CloudObserver { mgr })
}

impl Manager {
    /// (Re)start the worker to match the current config, or stop it if the
    /// config is incomplete. Synchronous and fast: it builds the `Config`,
    /// spawns the worker future on the Tauri runtime, and swaps the handle. It
    /// never awaits the worker.
    fn restart(&self) {
        let Some(cfg) = self.read_config() else {
            // Not fully configured (signed out, no workspace, …) → ensure no
            // worker is running and leave forwards as no-ops.
            self.stop();
            return;
        };

        let mut running = self.running.lock();
        // Abort any prior worker first so we never run two against the same DB
        // (which would race the shared outbox).
        if let Some(task) = running.task.take() {
            task.abort();
        }
        running.handle = None;

        let app = self.app.clone();
        let app_status = self.app.clone();
        let app_conflict = self.app.clone();
        match cloud_sync::start(
            self.db.clone(),
            cfg,
            move || {
                // Fired after a pull applied remote changes; the frontend
                // listens for `notes_changed` and refetches.
                let _ = app.emit("notes_changed", ());
            },
            move |state| {
                // Surface coarse sync state to the UI as a `sync_status` event.
                let s = match state {
                    cloud_sync::SyncState::Syncing => "syncing",
                    cloud_sync::SyncState::Idle => "idle",
                    cloud_sync::SyncState::Error => "error",
                };
                let _ = app_status.emit("sync_status", s);
            },
            move |title| {
                // A pull preserved local edits as a conflict copy → toast it so
                // the user knows their version was kept and the note also changed.
                let _ = app_conflict.emit("sync_conflict", title);
            },
        ) {
            Ok((handle, fut)) => {
                // MUST be tauri::async_runtime::spawn — a bare tokio::spawn
                // panics when this runs from the setup closure on first start
                // (no current tokio runtime there).
                let task = tauri::async_runtime::spawn(fut);
                running.handle = Some(handle);
                running.task = Some(task);
                eprintln!("cloud-sync: worker started");
            }
            Err(e) => eprintln!("cloud-sync: start failed: {e:#}"),
        }
    }

    /// Stop the worker if running. Aborting drops the worker future; the durable
    /// `sync_outbox` means any not-yet-pushed ops survive and replay on the next
    /// start.
    fn stop(&self) {
        let mut running = self.running.lock();
        if let Some(task) = running.task.take() {
            task.abort();
        }
        running.handle = None;
    }

    /// Snapshot the live handle for a forward, or `None` if the worker is down.
    fn handle(&self) -> Option<Arc<cloud_sync::CloudSync>> {
        self.running.lock().handle.clone()
    }

    /// Assemble a `cloud_sync::Config` from persisted settings + Keychain creds,
    /// or `None` if any piece is missing/empty. Reuses [`super::cloud`]'s keys
    /// and credential reader so there's one source of truth.
    fn read_config(&self) -> Option<cloud_sync::Config> {
        let (base_url, workspace_id) = {
            let conn = self.db.lock();
            let base = crate::db::get_setting(&conn, super::cloud::SETTING_BASE_URL).ok().flatten()?;
            let ws = crate::db::get_setting(&conn, super::cloud::SETTING_WORKSPACE).ok().flatten()?;
            (base.trim().to_string(), ws.trim().to_string())
        };
        if base_url.is_empty() || workspace_id.is_empty() {
            return None;
        }
        let (email, password) = super::cloud::read_creds()?;
        Some(cloud_sync::Config {
            base_url,
            email,
            password,
            workspace_id,
            poll_interval: POLL_INTERVAL,
        })
    }
}

impl SyncObserver for CloudObserver {
    fn note_upserted(&self, id: &str) {
        if let Some(h) = self.mgr.handle() {
            h.enqueue_note_upsert(id);
        }
    }
    fn note_deleted(&self, id: &str) {
        if let Some(h) = self.mgr.handle() {
            h.enqueue_note_delete(id);
        }
    }
    fn note_moved(&self, id: &str, from: &str, to: &str) {
        if let Some(h) = self.mgr.handle() {
            h.enqueue_note_move(id, from, to);
        }
    }
    fn folder_upserted(&self, id: &str) {
        if let Some(h) = self.mgr.handle() {
            h.enqueue_folder_upsert(id);
        }
    }
    fn folder_deleted(&self, id: &str) {
        if let Some(h) = self.mgr.handle() {
            h.enqueue_folder_delete(id);
        }
    }
    fn summary_prompt_upserted(&self, id: &str) {
        if let Some(h) = self.mgr.handle() {
            h.enqueue_prompt_upsert(id);
        }
    }
    fn summary_prompt_deleted(&self, id: &str) {
        if let Some(h) = self.mgr.handle() {
            h.enqueue_prompt_delete(id);
        }
    }
    fn config_changed(&self) {
        self.mgr.restart();
    }
}
