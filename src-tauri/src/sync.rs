//! Sync seam.
//!
//! The note / folder / summary-prompt commands ping a [`SyncObserver`] after
//! each successful local write. The open-source build installs [`NoopSync`],
//! so sync is a no-op and the app never touches the network. A commercial
//! build can install an observer that enqueues the change for upload.
//!
//! This mirrors [`crate::stt::BatchSttAdapter`]: the command layer calls the
//! trait and doesn't care which implementation it got.

use std::sync::Arc;

/// Notified after a successful local write so a sync layer can enqueue the
/// change for upload.
///
/// **Contract.** Callbacks run on the command's thread, *after* the
/// `AppState::db` guard has been dropped. They must return quickly, never
/// block, and never re-lock `AppState::db` synchronously — `parking_lot` is
/// not reentrant, so doing so while a command still held the guard would
/// deadlock (dropping the guard first removes the hazard, but implementations
/// should not rely on it). The intended implementation only does a fast
/// enqueue (a channel send, or an `INSERT` on its own connection) and returns.
pub trait SyncObserver: Send + Sync {
    /// A note was created or modified. `id` is the note id.
    fn note_upserted(&self, _id: &str) {}

    /// A note was deleted. `id` is the now-removed note id. Local deletes are
    /// hard `DELETE`s, so an implementation must record a tombstone here or
    /// the deletion will never propagate to other devices.
    fn note_deleted(&self, _id: &str) {}

    /// A note moved between workspaces. `from`/`to` are workspace ids (`""` =
    /// Personal/local-only). The sync layer must tombstone the note in `from`
    /// and (re)create it in `to`, since the two live as separate remote records
    /// keyed on `(workspace, client_id)`.
    fn note_moved(&self, _id: &str, _from_workspace: &str, _to_workspace: &str) {}

    /// A folder was created or renamed. `id` is the folder id.
    fn folder_upserted(&self, _id: &str) {}

    /// A folder was deleted. `id` is the now-removed folder id.
    fn folder_deleted(&self, _id: &str) {}

    /// A custom summary prompt was created or updated. `id` is the prompt id.
    fn summary_prompt_upserted(&self, _id: &str) {}

    /// A custom summary prompt was deleted. `id` is the now-removed prompt id.
    fn summary_prompt_deleted(&self, _id: &str) {}

    /// Cloud configuration changed — the server URL, the stored credentials, or
    /// the selected workspace. A sync implementation should re-evaluate whether
    /// it can run (all of base URL + credentials + workspace present) and
    /// start, stop, or restart its worker to match.
    ///
    /// Called from the cloud commands after they mutate that state (configure,
    /// login, logout, create/select workspace), on the command thread with no
    /// `AppState::db` guard held — same fast, non-blocking contract as the
    /// other callbacks. No-op in the open-source build.
    fn config_changed(&self) {}
}

/// Open-source default: every callback is a no-op.
pub struct NoopSync;

impl SyncObserver for NoopSync {}

/// Handed to a sync implementation when it is constructed, after the database
/// connection and `AppHandle` exist. A real implementation keeps `db` to apply
/// pulled remote changes to the local store, and `app` to emit refresh events
/// to the frontend.
pub struct SyncContext {
    pub db: Arc<parking_lot::Mutex<rusqlite::Connection>>,
    pub app: tauri::AppHandle,
}
