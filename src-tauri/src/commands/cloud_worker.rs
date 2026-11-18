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
// `Manager as _` brings the trait's methods (e.g. `AppHandle::path`) into scope
// without binding the name `Manager`, which is also this module's own struct.
use tauri::{AppHandle, Emitter, Manager as _};

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
    keychain_cache: crate::keychain::Cache,
    running: Mutex<Running>,
}

#[derive(Default)]
struct Running {
    /// `Some` while a worker is live; forwards no-op when `None`.
    handle: Option<Arc<cloud_sync::CloudSync>>,
    /// The spawned worker future, kept so a restart can abort it.
    task: Option<tauri::async_runtime::JoinHandle<()>>,
    /// Refreshes the organisation config on the worker's interval.
    org_task: Option<tauri::async_runtime::JoinHandle<()>>,
}

/// Build the observer and attempt an initial start (a no-op until the user has
/// configured + signed in + selected a workspace). Wired into `run()` as the
/// `run_with_sync` factory when the `cloud` feature is on.
pub fn install(ctx: SyncContext) -> Arc<dyn SyncObserver> {
    let mgr = Arc::new(Manager {
        db: ctx.db,
        app: ctx.app,
        keychain_cache: ctx.keychain_cache,
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
        running.abort();

        let app = self.app.clone();
        let app_status = self.app.clone();
        let app_conflict = self.app.clone();
        let app_purged = self.app.clone();
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
                    cloud_sync::SyncState::SignedOut => "signed_out",
                    cloud_sync::SyncState::TwoFactorRequired => "two_factor_required",
                };
                let _ = app_status.emit("sync_status", s);
            },
            move |title| {
                // A pull preserved local edits as a conflict copy → toast it so
                // the user knows their version was kept and the note also changed.
                let _ = app_conflict.emit("sync_conflict", title);
            },
            move |note_id| {
                // A pull purged a note (its author withdrew it, or it is past its
                // organisation's retention): its files go too, and the UI closes it.
                super::org::announce_purged(&app_purged, &[note_id.to_string()]);
            },
        ) {
            Ok((handle, fut)) => {
                // MUST be tauri::async_runtime::spawn — a bare tokio::spawn
                // panics when this runs from the setup closure on first start
                // (no current tokio runtime there).
                let task = tauri::async_runtime::spawn(fut);
                running.handle = Some(handle);
                running.task = Some(task);
                running.org_task = Some(tauri::async_runtime::spawn(self.org_refresh_loop()));
                eprintln!("cloud-sync: worker started");
            }
            Err(e) => eprintln!("cloud-sync: start failed: {e:#}"),
        }
    }

    /// Stop the worker if running. Aborting drops the worker future; the durable
    /// `sync_outbox` means any not-yet-pushed ops survive and replay on the next
    /// start.
    fn stop(&self) {
        self.running.lock().abort();
    }

    /// Refresh the organisation config and the pending invites while the worker
    /// runs, then send the queued audit reports, paced by `org::next_delay`: back off while the server can't be reached, stop on a
    /// server without the route until a refresh hears it answer. Without a
    /// session it skips the round, leaving the cache in force.
    fn org_refresh_loop(&self) -> impl std::future::Future<Output = ()> + Send + 'static {
        use super::org;
        let db = self.db.clone();
        let cache = self.keychain_cache.clone();
        let app = self.app.clone();
        async move {
            let mut delay = org::first_delay(org::since_last_attempt());
            loop {
                tokio::time::sleep(delay).await;
                let attempt = match super::cloud_auth::ensure_session_from(&db, &cache).await {
                    Ok((base, session)) => {
                        let attempt = org::refresh_org_config(&db, &app, &base, &session.token).await;
                        super::invites::refresh(&app, &base, &session).await;
                        Some(attempt)
                    }
                    Err(_) => None,
                };
                if attempt.is_some() {
                    crate::org_audit::flush(&db, &cache).await;
                }
                delay = match org::next_delay(delay, attempt) {
                    Some(next) => next,
                    None => {
                        org::route_answered().await;
                        org::REFRESH_INTERVAL
                    }
                };
            }
        }
    }

    /// Snapshot the live handle for a forward, or `None` if the worker is down.
    fn handle(&self) -> Option<Arc<cloud_sync::CloudSync>> {
        self.running.lock().handle.clone()
    }

    /// Assemble a `cloud_sync::Config` from persisted settings and the app's
    /// session, or `None` if any piece is missing/empty — signed out included.
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
        if !super::cloud_auth::has_credentials(&self.keychain_cache) {
            return None;
        }
        // Where session-metadata pushes read each note's sessions.json manifest.
        // A missing app-data dir is unexpected but non-fatal for the rest of
        // sync, so fall back to an empty path (session pushes then just no-op).
        let recordings_dir = self
            .app
            .path()
            .app_data_dir()
            .map(|d| d.join("recordings"))
            .unwrap_or_default();
        Some(cloud_sync::Config {
            base_url,
            session: Arc::new(AppSession {
                db: self.db.clone(),
                keychain_cache: self.keychain_cache.clone(),
            }),
            retention_cutoff: {
                let (db, workspace) = (self.db.clone(), workspace_id.clone());
                Arc::new(move || crate::org_policy::workspace_cutoff(&db.lock(), &workspace, crate::db::now_ms()))
            },
            note_busy: {
                let app = self.app.clone();
                Arc::new(move |note_id: &str| super::org::note_busy(&app, note_id))
            },
            purge_note: Arc::new(|conn: &rusqlite::Connection, id: &str| crate::db::purge_note_rows(conn, id)),
            workspace_id,
            poll_interval: POLL_INTERVAL,
            recordings_dir,
        })
    }
}

/// The app's one session (`super::cloud_auth`), lent to the worker.
struct AppSession {
    db: cloud_sync::Db,
    keychain_cache: crate::keychain::Cache,
}

impl Running {
    fn abort(&mut self) {
        for task in [self.task.take(), self.org_task.take()].into_iter().flatten() {
            task.abort();
        }
        self.handle = None;
    }
}

impl cloud_sync::Session for AppSession {
    fn token(&self) -> cloud_sync::TokenFuture<'_> {
        Box::pin(async move {
            match super::cloud_auth::ensure_session_from(&self.db, &self.keychain_cache).await {
                Ok((_, s)) => Ok(cloud_sync::Auth { token: s.token, user_id: s.user_id }),
                Err(super::cloud_auth::SessionError::SignedOut(_)) => {
                    Err(anyhow::Error::new(cloud_sync::SignedOut))
                }
                Err(super::cloud_auth::SessionError::Unreachable(e)) => Err(anyhow::anyhow!(e)),
            }
        })
    }

    fn rejected(&self, token: &str) {
        super::cloud_auth::forget_session_token(token);
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
    fn note_withdrawn(&self, id: &str) {
        if let Some(h) = self.mgr.handle() {
            h.enqueue_note_withdrawn(id);
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
    fn client_upserted(&self, id: &str) {
        if let Some(h) = self.mgr.handle() {
            h.enqueue_client_upsert(id);
        }
    }
    fn client_deleted(&self, id: &str) {
        if let Some(h) = self.mgr.handle() {
            h.enqueue_client_delete(id);
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
    fn session_upserted(&self, note_id: &str, session_id: &str) {
        if let Some(h) = self.mgr.handle() {
            h.enqueue_session_upsert(note_id, session_id);
        }
    }
    fn session_deleted(&self, note_id: &str, session_id: &str) {
        if let Some(h) = self.mgr.handle() {
            h.enqueue_session_delete(note_id, session_id);
        }
    }
    fn config_changed(&self) {
        self.mgr.restart();
    }
}
