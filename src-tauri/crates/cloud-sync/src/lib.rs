//! cloud-sync — PocketBase-backed sync engine for Humla.
//!
//! Framework-agnostic on purpose: it knows the local SQLite schema and talks
//! HTTP to PocketBase, but it does NOT depend on `humla_lib` or `tauri`. The
//! app's `commands::cloud_worker` glue module is the thin composition root: it
//! implements `humla_lib::sync::SyncObserver` by forwarding to the `enqueue_*`
//! methods here, owns the worker lifecycle (start/restart on login + workspace
//! switch), and passes a `notify` closure that emits the Tauri refresh event.
//!
//! Syncs three entities — notes, folders, summary_prompts — all keyed on
//! `client_id` (the local SQLite UUID; PocketBase's own 15-char ids can't hold
//! a UUID). `client_updated_at` is the last-write-wins key; PocketBase's
//! `updated` autodate is the pull cursor; `deleted` is a tombstone.
//!
//! Design:
//!  - The observer enqueues per-id ops onto a channel (never blocks the command
//!    thread).
//!  - The worker persists each op to a `sync_outbox` table, then drains it:
//!    snapshot → PUSH → delete the row on success (failures stay for retry).
//!  - On an interval it PULLs every entity `updated` since its cursor and
//!    applies it locally via raw SQL (NOT through the app's commands → no
//!    observer echo loop), then fires `notify` so the UI refetches.
//!
//! Concurrency contract: the shared `rusqlite::Connection` is behind a
//! `parking_lot::Mutex`. We NEVER hold that guard across an `.await` — every DB
//! access is a scoped lock/snapshot/unlock before any network call. That keeps
//! the worker future `Send`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;

/// Shared handle to the app's SQLite connection. Same concrete type as the
/// public core because rusqlite is pinned to the same version.
pub type Db = Arc<Mutex<Connection>>;


#[derive(Clone)]
pub struct Config {
    /// PocketBase base URL, e.g. `https://sync.humla.app`.
    pub base_url: String,
    /// Auth identity (email) + password for the syncing user.
    pub email: String,
    pub password: String,
    /// PocketBase `workspaces` record id this device syncs into.
    pub workspace_id: String,
    /// How often to pull remote changes.
    pub poll_interval: Duration,
    /// `<app_data>/recordings`. Session-metadata pushes read each note's
    /// `sessions.json` manifest from `<recordings_dir>/<note_id>/`. (Binary
    /// session assets are uploaded/downloaded separately by the app's cloud
    /// commands — the crate only syncs the metadata records.)
    pub recordings_dir: PathBuf,
}

/// Cheap handle the observer glue calls. Sends ops to the worker; never blocks.
pub struct CloudSync {
    tx: mpsc::UnboundedSender<Op>,
}

impl CloudSync {
    pub fn enqueue_note_upsert(&self, id: &str) {
        let _ = self.tx.send(Op::Note { id: id.to_string(), delete: false });
    }
    pub fn enqueue_note_delete(&self, id: &str) {
        let _ = self.tx.send(Op::Note { id: id.to_string(), delete: true });
    }
    pub fn enqueue_folder_upsert(&self, id: &str) {
        let _ = self.tx.send(Op::Folder { id: id.to_string(), delete: false });
    }
    pub fn enqueue_folder_delete(&self, id: &str) {
        let _ = self.tx.send(Op::Folder { id: id.to_string(), delete: true });
    }
    pub fn enqueue_prompt_upsert(&self, id: &str) {
        let _ = self.tx.send(Op::Prompt { id: id.to_string(), delete: false });
    }
    pub fn enqueue_prompt_delete(&self, id: &str) {
        let _ = self.tx.send(Op::Prompt { id: id.to_string(), delete: true });
    }
    /// A note moved between workspaces (either end may be `""` = Personal).
    /// Tombstones it in `from` and (re)creates it in `to`.
    pub fn enqueue_note_move(&self, id: &str, from: &str, to: &str) {
        let _ = self.tx.send(Op::NoteMove {
            id: id.to_string(),
            from: from.to_string(),
            to: to.to_string(),
        });
    }
    /// A recording session's metadata was created or changed (#16). Pushes the
    /// `note_sessions` record; the parent note must be synced first (it is —
    /// the note upsert is enqueued ahead of this and drains in seq order).
    pub fn enqueue_session_upsert(&self, note_id: &str, session_id: &str) {
        let _ = self.tx.send(Op::Session {
            note_id: note_id.to_string(),
            session_id: session_id.to_string(),
            delete: false,
        });
    }
    /// A recording session was deleted → tombstone its `note_sessions` record.
    pub fn enqueue_session_delete(&self, note_id: &str, session_id: &str) {
        let _ = self.tx.send(Op::Session {
            note_id: note_id.to_string(),
            session_id: session_id.to_string(),
            delete: true,
        });
    }
}

enum Op {
    Note { id: String, delete: bool },
    Folder { id: String, delete: bool },
    Prompt { id: String, delete: bool },
    NoteMove { id: String, from: String, to: String },
    Session { note_id: String, session_id: String, delete: bool },
}

/// Build the sync engine. Returns the handle plus the worker future; the
/// caller spawns the future on its runtime. In the Tauri app that means
/// `tauri::async_runtime::spawn(fut)` — NOT `tokio::spawn`, which panics when
/// called from Tauri's setup closure (no current runtime there).
/// Coarse, UI-facing sync state, reported via the `status` callback passed to
/// [`start`]. The app maps these to an indicator (spinner / synced / warning).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncState {
    /// A push/pull cycle is in flight.
    Syncing,
    /// The last cycle completed successfully.
    Idle,
    /// The last cycle failed (will retry).
    Error,
}

pub fn start<N, S, C>(
    db: Db,
    config: Config,
    notify: N,
    status: S,
    conflict: C,
) -> Result<(Arc<CloudSync>, impl std::future::Future<Output = ()>)>
where
    N: Fn() + Send + Sync + 'static,
    S: Fn(SyncState) + Send + Sync + 'static,
    C: Fn(&str) + Send + Sync + 'static,
{
    init_tables(&db)?;
    let (tx, rx) = mpsc::unbounded_channel();
    let worker = Worker {
        db,
        config,
        http: reqwest::Client::new(),
        notify: Arc::new(notify),
        status: Arc::new(status),
        conflict: Arc::new(conflict),
        auth: Mutex::new(None),
    };
    Ok((Arc::new(CloudSync { tx }), worker.run(rx)))
}

#[derive(Clone)]
struct Auth {
    token: String,
    user_id: String,
}

struct Worker {
    db: Db,
    config: Config,
    http: reqwest::Client,
    notify: Arc<dyn Fn() + Send + Sync>,
    status: Arc<dyn Fn(SyncState) + Send + Sync>,
    /// Fired (with the note title) when a pull preserved local edits as a
    /// conflict copy instead of silently overwriting them.
    conflict: Arc<dyn Fn(&str) + Send + Sync>,
    auth: Mutex<Option<Auth>>,
}

impl Worker {
    async fn run(self, mut rx: mpsc::UnboundedReceiver<Op>) {
        (self.status)(SyncState::Syncing);
        let ok = self
            .pull()
            .await
            .inspect_err(|e| eprintln!("cloud-sync: initial pull failed: {e:#}"))
            .is_ok();
        (self.status)(if ok { SyncState::Idle } else { SyncState::Error });

        let mut tick = tokio::time::interval(self.config.poll_interval);
        tick.tick().await; // the first tick fires immediately; consume it

        // Realtime: a best-effort SSE listener that nudges us to pull the instant
        // a record changes server-side, instead of waiting for the next poll. It
        // only TRIGGERS pulls (never applies data), and the interval poll is the
        // reliable backstop — so a dropped or failing realtime connection simply
        // degrades to polling, never to data loss. Owned by this future, so it's
        // torn down with the worker on restart (no leaked connection).
        let pull_now = Arc::new(tokio::sync::Notify::new());
        let mut realtime =
            Box::pin(realtime_loop(self.config.clone(), self.http.clone(), pull_now.clone()));

        loop {
            tokio::select! {
                op = rx.recv() => {
                    match op {
                        Some(op) => {
                            (self.status)(SyncState::Syncing);
                            let mut ok = true;
                            if let Err(e) = self.enqueue(op) {
                                eprintln!("cloud-sync: enqueue failed: {e:#}");
                                ok = false;
                            }
                            if let Err(e) = self.drain_outbox().await {
                                eprintln!("cloud-sync: push failed (will retry): {e:#}");
                                ok = false;
                            }
                            (self.status)(if ok { SyncState::Idle } else { SyncState::Error });
                        }
                        None => break, // every sender dropped → shut down
                    }
                }
                _ = tick.tick() => self.sync_cycle().await,
                _ = pull_now.notified() => self.sync_cycle().await,
                // realtime_loop never returns; this branch just keeps it polled.
                _ = &mut realtime => {}
            }
        }
    }

    /// One push-drain + pull cycle with status reporting. Shared by the interval
    /// tick and the realtime nudge.
    async fn sync_cycle(&self) {
        (self.status)(SyncState::Syncing);
        let mut ok = true;
        if let Err(e) = self.drain_outbox().await {
            eprintln!("cloud-sync: push failed (will retry): {e:#}");
            ok = false;
        }
        if let Err(e) = self.pull().await {
            eprintln!("cloud-sync: pull failed (will retry): {e:#}");
            ok = false;
        }
        (self.status)(if ok { SyncState::Idle } else { SyncState::Error });
    }

    /// Persist an op to the durable outbox so a crash between enqueue and push
    /// doesn't lose the change.
    fn enqueue(&self, op: Op) -> Result<()> {
        match op {
            Op::Note { id, delete } => self.enqueue_simple("note", "notes", &id, delete),
            Op::Folder { id, delete } => self.enqueue_simple("folder", "folders", &id, delete),
            Op::Prompt { id, delete } => self.enqueue_simple("prompt", "summary_prompts", &id, delete),
            Op::NoteMove { id, from, to } => {
                // A move = tombstone in the old workspace + (re)create in the new
                // one. The two rows differ only by `workspace`, so they don't
                // coalesce; the lower seq (delete) drains before the upsert.
                // Personal ('') endpoints skip the network: nothing to tombstone
                // when leaving Personal, nothing to push when entering it.
                if !from.is_empty() {
                    self.enqueue_row("note", &id, "delete", &from)?;
                }
                if !to.is_empty() {
                    self.enqueue_row("note", &id, "upsert", &to)?;
                }
                Ok(())
            }
            Op::Session { note_id, session_id, delete } => {
                // A session row keys on the parent note's workspace. On delete
                // the note may already be gone locally, so fall back to the
                // active workspace (reads/tombstones are workspace-scoped),
                // mirroring `enqueue_simple`. The entity_id packs both ids so
                // the push can read the manifest AND resolve the parent note.
                let workspace = if delete {
                    self.config.workspace_id.clone()
                } else {
                    let conn = self.db.lock();
                    conn.query_row(
                        "SELECT workspace_id FROM notes WHERE id = ?1",
                        rusqlite::params![note_id],
                        |r| r.get::<_, String>(0),
                    )
                    .optional()?
                    .unwrap_or_default()
                };
                let entity_id = session_entity_id(&note_id, &session_id);
                self.enqueue_row("session", &entity_id, if delete { "delete" } else { "upsert" }, &workspace)
            }
        }
    }

    /// Enqueue an upsert/delete for a single row, capturing its target workspace.
    /// Upsert reads the row's own `workspace_id` so it pushes to the right tenant
    /// even if the active workspace changes before it drains; delete uses the
    /// active workspace (the row is gone, and reads are workspace-scoped). Empty
    /// workspace = Personal → dropped without a network call when it drains.
    fn enqueue_simple(&self, entity: &str, table: &str, id: &str, delete: bool) -> Result<()> {
        let workspace = if delete {
            self.config.workspace_id.clone()
        } else {
            let conn = self.db.lock();
            conn.query_row(
                &format!("SELECT workspace_id FROM {table} WHERE id = ?1"),
                rusqlite::params![id],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .unwrap_or_default()
        };
        self.enqueue_row(entity, id, if delete { "delete" } else { "upsert" }, &workspace)
    }

    /// Persist one outbox row, coalescing on (entity, entity_id, workspace): a
    /// fresh op for the same row+workspace supersedes any pending one and resets
    /// the retry counter. Distinct workspaces (a move) coexist as two rows.
    fn enqueue_row(&self, entity: &str, entity_id: &str, kind: &str, workspace: &str) -> Result<()> {
        let conn = self.db.lock();
        conn.execute(
            "INSERT INTO sync_outbox (entity, entity_id, op, workspace, attempts, enqueued_at)
             VALUES (?1, ?2, ?3, ?4, 0, ?5)
             ON CONFLICT(entity, entity_id, workspace) DO UPDATE SET
                op = excluded.op, attempts = 0, enqueued_at = excluded.enqueued_at",
            rusqlite::params![entity, entity_id, kind, workspace, now_ms()],
        )?;
        Ok(())
    }

    /// Push pending outbox rows oldest-first. On an ordinary transient failure it
    /// stops at the first failing row so ordering is preserved and the next tick
    /// retries. But a row that has failed `MAX_TRANSIENT_ATTEMPTS` times is
    /// stepped over (kept queued, retried later) so one persistently-stuck row
    /// can't head-of-line-block every later change forever.
    async fn drain_outbox(&self) -> Result<()> {
        // Highest seq we've stepped over THIS pass (persistently-failing but
        // transient). Starts at -1 each call, so a skipped row is always retried
        // from the top on the next tick.
        let mut skip_after: i64 = -1;
        loop {
            let next: Option<(i64, String, String, String, String, i64)> = {
                let conn = self.db.lock();
                conn.query_row(
                    "SELECT seq, entity, entity_id, op, workspace, attempts FROM sync_outbox WHERE seq > ?1 ORDER BY seq LIMIT 1",
                    rusqlite::params![skip_after],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
                )
                .optional()?
            };
            let Some((seq, entity, entity_id, op, workspace, attempts)) = next else {
                return Ok(());
            };

            // Personal / local-only rows ('' workspace) never sync — drop them
            // without a network call.
            if workspace.is_empty() {
                self.db.lock().execute("DELETE FROM sync_outbox WHERE seq = ?1", rusqlite::params![seq])?;
                continue;
            }

            // Push to the row's OWN workspace.
            let result = match (entity.as_str(), op.as_str()) {
                ("note", "upsert") => self.push_note(&entity_id, &workspace).await,
                ("note", "delete") => self.push_delete("notes", &entity_id, &workspace).await,
                ("folder", "upsert") => self.push_folder(&entity_id, &workspace).await,
                ("folder", "delete") => self.push_delete("folders", &entity_id, &workspace).await,
                ("prompt", "upsert") => self.push_prompt(&entity_id, &workspace).await,
                ("prompt", "delete") => self.push_delete("summary_prompts", &entity_id, &workspace).await,
                ("session", "upsert") => {
                    let (note_id, session_id) = split_session_entity_id(&entity_id);
                    self.push_session(&note_id, &session_id, &workspace).await
                }
                ("session", "delete") => {
                    let (_, session_id) = split_session_entity_id(&entity_id);
                    // Session UUIDs are globally unique, so (client_id, workspace)
                    // locates the record without needing the parent note.
                    self.push_delete("note_sessions", &session_id, &workspace).await
                }
                _ => {
                    eprintln!("cloud-sync: unknown outbox row {entity}/{op}; dropping");
                    Ok(())
                }
            };

            match result {
                Ok(()) => {
                    self.db.lock().execute("DELETE FROM sync_outbox WHERE seq = ?1", rusqlite::params![seq])?;
                }
                Err(e) if is_permanent_push_error(&e) => {
                    // Genuinely un-pushable for this payload (a 4xx allow-listed in
                    // `is_permanent_status`: bad-request / forbidden / not-found /
                    // validation). Retrying can't help, so drop it. The classifier
                    // is a strict allow-list keyed on the numeric status, so a
                    // transient error is never misread as permanent — this can't
                    // silently lose a change that would have eventually synced.
                    eprintln!("cloud-sync: dropping un-pushable {entity}/{op} {entity_id}: {e:#}");
                    self.db.lock().execute("DELETE FROM sync_outbox WHERE seq = ?1", rusqlite::params![seq])?;
                    continue;
                }
                Err(e) => {
                    // Transient failure (network / 5xx / 401 / 408 / 413 / 429 …):
                    // never dropped. Record the attempt; once a single row has
                    // failed too many times, step over it (kept queued, retried
                    // next tick) so it can't block newer changes indefinitely.
                    // Otherwise stop draining here to preserve ordering.
                    let n = attempts + 1;
                    self.db.lock().execute(
                        "UPDATE sync_outbox SET attempts = ?2 WHERE seq = ?1",
                        rusqlite::params![seq, n],
                    )?;
                    if n >= MAX_TRANSIENT_ATTEMPTS {
                        eprintln!(
                            "cloud-sync: stepping over persistently-failing {entity}/{op} {entity_id} after {n} attempts (kept for retry): {e:#}"
                        );
                        skip_after = seq;
                        continue;
                    }
                    return Err(e);
                }
            }
        }
    }

    async fn push_note(&self, uuid: &str, workspace: &str) -> Result<()> {
        let Some(note) = self.snapshot_note(uuid)? else {
            return Ok(()); // deleted locally before we got here; nothing to push
        };
        // If the note has since moved to a different workspace, don't push its
        // current content into the stale (outbox-captured) workspace — that would
        // briefly leak content to the old workspace's members. The move enqueued
        // a tombstone for the old workspace + an upsert for the new one, which
        // place the note correctly.
        {
            let conn = self.db.lock();
            let current_ws: Option<String> = conn
                .query_row(
                    "SELECT workspace_id FROM notes WHERE id = ?1",
                    rusqlite::params![uuid],
                    |r| r.get(0),
                )
                .optional()?;
            if current_ws.as_deref() != Some(workspace) {
                return Ok(());
            }
        }
        let auth = self.ensure_auth().await?;
        // Preserve the creator across edits: this device only becomes the owner
        // when it's creating the note (no owner yet). Editing a teammate's
        // pulled note (owner already set) must NOT reassign ownership to the
        // editor.
        let owner = if note.owner.is_empty() {
            auth.user_id.clone()
        } else {
            note.owner.clone()
        };
        // Look up any existing remote record for this (client_id, workspace).
        let remote = self.find_remote_id("notes", uuid, workspace, &auth.token).await?;
        // LWW timestamp for this push. Normally the note's own `updated_at`, so a
        // pure workspace move — which deliberately does NOT bump `updated_at`
        // (`db::set_note_workspace`) — doesn't make the note read as
        // freshly-modified on any device. The exception: if the target workspace
        // still holds a *newer* tombstone for this note (it was moved out of here
        // earlier), the un-bumped value would lose the server's last-write-wins
        // and the move would silently re-delete the note — so step one past the
        // tombstone to win and resurrect it.
        let client_updated_at = match &remote {
            Some((_, true, remote_cua)) if note.updated_at <= *remote_cua => *remote_cua + 1,
            _ => note.updated_at,
        };
        let body = json!({
            "client_id": uuid,
            "workspace": workspace,
            "owner": owner,
            "title": note.title,
            "body": note.body,
            "transcript": note.transcript,
            "summary": note.summary,
            "language": note.language,
            "summary_preset": note.summary_preset,
            "folder_client_id": note.folder_id.unwrap_or_default(),
            "expected_speakers": note.expected_speakers.unwrap_or(0),
            "created_at": note.created_at,
            "client_updated_at": client_updated_at,
            "deleted": false,
        });
        let resp = match &remote {
            Some((pb_id, _, _)) => {
                self.http
                    .patch(format!("{}/api/collections/notes/records/{}", self.config.base_url, pb_id))
                    .bearer_auth(&auth.token)
                    .json(&body)
                    .send()
                    .await?
            }
            None => {
                self.http
                    .post(format!("{}/api/collections/notes/records", self.config.base_url))
                    .bearer_auth(&auth.token)
                    .json(&body)
                    .send()
                    .await?
            }
        };
        self.pb_ok(resp).await.map(|_| ())
    }

    async fn push_folder(&self, uuid: &str, workspace: &str) -> Result<()> {
        let Some(f) = self.snapshot_folder(uuid)? else {
            return Ok(());
        };
        let auth = self.ensure_auth().await?;
        let body = json!({
            "client_id": uuid,
            "workspace": workspace,
            "name": f.name,
            "client_updated_at": f.updated_at,
            "deleted": false,
        });
        self.upsert_record("folders", uuid, workspace, &auth.token, body).await
    }

    async fn push_prompt(&self, uuid: &str, workspace: &str) -> Result<()> {
        let Some(p) = self.snapshot_prompt(uuid)? else {
            return Ok(());
        };
        let auth = self.ensure_auth().await?;
        let body = json!({
            "client_id": uuid,
            "workspace": workspace,
            "name": p.name,
            "content": p.content,
            "client_updated_at": p.updated_at,
            "deleted": false,
        });
        self.upsert_record("summary_prompts", uuid, workspace, &auth.token, body).await
    }

    /// Push a recording session's METADATA record (#16). The binary assets
    /// (playback / timeline / mic / sys / chunks) are uploaded separately by
    /// the app's cloud commands via multipart PATCH — this only creates/updates
    /// the `note_sessions` row the assets attach to.
    ///
    /// Metadata is effectively immutable after a take finalises (index /
    /// started_at / duration / streams never change), so the LWW key is derived
    /// from the session's own start time: re-pushing is idempotent, and two
    /// devices can't create the same session UUID, so there's no real conflict.
    async fn push_session(&self, note_id: &str, session_id: &str, workspace: &str) -> Result<()> {
        let Some(meta) = self.read_session_meta(note_id, session_id) else {
            return Ok(()); // manifest entry gone (deleted before push) — nothing to do
        };
        let auth = self.ensure_auth().await?;
        // The parent note must exist on the server first — the note upsert is
        // enqueued ahead of this and drains in seq order. If it isn't there yet
        // (out-of-order pull, or the note push is still failing), treat as
        // transient so this retries once the note lands, rather than dropping it.
        let Some((note_pb_id, _, _)) =
            self.find_remote_id("notes", note_id, workspace, &auth.token).await?
        else {
            // The parent note isn't on the server. Distinguish two cases so the
            // session can't orphan-loop forever:
            //  - the note's own upsert is still queued in the outbox → it simply
            //    hasn't drained yet (drains ahead of us in seq order, or is being
            //    retried). TRANSIENT: retry once it lands.
            //  - no queued note upsert remains → the note push was permanently
            //    dropped (an allow-listed 4xx) or the note is gone, so this
            //    session can NEVER resolve its `note` relation. PERMANENT: drop
            //    it, else it re-auths + GETs /notes every tick and pins the
            //    note's "syncing…" indicator on for good.
            return Err(self.orphan_session_error(note_id, session_id, workspace));
        };
        let started_ms = started_at_to_ms(&meta.started_at);
        let client_updated_at = started_ms.max(1);
        let body = json!({
            "client_id": session_id,
            "note": note_pb_id,
            "workspace": workspace,
            "session_index": meta.index,
            "started_at": started_ms,
            "duration_ms": meta.duration_ms,
            "streams": meta.streams,
            "client_updated_at": client_updated_at,
            "deleted": false,
        });
        // (client_id, workspace) uniquely finds the record because session UUIDs
        // are globally unique; on create the POST carries the `note` relation.
        self.upsert_record("note_sessions", session_id, workspace, &auth.token, body).await
    }

    /// Read one session's manifest entry from `<recordings_dir>/<note>/sessions.json`.
    /// `None` when the manifest or entry is absent/unparseable.
    fn read_session_meta(&self, note_id: &str, session_id: &str) -> Option<ManifestEntry> {
        let path = self.config.recordings_dir.join(note_id).join("sessions.json");
        let body = std::fs::read_to_string(&path).ok()?;
        let manifest: ManifestFile = serde_json::from_str(&body).ok()?;
        manifest.sessions.into_iter().find(|e| e.id == session_id)
    }

    /// True while the parent note still has a queued upsert in the outbox for
    /// this workspace — i.e. it just hasn't drained yet, versus having been
    /// dropped/quarantined. On a DB error, err on the side of `true` (transient)
    /// so a read hiccup never turns into a dropped session.
    fn note_push_pending(&self, note_id: &str, workspace: &str) -> bool {
        let conn = self.db.lock();
        conn.query_row(
            "SELECT 1 FROM sync_outbox WHERE entity='note' AND entity_id=?1 AND op='upsert' AND workspace=?2 LIMIT 1",
            rusqlite::params![note_id, workspace],
            |_| Ok(()),
        )
        .optional()
        .map(|row| row.is_some())
        .unwrap_or(true)
    }

    /// Decide the retry policy for a session push whose parent note was NOT found
    /// on the server. Transient (defer, keep retrying) while the note's own
    /// upsert is still queued; permanent (drop) once that row is gone so the
    /// orphaned session can't loop forever. See the call site in `push_session`.
    fn orphan_session_error(&self, note_id: &str, session_id: &str, workspace: &str) -> anyhow::Error {
        if self.note_push_pending(note_id, workspace) {
            anyhow!("cloud-sync: parent note {note_id} not on server yet; deferring session push")
        } else {
            PermanentPushError(format!(
                "cloud-sync: parent note {note_id} has no queued push and isn't on the server; \
                 session {session_id} can never resolve its note relation — dropping"
            ))
            .into()
        }
    }

    /// Soft-delete (tombstone) a record by `client_id` so other devices pull
    /// the deletion instead of the row reappearing on their next pull.
    async fn push_delete(&self, collection: &str, uuid: &str, workspace: &str) -> Result<()> {
        let auth = self.ensure_auth().await?;
        let Some((pb_id, _, _)) = self.find_remote_id(collection, uuid, workspace, &auth.token).await? else {
            return Ok(()); // never synced; nothing to tombstone
        };
        let resp = self
            .http
            .patch(format!("{}/api/collections/{}/records/{}", self.config.base_url, collection, pb_id))
            .bearer_auth(&auth.token)
            .json(&json!({ "deleted": true, "client_updated_at": now_ms() }))
            .send()
            .await?;
        self.pb_ok(resp).await.map(|_| ())
    }

    /// Create the record, or PATCH it if one with this `client_id` already
    /// exists in the given workspace.
    async fn upsert_record(
        &self,
        collection: &str,
        client_id: &str,
        workspace: &str,
        token: &str,
        body: serde_json::Value,
    ) -> Result<()> {
        let resp = match self.find_remote_id(collection, client_id, workspace, token).await? {
            Some((pb_id, _, _)) => {
                self.http
                    .patch(format!(
                        "{}/api/collections/{}/records/{}",
                        self.config.base_url, collection, pb_id
                    ))
                    .bearer_auth(token)
                    .json(&body)
                    .send()
                    .await?
            }
            None => {
                self.http
                    .post(format!("{}/api/collections/{}/records", self.config.base_url, collection))
                    .bearer_auth(token)
                    .json(&body)
                    .send()
                    .await?
            }
        };
        self.pb_ok(resp).await.map(|_| ())
    }

    /// Locate the remote record for `(client_id, workspace)`, returning its PB
    /// id plus its current tombstone state and LWW timestamp — callers need the
    /// latter two to decide whether a push must step past an existing tombstone
    /// (see `push_note`'s resurrect logic).
    async fn find_remote_id(
        &self,
        collection: &str,
        client_id: &str,
        workspace: &str,
        token: &str,
    ) -> Result<Option<(String, bool, i64)>> {
        let filter = format!("client_id='{}' && workspace='{}'", client_id, workspace);
        let resp = self
            .http
            .get(format!("{}/api/collections/{}/records", self.config.base_url, collection))
            .bearer_auth(token)
            .query(&[
                ("filter", filter.as_str()),
                ("perPage", "1"),
                ("fields", "id,deleted,client_updated_at"),
            ])
            .send()
            .await?;
        let resp = self.pb_ok(resp).await?;

        #[derive(Deserialize)]
        struct ListResp {
            items: Vec<Item>,
        }
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Item {
            id: String,
            deleted: bool,
            client_updated_at: i64,
        }
        let list: ListResp = resp.json().await?;
        Ok(list.items.into_iter().next().map(|i| (i.id, i.deleted, i.client_updated_at)))
    }

    /// Pull every entity changed since its cursor and apply locally.
    async fn pull(&self) -> Result<()> {
        let auth = self.ensure_auth().await?;
        let mut changed = false;
        changed |= self
            .pull_collection("notes", "notes_cursor", &auth.token, |v| self.apply_remote_note_json(v))
            .await?;
        changed |= self
            .pull_collection("folders", "folders_cursor", &auth.token, |v| self.apply_remote_folder_json(v))
            .await?;
        changed |= self
            .pull_collection("summary_prompts", "prompts_cursor", &auth.token, |v| {
                self.apply_remote_prompt_json(v)
            })
            .await?;
        if changed {
            (self.notify)();
        }
        Ok(())
    }

    /// Paginate `collection` for records `updated` since the stored cursor,
    /// applying each via `apply`. Returns whether anything changed (so the
    /// caller can fire a single `notify`). Generic over entity — `apply`
    /// carries the per-entity deserialize + local write.
    async fn pull_collection<F>(
        &self,
        collection: &str,
        cursor_key: &str,
        token: &str,
        mut apply: F,
    ) -> Result<bool>
    where
        F: FnMut(&serde_json::Value) -> Result<()>,
    {
        // Cursor is per-workspace, so switching workspaces never skips or
        // re-pulls the wrong tenant's records under a shared cursor. It's a
        // composite "<updated>|<id>": a strict `(updated, id)` watermark. This
        // advances past every applied record WITHOUT re-fetching the boundary
        // tick each poll (the old `updated >=` did), while the `id` tiebreak
        // means same-millisecond siblings are never skipped.
        let cursor_key = format!("{}:{}", cursor_key, self.config.workspace_id);
        let raw = self.read_state(&cursor_key)?.unwrap_or_default();
        let (mut cur_ts, mut cur_id) = match raw.split_once('|') {
            Some((t, i)) => (t.to_string(), i.to_string()),
            None => (raw, String::new()), // legacy bare "<updated>" cursor
        };
        // Defensive: a value that could break the interpolated filter (only
        // possible from a misbehaving/hostile server) resets the cursor.
        if cur_ts.contains('\'') || cur_id.contains('\'') {
            cur_ts.clear();
            cur_id.clear();
        }

        let ws = self.config.workspace_id.as_str();
        let mut newest_ts = cur_ts.clone();
        let mut newest_id = cur_id.clone();
        let mut page: u32 = 1;

        loop {
            let filter = if cur_ts.is_empty() {
                format!("workspace='{}'", ws)
            } else {
                format!(
                    "workspace='{}' && (updated > '{}' || (updated = '{}' && id > '{}'))",
                    ws, cur_ts, cur_ts, cur_id
                )
            };
            let page_s = page.to_string();
            let resp = self
                .http
                .get(format!("{}/api/collections/{}/records", self.config.base_url, collection))
                .bearer_auth(token)
                .query(&[
                    ("filter", filter.as_str()),
                    ("sort", "updated,id"),
                    ("perPage", "200"),
                    ("page", page_s.as_str()),
                ])
                .send()
                .await?;
            let resp = self.pb_ok(resp).await?;
            let list: serde_json::Value = resp.json().await?;

            let Some(items) = list.get("items").and_then(|v| v.as_array()) else {
                break;
            };
            if items.is_empty() {
                break;
            }
            for item in items {
                apply(item)?;
                let u = item.get("updated").and_then(|v| v.as_str()).unwrap_or_default();
                let id = item.get("id").and_then(|v| v.as_str()).unwrap_or_default();
                if (u, id) > (newest_ts.as_str(), newest_id.as_str()) {
                    newest_ts = u.to_string();
                    newest_id = id.to_string();
                }
            }
            let total_pages = list.get("totalPages").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
            if page >= total_pages {
                break;
            }
            page += 1;
        }

        if newest_ts != cur_ts || newest_id != cur_id {
            self.write_state(&cursor_key, &format!("{}|{}", newest_ts, newest_id))?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn apply_remote_note_json(&self, v: &serde_json::Value) -> Result<()> {
        let n: RemoteNote = serde_json::from_value(v.clone())?;
        self.apply_remote_note(&n)
    }

    fn apply_remote_folder_json(&self, v: &serde_json::Value) -> Result<()> {
        let f: RemoteFolder = serde_json::from_value(v.clone())?;
        self.apply_remote_folder(&f)
    }

    fn apply_remote_prompt_json(&self, v: &serde_json::Value) -> Result<()> {
        let p: RemotePrompt = serde_json::from_value(v.clone())?;
        self.apply_remote_prompt(&p)
    }

    /// Apply one pulled note to the local store via raw SQL. Bypasses the app
    /// commands (no observer re-ping) and writes the server's timestamp as
    /// `updated_at` (NOT now()), with a last-write-wins guard so a stale pull
    /// can't clobber a newer local edit.
    fn apply_remote_note(&self, n: &RemoteNote) -> Result<()> {
        // Reject a non-UUID client_id from the server: it would become a local
        // primary key and later be interpolated into PocketBase filters. Our
        // client only ever creates UUID ids, so anything else is malformed or
        // hostile — skip it (closes both the filter-injection and the
        // find_remote_id-misses → duplicate-create failure mode).
        if !is_safe_id(&n.client_id) {
            eprintln!("cloud-sync: skipping pulled note with unsafe client_id");
            return Ok(());
        }
        let conn = self.db.lock();
        if n.deleted {
            // Soft-delete (move to Trash) rather than dropping the row: the
            // deletion stays recoverable, and re-pulling our own tombstone is
            // idempotent (it won't purge the local Trash entry). Scoped to the
            // pulled workspace (a tombstone from the old workspace mustn't touch
            // a copy just moved elsewhere) and LWW-guarded (a newer local edit
            // wins over the delete).
            conn.execute(
                "UPDATE notes SET deleted_at = ?3, updated_at = ?3
                 WHERE id = ?1 AND workspace_id = ?2 AND ?3 >= updated_at",
                rusqlite::params![n.client_id, self.config.workspace_id, n.client_updated_at],
            )?;
            return Ok(());
        }

        // Conflict guard. If this device has an unpushed local edit for the note
        // (a pending outbox upsert) AND the incoming server version is newer,
        // applying it below would silently wipe the user's in-progress edits.
        // Preserve them first as a local-only "(conflict copy)" — Dropbox-style,
        // nothing is lost — and let the server version become canonical.
        // All queries are scoped to the pulled workspace (`ws`): the local row,
        // its pending push, and the drop must all concern THIS workspace's copy,
        // never a same-client_id row that was moved to another workspace.
        let ws = self.config.workspace_id.as_str();
        let conflict_title: Option<String> = {
            let local: Option<(i64, String)> = conn
                .query_row(
                    "SELECT updated_at, title FROM notes WHERE id = ?1 AND workspace_id = ?2",
                    rusqlite::params![n.client_id, ws],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            match local {
                Some((local_updated, title)) if n.client_updated_at > local_updated => {
                    let has_pending = conn
                        .query_row(
                            "SELECT 1 FROM sync_outbox WHERE entity='note' AND entity_id=?1 AND op='upsert' AND workspace=?2 LIMIT 1",
                            rusqlite::params![n.client_id, ws],
                            |_| Ok(()),
                        )
                        .optional()?
                        .is_some();
                    if has_pending {
                        let copy_id = format!("{}-conflict-{}", n.client_id, now_ms());
                        // Copy every field from the current local row; mark it
                        // Personal (workspace '') so the copy never syncs back.
                        conn.execute(
                            "INSERT INTO notes
                                (id, title, body, transcript, summary, audio_path, summary_preset,
                                 folder_id, language, summary_provider, expected_speakers, owner,
                                 workspace_id, created_at, updated_at)
                             SELECT ?1, title || ' (conflict copy)', body, transcript, summary,
                                 audio_path, summary_preset, folder_id, language, summary_provider,
                                 expected_speakers, '', '', created_at, ?2
                             FROM notes WHERE id = ?3",
                            rusqlite::params![copy_id, now_ms(), n.client_id],
                        )?;
                        // Drop the queued push for the original (this workspace) —
                        // we're taking the server's version, so it would just echo.
                        conn.execute(
                            "DELETE FROM sync_outbox WHERE entity='note' AND entity_id=?1 AND op='upsert' AND workspace=?2",
                            rusqlite::params![n.client_id, ws],
                        )?;
                        Some(title)
                    } else {
                        None
                    }
                }
                _ => None,
            }
        };

        let folder = if n.folder_client_id.is_empty() {
            None
        } else {
            Some(n.folder_client_id.as_str())
        };
        let speakers = if n.expected_speakers > 0 { Some(n.expected_speakers) } else { None };
        conn.execute(
            "INSERT INTO notes
                (id, title, body, transcript, summary, audio_path, summary_preset,
                 folder_id, language, summary_provider, expected_speakers, owner, workspace_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, ?7, ?8, '', ?9, ?12, ?13, ?10, ?11)
             ON CONFLICT(id) DO UPDATE SET
                title=excluded.title, body=excluded.body, transcript=excluded.transcript,
                summary=excluded.summary, summary_preset=excluded.summary_preset,
                folder_id=excluded.folder_id, language=excluded.language,
                expected_speakers=excluded.expected_speakers, owner=excluded.owner,
                updated_at=excluded.updated_at, deleted_at=NULL
             WHERE excluded.updated_at >= notes.updated_at",
            rusqlite::params![
                n.client_id,
                n.title,
                n.body,
                n.transcript,
                n.summary,
                n.summary_preset,
                folder,
                n.language,
                speakers,
                n.created_at,
                n.client_updated_at,
                n.owner,
                self.config.workspace_id,
            ],
        )?;
        drop(conn); // release before firing the callback
        if let Some(title) = conflict_title {
            (self.conflict)(&title);
        }
        Ok(())
    }

    fn apply_remote_folder(&self, f: &RemoteFolder) -> Result<()> {
        if !is_safe_id(&f.client_id) {
            eprintln!("cloud-sync: skipping pulled folder with unsafe client_id");
            return Ok(());
        }
        let conn = self.db.lock();
        if f.deleted {
            conn.execute(
                "DELETE FROM folders WHERE id = ?1 AND workspace_id = ?2",
                rusqlite::params![f.client_id, self.config.workspace_id],
            )?;
            return Ok(());
        }
        // folders has no separate client-created_at column synced; seed it from
        // client_updated_at on first insert.
        conn.execute(
            "INSERT INTO folders (id, name, created_at, updated_at, workspace_id)
             VALUES (?1, ?2, ?3, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET name=excluded.name, updated_at=excluded.updated_at
             WHERE excluded.updated_at >= folders.updated_at",
            rusqlite::params![f.client_id, f.name, f.client_updated_at, self.config.workspace_id],
        )?;
        Ok(())
    }

    fn apply_remote_prompt(&self, p: &RemotePrompt) -> Result<()> {
        if !is_safe_id(&p.client_id) {
            eprintln!("cloud-sync: skipping pulled prompt with unsafe client_id");
            return Ok(());
        }
        let conn = self.db.lock();
        if p.deleted {
            conn.execute(
                "DELETE FROM summary_prompts WHERE id = ?1 AND workspace_id = ?2",
                rusqlite::params![p.client_id, self.config.workspace_id],
            )?;
            return Ok(());
        }
        conn.execute(
            "INSERT INTO summary_prompts (id, name, content, created_at, updated_at, workspace_id)
             VALUES (?1, ?2, ?3, ?4, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET name=excluded.name, content=excluded.content, updated_at=excluded.updated_at
             WHERE excluded.updated_at >= summary_prompts.updated_at",
            rusqlite::params![p.client_id, p.name, p.content, p.client_updated_at, self.config.workspace_id],
        )?;
        Ok(())
    }

    async fn ensure_auth(&self) -> Result<Auth> {
        let cached = self.auth.lock().clone();
        if let Some(a) = cached {
            return Ok(a);
        }
        let a = self.authenticate().await?;
        *self.auth.lock() = Some(a.clone());
        Ok(a)
    }

    /// Like `error_for_pb`, but drops the cached auth on a 401 so the next
    /// operation re-authenticates instead of looping forever on a stale or
    /// expired token. The current call still errors and retries with fresh
    /// credentials (pushes via the outbox, pulls on the next tick) — without
    /// this, an expired token silently wedges sync until the app restarts.
    async fn pb_ok(&self, resp: reqwest::Response) -> Result<reqwest::Response> {
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            *self.auth.lock() = None;
        }
        error_for_pb(resp).await
    }

    async fn authenticate(&self) -> Result<Auth> {
        let resp = self
            .http
            .post(format!("{}/api/collections/users/auth-with-password", self.config.base_url))
            .json(&json!({ "identity": self.config.email, "password": self.config.password }))
            .send()
            .await?;
        let resp = self.pb_ok(resp).await?;

        #[derive(Deserialize)]
        struct AuthResp {
            token: String,
            record: Rec,
        }
        #[derive(Deserialize)]
        struct Rec {
            id: String,
        }
        let a: AuthResp = resp.json().await?;
        Ok(Auth { token: a.token, user_id: a.record.id })
    }

    fn snapshot_note(&self, uuid: &str) -> Result<Option<NoteRow>> {
        let conn = self.db.lock();
        conn.query_row(
            "SELECT title, body, transcript, summary, language, summary_preset,
                    folder_id, expected_speakers, created_at, updated_at, owner
             FROM notes WHERE id = ?1",
            rusqlite::params![uuid],
            |r| {
                Ok(NoteRow {
                    title: r.get(0)?,
                    body: r.get(1)?,
                    transcript: r.get(2)?,
                    summary: r.get(3)?,
                    language: r.get(4)?,
                    summary_preset: r.get(5)?,
                    folder_id: r.get(6)?,
                    expected_speakers: r.get(7)?,
                    created_at: r.get(8)?,
                    updated_at: r.get(9)?,
                    owner: r.get(10)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    fn snapshot_folder(&self, uuid: &str) -> Result<Option<FolderRow>> {
        let conn = self.db.lock();
        conn.query_row(
            "SELECT name, updated_at FROM folders WHERE id = ?1",
            rusqlite::params![uuid],
            |r| Ok(FolderRow { name: r.get(0)?, updated_at: r.get(1)? }),
        )
        .optional()
        .map_err(Into::into)
    }

    fn snapshot_prompt(&self, uuid: &str) -> Result<Option<PromptRow>> {
        let conn = self.db.lock();
        conn.query_row(
            "SELECT name, content, updated_at FROM summary_prompts WHERE id = ?1",
            rusqlite::params![uuid],
            |r| Ok(PromptRow { name: r.get(0)?, content: r.get(1)?, updated_at: r.get(2)? }),
        )
        .optional()
        .map_err(Into::into)
    }

    fn read_state(&self, key: &str) -> Result<Option<String>> {
        let conn = self.db.lock();
        conn.query_row("SELECT value FROM sync_state WHERE key = ?1", rusqlite::params![key], |r| {
            r.get(0)
        })
        .optional()
        .map_err(Into::into)
    }

    fn write_state(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.db.lock();
        conn.execute(
            "INSERT INTO sync_state (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            rusqlite::params![key, value],
        )?;
        Ok(())
    }
}

struct NoteRow {
    title: String,
    body: String,
    transcript: String,
    summary: String,
    language: String,
    summary_preset: String,
    folder_id: Option<String>,
    expected_speakers: Option<i64>,
    created_at: i64,
    updated_at: i64,
    owner: String,
}

struct FolderRow {
    name: String,
    updated_at: i64,
}

struct PromptRow {
    name: String,
    content: String,
    updated_at: i64,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RemoteNote {
    client_id: String,
    deleted: bool,
    title: String,
    body: String,
    transcript: String,
    summary: String,
    language: String,
    summary_preset: String,
    folder_client_id: String,
    expected_speakers: i64,
    owner: String,
    created_at: i64,
    client_updated_at: i64,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RemoteFolder {
    client_id: String,
    deleted: bool,
    name: String,
    client_updated_at: i64,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RemotePrompt {
    client_id: String,
    deleted: bool,
    name: String,
    content: String,
    client_updated_at: i64,
}

fn init_tables(db: &Db) -> Result<()> {
    let conn = db.lock();
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS sync_outbox (
            seq         INTEGER PRIMARY KEY AUTOINCREMENT,
            entity      TEXT NOT NULL,
            entity_id   TEXT NOT NULL,
            op          TEXT NOT NULL,
            workspace   TEXT NOT NULL DEFAULT '',
            attempts    INTEGER NOT NULL DEFAULT 0,
            enqueued_at INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS sync_state (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
         );",
    )?;
    // Idempotent migrations for outboxes created before these columns existed.
    // `workspace`: per-row push target (empty = Personal, dropped). `attempts`:
    // retry counter for poison-row quarantine.
    let _ = conn.execute("ALTER TABLE sync_outbox ADD COLUMN workspace TEXT NOT NULL DEFAULT ''", []);
    let _ = conn.execute("ALTER TABLE sync_outbox ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0", []);
    // Coalesce to at most one pending op per record: drop older duplicates
    // (keep the newest seq), then enforce it with a unique index that `enqueue`
    // upserts onto. A burst of edits collapses to a single pending push instead
    // of one outbox row (and one network round-trip) per keystroke-save.
    conn.execute(
        "DELETE FROM sync_outbox WHERE seq NOT IN (SELECT MAX(seq) FROM sync_outbox GROUP BY entity, entity_id, workspace)",
        [],
    )?;
    // Older builds keyed the index on (entity, entity_id); replace it with one
    // that includes workspace so a workspace move (tombstone-in-old +
    // upsert-in-new for the same note) can hold both rows at once.
    let _ = conn.execute("DROP INDEX IF EXISTS idx_outbox_entity", []);
    conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_outbox_entity_ws ON sync_outbox(entity, entity_id, workspace)",
        [],
    )?;
    Ok(())
}

/// True if `s` is a safe local id: non-empty and only `[A-Za-z0-9_-]`. Our client
/// only mints such ids (UUIDs), so rejecting anything else stops a server-sent
/// `client_id` from carrying PocketBase filter metacharacters (quotes, operators,
/// whitespace) — it becomes a local primary key and is later interpolated into a
/// filter string on push. Closes both the filter-injection and the
/// find_remote_id-misses → spurious-duplicate-create failure mode.
fn is_safe_id(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

/// After this many consecutive failed push attempts, a transient-failing outbox
/// row is stepped over (kept queued and retried on the next tick, NOT dropped) so
/// one persistently-stuck row can't head-of-line-block every later change. Low
/// enough that a poison row stops blocking newer edits quickly; an ordinary
/// outage just waits, because all rows fail together anyway.
const MAX_TRANSIENT_ATTEMPTS: i64 = 5;

/// A non-2xx response from PocketBase, carrying the numeric HTTP status so a push
/// failure can be classified on the *status alone* — never by string-matching the
/// (server-controlled) response body, which could otherwise flip the verdict.
#[derive(Debug)]
struct PbError {
    status: u16,
    body: String,
}

impl std::fmt::Display for PbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "pocketbase {}: {}", self.status, self.body)
    }
}

impl std::error::Error for PbError {}

/// A locally-determined permanent push failure that isn't an HTTP status. Unlike
/// a transient network error, retrying can never succeed as enqueued, so the
/// drain loop drops it. The one producer today: a session push whose parent note
/// has been permanently dropped from the outbox (and isn't on the server), so
/// the session can never resolve its `note` relation and would otherwise
/// orphan-loop every tick (auth + GET /notes forever) — and, via
/// `cloud_pending_note_ids`, pin a note's "syncing…" indicator on for good.
#[derive(Debug)]
struct PermanentPushError(String);

impl std::fmt::Display for PermanentPushError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for PermanentPushError {}

/// True for a 4xx that means "this exact request is malformed/forbidden and will
/// always be rejected", so dropping the outbox row is safe. This is a strict
/// allow-list: anything not listed (401 auth-expired, 408/409/413/423/425/429,
/// every 5xx, transport/timeout errors) is treated as transient and KEPT for
/// retry — we only ever drop a change when we're certain retrying can't help.
fn is_permanent_status(status: u16) -> bool {
    matches!(status, 400 | 403 | 404 | 405 | 422)
}

/// Classify a push failure as permanent (retrying the same payload can't fix it)
/// vs transient. Branches ONLY on the numeric status of a typed `PbError`; a
/// non-HTTP error (reqwest transport, timeout, serde) isn't a `PbError` → treated
/// as transient.
fn is_permanent_push_error(e: &anyhow::Error) -> bool {
    e.downcast_ref::<PbError>().is_some_and(|pb| is_permanent_status(pb.status))
        // A locally-classified permanent failure (e.g. an orphaned session whose
        // parent note is gone) is also un-retryable — drop rather than loop.
        || e.downcast_ref::<PermanentPushError>().is_some()
}

/// True if the error is a PocketBase 401 (auth/token expired) — the realtime loop
/// uses this to drop its cached token and re-authenticate.
fn is_unauthorized(e: &anyhow::Error) -> bool {
    e.downcast_ref::<PbError>().is_some_and(|pb| pb.status == 401)
}

/// Pack a session outbox row's `entity_id` as `<note_id>/<session_id>`. Both
/// are client-minted UUIDs, so the `/` separator is unambiguous and neither
/// half carries filter metacharacters.
fn session_entity_id(note_id: &str, session_id: &str) -> String {
    format!("{note_id}/{session_id}")
}

/// Inverse of [`session_entity_id`]. A malformed value (no `/`) yields an empty
/// note id and the whole string as the session id — the push then simply finds
/// no manifest entry and no-ops.
fn split_session_entity_id(entity_id: &str) -> (String, String) {
    match entity_id.split_once('/') {
        Some((n, s)) => (n.to_string(), s.to_string()),
        None => (String::new(), entity_id.to_string()),
    }
}

/// RFC3339 (manifest `started_at`) → epoch-ms for the note_sessions contract.
/// Empty / unparseable → 0.
fn started_at_to_ms(rfc3339: &str) -> i64 {
    if rfc3339.trim().is_empty() {
        return 0;
    }
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .map(|dt| dt.timestamp_millis())
        .unwrap_or(0)
}

/// Minimal mirror of the app's `sessions.json` manifest — just the fields a
/// metadata push needs. The crate is framework-agnostic (can't depend on the
/// app's `sessions` module), so this deliberately duplicates the shape, exactly
/// as the worker already duplicates knowledge of the SQLite schema.
#[derive(Deserialize)]
struct ManifestFile {
    #[serde(default)]
    sessions: Vec<ManifestEntry>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManifestEntry {
    id: String,
    #[serde(default)]
    index: u32,
    #[serde(default)]
    started_at: String,
    #[serde(default)]
    duration_ms: u64,
    #[serde(default)]
    streams: Vec<String>,
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Turn a non-2xx PocketBase response into a typed `PbError` carrying the numeric
/// status and body text.
async fn error_for_pb(resp: reqwest::Response) -> Result<reqwest::Response> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    Err(anyhow::Error::new(PbError { status, body }))
}

/// Best-effort realtime: keep a PocketBase SSE connection open and, on every
/// record change in a subscribed collection, nudge `notify` to pull. The token
/// is reused across reconnects (re-auth only on a 401), and reconnects use
/// exponential backoff capped at 5 minutes — so a persistently-broken realtime
/// endpoint can't hammer the login endpoint (and trip its rate-limit) every few
/// seconds. All failures are non-fatal: the worker's interval poll is the
/// reliable path; realtime only lowers latency when it's connected.
async fn realtime_loop(config: Config, http: reqwest::Client, notify: Arc<tokio::sync::Notify>) {
    let mut backoff_secs = 1u64;
    let mut token: Option<String> = None;
    loop {
        if token.is_none() {
            match realtime_auth(&config, &http).await {
                Ok(t) => token = Some(t),
                Err(e) => {
                    eprintln!("cloud-sync: realtime auth failed ({e:#}); polling continues");
                    tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                    backoff_secs = (backoff_secs * 2).min(300);
                    continue;
                }
            }
        }
        let started = now_ms();
        let tok = token.clone().unwrap_or_default();
        if let Err(e) = realtime_once(&config, &http, &tok, &notify).await {
            eprintln!("cloud-sync: realtime disconnected ({e:#}); polling continues");
            if is_unauthorized(&e) {
                token = None; // token died → re-auth next round
            }
        }
        // A connection that stayed up a while was healthy → reset the backoff.
        if now_ms() - started > 60_000 {
            backoff_secs = 1;
        }
        tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
        backoff_secs = (backoff_secs * 2).min(300);
    }
}

/// Authenticate for realtime, returning a token to reuse across reconnects.
async fn realtime_auth(config: &Config, http: &reqwest::Client) -> Result<String> {
    let auth = http
        .post(format!("{}/api/collections/users/auth-with-password", config.base_url))
        .json(&json!({ "identity": config.email, "password": config.password }))
        .send()
        .await?;
    let auth = error_for_pb(auth).await?.json::<serde_json::Value>().await?;
    Ok(auth.get("token").and_then(|v| v.as_str()).unwrap_or_default().to_string())
}

async fn realtime_once(
    config: &Config,
    http: &reqwest::Client,
    token: &str,
    notify: &Arc<tokio::sync::Notify>,
) -> Result<()> {
    let mut resp = http.get(format!("{}/api/realtime", config.base_url)).send().await?;
    if !resp.status().is_success() {
        return Err(anyhow::Error::new(PbError {
            status: resp.status().as_u16(),
            body: "realtime connect".to_string(),
        }));
    }

    let mut buf: Vec<u8> = Vec::new();
    let mut subscribed = false;
    // `chunk()` reads the next piece of the streamed body (no StreamExt needed).
    while let Some(chunk) = resp.chunk().await? {
        buf.extend_from_slice(&chunk);
        // Bound memory, but generously: a single SSE event carries a full changed
        // record, and a long meeting transcript can be hundreds of KB — tearing
        // down realtime on a legit large note would just bounce the connection
        // (the interval poll is the correctness backstop regardless). Only a
        // stream that never sends a delimiter should ever hit this.
        if buf.len() > 16_000_000 {
            return Err(anyhow!("realtime: event buffer overflow"));
        }
        // SSE events are separated by a blank line — handle both LF and CRLF.
        while let Some((idx, delim)) = find_event_end(&buf) {
            let block = String::from_utf8_lossy(&buf[..idx]).to_string();
            buf.drain(..idx + delim);
            let (event, data) = parse_sse(&block);
            match event.as_deref() {
                Some("PB_CONNECT") => {
                    let client_id = serde_json::from_str::<serde_json::Value>(&data)
                        .ok()
                        .and_then(|v| v.get("clientId").and_then(|c| c.as_str()).map(String::from));
                    if let Some(client_id) = client_id {
                        let sub = http
                            .post(format!("{}/api/realtime", config.base_url))
                            .bearer_auth(token)
                            .json(&json!({
                                "clientId": client_id,
                                "subscriptions": ["notes", "folders", "summary_prompts"],
                            }))
                            .send()
                            .await?;
                        if !sub.status().is_success() {
                            return Err(anyhow::Error::new(PbError {
                                status: sub.status().as_u16(),
                                body: "realtime subscribe".to_string(),
                            }));
                        }
                        subscribed = true;
                    }
                }
                // Any record event in a subscribed collection → coalesced pull.
                Some(_) if subscribed => notify.notify_one(),
                _ => {}
            }
        }
    }
    Ok(()) // stream ended → caller reconnects
}

/// Find the end of the first complete SSE event in `buf`, returning the index
/// where the delimiter starts and its length. Handles both LF (`\n\n`) and CRLF
/// (`\r\n\r\n`) so a proxy that rewrites line endings doesn't silently break
/// realtime, picking whichever delimiter comes first.
fn find_event_end(buf: &[u8]) -> Option<(usize, usize)> {
    let lf = buf.windows(2).position(|w| w == b"\n\n").map(|i| (i, 2));
    let crlf = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| (i, 4));
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

/// Parse one SSE event block into (event name, data payload).
fn parse_sse(block: &str) -> (Option<String>, String) {
    let mut event = None;
    let mut data = String::new();
    for line in block.lines() {
        if let Some(v) = line.strip_prefix("event:") {
            event = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("data:") {
            data.push_str(v.strip_prefix(' ').unwrap_or(v));
        }
    }
    (event, data)
}


#[cfg(test)]
mod it {
    //! Integration tests that drive the REAL worker against a live PocketBase
    //! (push → pull → tombstone), for notes, folders, and prompts. Skip unless
    //! HUMLA_TEST_* env vars are set — `scripts/integration.sh` boots
    //! PocketBase, seeds a user + workspace, exports them, and runs these.
    //! Plain `cargo test` skips them.
    use super::*;
    use std::time::Duration;

    fn env_config() -> Option<Config> {
        Some(Config {
            base_url: std::env::var("HUMLA_TEST_PB_URL").ok()?,
            email: std::env::var("HUMLA_TEST_EMAIL").ok()?,
            password: std::env::var("HUMLA_TEST_PASSWORD").ok()?,
            workspace_id: std::env::var("HUMLA_TEST_WORKSPACE").ok()?,
            poll_interval: Duration::from_secs(60),
            recordings_dir: std::env::temp_dir(),
        })
    }

    /// Throwaway in-memory DB with the columns the worker touches — mirrors
    /// humla's notes/folders/summary_prompts schema + the worker's own tables.
    fn test_db() -> Db {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE notes (
                id TEXT PRIMARY KEY, title TEXT NOT NULL DEFAULT '',
                body TEXT NOT NULL DEFAULT '', transcript TEXT NOT NULL DEFAULT '',
                summary TEXT NOT NULL DEFAULT '', audio_path TEXT,
                summary_preset TEXT NOT NULL DEFAULT 'meeting', folder_id TEXT,
                language TEXT NOT NULL DEFAULT '', summary_provider TEXT NOT NULL DEFAULT '',
                expected_speakers INTEGER, owner TEXT NOT NULL DEFAULT '',
                workspace_id TEXT NOT NULL DEFAULT '', deleted_at INTEGER,
                created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
            );
            CREATE TABLE folders (
                id TEXT PRIMARY KEY, name TEXT NOT NULL,
                workspace_id TEXT NOT NULL DEFAULT '',
                created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
            );
            CREATE TABLE summary_prompts (
                id TEXT PRIMARY KEY, name TEXT NOT NULL, content TEXT NOT NULL,
                workspace_id TEXT NOT NULL DEFAULT '',
                created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
            );",
        )
        .unwrap();
        let db = Arc::new(Mutex::new(conn));
        init_tables(&db).unwrap();
        db
    }

    fn worker(db: Db, config: Config) -> Worker {
        Worker {
            db,
            config,
            http: reqwest::Client::new(),
            notify: Arc::new(|| {}) as Arc<dyn Fn() + Send + Sync>,
            status: Arc::new(|_| {}) as Arc<dyn Fn(SyncState) + Send + Sync>,
            conflict: Arc::new(|_: &str| {}) as Arc<dyn Fn(&str) + Send + Sync>,
            auth: Mutex::new(None),
        }
    }

    fn scalar(db: &Db, sql: &str, id: &str) -> Option<String> {
        let conn = db.lock();
        conn.query_row(sql, rusqlite::params![id], |r| r.get(0)).optional().unwrap()
    }

    fn offline_config(workspace: &str) -> Config {
        Config {
            base_url: String::new(),
            email: String::new(),
            password: String::new(),
            workspace_id: workspace.to_string(),
            poll_interval: Duration::from_secs(60),
            recordings_dir: std::path::PathBuf::new(),
        }
    }

    /// Outbox coalescing (no network): repeated edits to the same record collapse
    /// to a single pending op, the latest op wins, and the per-row workspace is
    /// captured from the note. Guards the P1 churn/head-of-line fixes.
    #[test]
    fn outbox_coalesces_per_record() {
        let db = test_db();
        let w = worker(db.clone(), offline_config("wsX"));
        {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO notes (id, workspace_id, created_at, updated_at) VALUES ('n1', 'wsX', 1, 1)",
                [],
            )
            .unwrap();
        }
        w.enqueue(Op::Note { id: "n1".into(), delete: false }).unwrap();
        w.enqueue(Op::Note { id: "n1".into(), delete: false }).unwrap();
        w.enqueue(Op::Note { id: "n1".into(), delete: true }).unwrap();

        let conn = db.lock();
        let (count, op, ws): (i64, String, String) = conn
            .query_row(
                "SELECT COUNT(*), MAX(op), MAX(workspace) FROM sync_outbox WHERE entity='note' AND entity_id='n1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(count, 1, "three enqueues collapse to one pending row");
        assert_eq!(op, "delete", "latest op wins");
        assert_eq!(ws, "wsX", "upsert captured the note's own workspace");
    }

    /// Session outbox entity_id packing round-trips, and a malformed value
    /// degrades safely (empty note id → push finds no manifest entry, no-ops).
    #[test]
    fn session_entity_id_roundtrips() {
        let packed = session_entity_id("note-uuid", "sess-uuid");
        assert_eq!(packed, "note-uuid/sess-uuid");
        assert_eq!(
            split_session_entity_id(&packed),
            ("note-uuid".to_string(), "sess-uuid".to_string())
        );
        assert_eq!(
            split_session_entity_id("no-slash"),
            (String::new(), "no-slash".to_string())
        );
    }

    /// started_at RFC3339 → epoch-ms (the note_sessions contract's number),
    /// with empty / unparseable collapsing to 0.
    #[test]
    fn started_at_parses_to_ms() {
        assert!(started_at_to_ms("2026-07-09T10:00:00+00:00") > 0);
        assert_eq!(started_at_to_ms(""), 0);
        assert_eq!(started_at_to_ms("garbage"), 0);
    }

    /// A session upsert enqueues one row keyed on the packed (note/session)
    /// entity_id, capturing the parent note's workspace; a repeat coalesces.
    #[test]
    fn session_enqueue_captures_workspace_and_coalesces() {
        let db = test_db();
        let w = worker(db.clone(), offline_config("wsS"));
        {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO notes (id, workspace_id, created_at, updated_at) VALUES ('n1', 'wsS', 1, 1)",
                [],
            )
            .unwrap();
        }
        w.enqueue(Op::Session { note_id: "n1".into(), session_id: "s1".into(), delete: false })
            .unwrap();
        w.enqueue(Op::Session { note_id: "n1".into(), session_id: "s1".into(), delete: false })
            .unwrap();

        let conn = db.lock();
        let (count, eid, ws): (i64, String, String) = conn
            .query_row(
                "SELECT COUNT(*), MAX(entity_id), MAX(workspace) FROM sync_outbox WHERE entity='session'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(count, 1, "repeat session enqueues collapse to one row");
        assert_eq!(eid, "n1/s1");
        assert_eq!(ws, "wsS", "captured the parent note's workspace");
    }

    /// Reads the just-recorded take's metadata out of a real sessions.json.
    #[test]
    fn read_session_meta_from_manifest() {
        let root = std::env::temp_dir().join(format!("humla-sess-test-{}", now_ms()));
        let note_dir = root.join("note-1");
        std::fs::create_dir_all(&note_dir).unwrap();
        std::fs::write(
            note_dir.join("sessions.json"),
            r#"{"version":1,"sessions":[
                {"id":"sA","index":1,"startedAt":"2026-07-09T10:00:00+00:00","durationMs":4200,"streams":["mic","sys"]}
            ]}"#,
        )
        .unwrap();
        let mut cfg = offline_config("wsS");
        cfg.recordings_dir = root.clone();
        let w = worker(test_db(), cfg);
        let meta = w.read_session_meta("note-1", "sA").expect("entry present");
        assert_eq!(meta.index, 1);
        assert_eq!(meta.duration_ms, 4200);
        assert_eq!(meta.streams, vec!["mic", "sys"]);
        assert!(w.read_session_meta("note-1", "missing").is_none());
        assert!(w.read_session_meta("no-note", "sA").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Ingest id safety: real UUIDs + plain test ids pass; filter-metacharacter
    /// ids (the injection vector) are rejected.
    #[test]
    fn rejects_unsafe_client_ids() {
        assert!(is_safe_id("550e8400-e29b-41d4-a716-446655440000"));
        assert!(is_safe_id("note-123"));
        assert!(!is_safe_id(""));
        assert!(!is_safe_id("x' || workspace!='"));
        assert!(!is_safe_id("a b"));
        assert!(!is_safe_id("a=b"));
    }

    /// SSE block parsing for the realtime listener.
    #[test]
    fn parses_sse_event_blocks() {
        let (e, d) = parse_sse("id:abc\nevent:notes\ndata: {\"id\":\"x\"}");
        assert_eq!(e.as_deref(), Some("notes"));
        assert_eq!(d, "{\"id\":\"x\"}", "leading space after data: is stripped");
        let (e2, d2) = parse_sse("event:PB_CONNECT\ndata:{\"clientId\":\"c1\"}");
        assert_eq!(e2.as_deref(), Some("PB_CONNECT"));
        assert!(d2.contains("c1"));
        // A keepalive/comment block has no event.
        assert_eq!(parse_sse(":keepalive").0, None);
    }

    /// Push-error classification: ONLY the allow-listed 4xx are permanent (safe to
    /// drop); auth-expired, rate-limit, payload-too-large, every 5xx, and non-HTTP
    /// transport errors are transient (kept for retry). Guards both review
    /// regressions — a transient failure misread as permanent (silent data loss),
    /// and the verdict being influenced by the server-controlled response body.
    #[test]
    fn classifies_push_errors_by_status_only() {
        let perm =
            |s: u16| is_permanent_push_error(&anyhow::Error::new(PbError { status: s, body: String::new() }));
        // Permanent (drop): malformed / forbidden / not-found / validation.
        for s in [400u16, 403, 404, 405, 422] {
            assert!(perm(s), "status {s} should be permanent");
        }
        // Transient (keep + retry): auth, payment-required (the server billing
        // gate returns 402 so a queued edit survives a lapse and re-syncs on
        // resubscribe instead of being dropped), timeout, conflict,
        // payload-too-large, locked, too-early, rate-limit, and all 5xx.
        for s in [401u16, 402, 408, 409, 413, 423, 425, 429, 500, 502, 503] {
            assert!(!perm(s), "status {s} should be transient");
        }
        // The body must NEVER flip the verdict: a 5xx whose body echoes a
        // permanent-looking code stays transient.
        assert!(!is_permanent_push_error(&anyhow::Error::new(PbError {
            status: 500,
            body: "pocketbase 400: bad request".to_string(),
        })));
        // A non-HTTP error (transport / timeout / serde) is transient.
        assert!(!is_permanent_push_error(&anyhow!("connection reset by peer")));
        // A locally-classified permanent failure (orphaned session) is permanent
        // regardless of status — it carries no HTTP code.
        assert!(is_permanent_push_error(
            &PermanentPushError("orphaned session".into()).into()
        ));
        // 401 is recognised for realtime re-auth — by status, not substring.
        assert!(is_unauthorized(&anyhow::Error::new(PbError { status: 401, body: String::new() })));
        assert!(!is_unauthorized(&anyhow::Error::new(PbError { status: 403, body: String::new() })));
        assert!(!is_unauthorized(&anyhow!("incidental 401 in some message")));
    }

    /// BUG D: an orphaned session push (parent note not on the server) is
    /// TRANSIENT while the note's own upsert is still queued (it just hasn't
    /// drained), but PERMANENT once that row is gone — the note was dropped, so
    /// the session can never resolve its `note` relation and must not loop.
    /// Also verifies the per-note "syncing…" set (mirrors `cloud_pending_note_ids`)
    /// clears once the orphaned session outbox row is dropped.
    #[test]
    fn orphan_session_push_transient_until_note_dropped_then_permanent() {
        let db = test_db();
        let w = worker(db.clone(), offline_config("wsS"));
        {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO notes (id, workspace_id, created_at, updated_at) VALUES ('n1', 'wsS', 1, 1)",
                [],
            )
            .unwrap();
        }
        // Parent note upsert + the session upsert are both queued.
        w.enqueue(Op::Note { id: "n1".into(), delete: false }).unwrap();
        w.enqueue(Op::Session { note_id: "n1".into(), session_id: "s1".into(), delete: false })
            .unwrap();

        // (a) Note still queued → session push is transient (kept for retry).
        let e = w.orphan_session_error("n1", "s1", "wsS");
        assert!(
            !is_permanent_push_error(&e),
            "session push must be transient while the parent note is still queued"
        );

        // Simulate the note push being permanently dropped (an allow-listed 4xx):
        // its outbox row is deleted.
        db.lock()
            .execute(
                "DELETE FROM sync_outbox WHERE entity='note' AND entity_id='n1' AND workspace='wsS'",
                [],
            )
            .unwrap();

        // (b) No queued note upsert remains → the session can never resolve its
        // parent, so the push is permanent (drop, don't loop).
        let e = w.orphan_session_error("n1", "s1", "wsS");
        assert!(
            is_permanent_push_error(&e),
            "session push must be permanent once the parent note is gone"
        );

        // The drain loop drops permanent failures. Mirror that (delete the
        // session row) and confirm the per-note pending set — the query
        // `cloud_pending_note_ids` runs — no longer folds this note in.
        db.lock()
            .execute("DELETE FROM sync_outbox WHERE entity='session' AND entity_id='n1/s1'", [])
            .unwrap();
        let pending: i64 = db
            .lock()
            .query_row(
                "SELECT COUNT(*) FROM sync_outbox WHERE entity IN ('note','session')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pending, 0, "pending set clears once the orphaned session is dropped");
    }

    /// Moving a note across workspaces queues a tombstone in the old workspace
    /// and an upsert in the new one — two coexisting rows, drained delete-first.
    #[test]
    fn note_move_enqueues_tombstone_then_upsert() {
        let db = test_db();
        let w = worker(db.clone(), offline_config("wsB"));
        {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO notes (id, workspace_id, created_at, updated_at) VALUES ('n1', 'wsB', 1, 1)",
                [],
            )
            .unwrap();
        }
        w.enqueue(Op::NoteMove { id: "n1".into(), from: "wsA".into(), to: "wsB".into() }).unwrap();
        let conn = db.lock();
        let rows: Vec<(String, String)> = conn
            .prepare("SELECT op, workspace FROM sync_outbox WHERE entity='note' AND entity_id='n1' ORDER BY seq")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(
            rows,
            vec![("delete".to_string(), "wsA".to_string()), ("upsert".to_string(), "wsB".to_string())],
            "move = delete@from then upsert@to"
        );
    }

    /// Moving FROM Personal ('') only enqueues the upsert — there's no remote
    /// row in Personal to tombstone.
    #[test]
    fn note_move_from_personal_only_upserts() {
        let db = test_db();
        let w = worker(db.clone(), offline_config("wsB"));
        {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO notes (id, workspace_id, created_at, updated_at) VALUES ('n2', 'wsB', 1, 1)",
                [],
            )
            .unwrap();
        }
        w.enqueue(Op::NoteMove { id: "n2".into(), from: "".into(), to: "wsB".into() }).unwrap();
        let conn = db.lock();
        let (count, op): (i64, String) = conn
            .query_row(
                "SELECT COUNT(*), MAX(op) FROM sync_outbox WHERE entity_id='n2'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((count, op.as_str()), (1, "upsert"));
    }

    /// A pull that would overwrite an unpushed local edit preserves the local
    /// version as a local-only "(conflict copy)", takes the server version as
    /// canonical, drops the now-stale pending push, and fires the callback.
    #[test]
    fn conflict_preserves_local_as_copy() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let db = test_db();
        let fired = Arc::new(AtomicBool::new(false));
        let f = fired.clone();
        let w = Worker {
            db: db.clone(),
            config: offline_config("wsA"),
            http: reqwest::Client::new(),
            notify: Arc::new(|| {}),
            status: Arc::new(|_| {}),
            conflict: Arc::new(move |_: &str| f.store(true, Ordering::SeqCst)),
            auth: Mutex::new(None),
        };
        {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO notes (id, title, body, workspace_id, created_at, updated_at) VALUES ('n1','mine','local body','wsA',1,100)",
                [],
            )
            .unwrap();
        }
        w.enqueue(Op::Note { id: "n1".into(), delete: false }).unwrap(); // pending unpushed edit
        let remote = RemoteNote {
            client_id: "n1".into(),
            title: "theirs".into(),
            client_updated_at: 200,
            ..Default::default()
        };
        w.apply_remote_note(&remote).unwrap();

        let conn = db.lock();
        let title: String =
            conn.query_row("SELECT title FROM notes WHERE id='n1'", [], |r| r.get(0)).unwrap();
        assert_eq!(title, "theirs", "server version becomes canonical");
        let (copies, body): (i64, String) = conn
            .query_row(
                "SELECT COUNT(*), MAX(body) FROM notes WHERE title LIKE '%(conflict copy)' AND workspace_id=''",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(copies, 1, "local edit preserved as a local-only copy");
        assert_eq!(body, "local body");
        let pending: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sync_outbox WHERE entity_id='n1' AND op='upsert'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pending, 0, "stale pending push dropped");
        assert!(fired.load(Ordering::SeqCst), "conflict callback fired");
    }

    #[tokio::test]
    async fn note_roundtrip() {
        let Some(config) = env_config() else {
            eprintln!("note_roundtrip: skipped (set HUMLA_TEST_* — see scripts/integration.sh)");
            return;
        };
        let db = test_db();
        let w = worker(db.clone(), config);
        let ws = w.config.workspace_id.clone();
        let uuid = format!("note-{}", now_ms());
        {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO notes (id, title, body, transcript, summary, summary_preset, language, workspace_id, created_at, updated_at)
                 VALUES (?1, 'IT title', '<p>b</p>', 'tx', 'sm', 'meeting', 'en', ?2, 10, 10)",
                rusqlite::params![uuid, ws],
            )
            .unwrap();
        }
        let auth = w.ensure_auth().await.expect("auth");
        w.push_note(&uuid, &ws).await.expect("push_note");
        assert!(
            w.find_remote_id("notes", &uuid, &ws, &auth.token).await.expect("find").is_some(),
            "pushed note should exist remotely"
        );

        { let conn = db.lock(); conn.execute("DELETE FROM notes WHERE id = ?1", rusqlite::params![uuid]).unwrap(); }
        w.write_state("notes_cursor", "").unwrap();
        w.pull_collection("notes", "notes_cursor", &auth.token, |v| w.apply_remote_note_json(v)).await.expect("pull");
        assert_eq!(scalar(&db, "SELECT title FROM notes WHERE id = ?1", &uuid).as_deref(), Some("IT title"));
        // Transcript + summary must survive the push → PB → pull roundtrip too
        // (guards against the PB schema dropping those fields, and against a
        // pull that doesn't restore them — the two things that would make a
        // synced note show an empty transcript/summary).
        assert_eq!(scalar(&db, "SELECT transcript FROM notes WHERE id = ?1", &uuid).as_deref(), Some("tx"));
        assert_eq!(scalar(&db, "SELECT summary FROM notes WHERE id = ?1", &uuid).as_deref(), Some("sm"));
        // Owner is assigned to the syncing user on first push (the note had no
        // owner) and round-trips back on pull → drives "created by" attribution.
        assert_eq!(scalar(&db, "SELECT owner FROM notes WHERE id = ?1", &uuid).as_deref(), Some(auth.user_id.as_str()));

        w.push_delete("notes", &uuid, &ws).await.expect("delete");
        w.write_state("notes_cursor", "").unwrap();
        w.pull_collection("notes", "notes_cursor", &auth.token, |v| w.apply_remote_note_json(v)).await.expect("pull2");
        assert_eq!(
            scalar(&db, "SELECT title FROM notes WHERE id = ?1 AND deleted_at IS NULL", &uuid),
            None,
            "tombstone moves the note to Trash (out of the live set)"
        );
    }

    #[tokio::test]
    async fn folder_roundtrip() {
        let Some(config) = env_config() else {
            eprintln!("folder_roundtrip: skipped");
            return;
        };
        let db = test_db();
        let w = worker(db.clone(), config);
        let ws = w.config.workspace_id.clone();
        let uuid = format!("folder-{}", now_ms());
        { let conn = db.lock(); conn.execute("INSERT INTO folders (id, name, created_at, updated_at) VALUES (?1, 'Team Folder', 10, 10)", rusqlite::params![uuid]).unwrap(); }
        let auth = w.ensure_auth().await.expect("auth");
        w.push_folder(&uuid, &ws).await.expect("push_folder");
        assert!(w.find_remote_id("folders", &uuid, &ws, &auth.token).await.expect("find").is_some());

        { let conn = db.lock(); conn.execute("DELETE FROM folders WHERE id = ?1", rusqlite::params![uuid]).unwrap(); }
        w.write_state("folders_cursor", "").unwrap();
        w.pull_collection("folders", "folders_cursor", &auth.token, |v| w.apply_remote_folder_json(v)).await.expect("pull");
        assert_eq!(scalar(&db, "SELECT name FROM folders WHERE id = ?1", &uuid).as_deref(), Some("Team Folder"));

        w.push_delete("folders", &uuid, &ws).await.expect("delete");
        w.write_state("folders_cursor", "").unwrap();
        w.pull_collection("folders", "folders_cursor", &auth.token, |v| w.apply_remote_folder_json(v)).await.expect("pull2");
        assert_eq!(scalar(&db, "SELECT name FROM folders WHERE id = ?1", &uuid), None);
    }

    #[tokio::test]
    async fn prompt_roundtrip() {
        let Some(config) = env_config() else {
            eprintln!("prompt_roundtrip: skipped");
            return;
        };
        let db = test_db();
        let w = worker(db.clone(), config);
        let ws = w.config.workspace_id.clone();
        let uuid = format!("prompt-{}", now_ms());
        { let conn = db.lock(); conn.execute("INSERT INTO summary_prompts (id, name, content, created_at, updated_at) VALUES (?1, 'Standup', 'Summarize tersely', 10, 10)", rusqlite::params![uuid]).unwrap(); }
        let auth = w.ensure_auth().await.expect("auth");
        w.push_prompt(&uuid, &ws).await.expect("push_prompt");
        assert!(w.find_remote_id("summary_prompts", &uuid, &ws, &auth.token).await.expect("find").is_some());

        { let conn = db.lock(); conn.execute("DELETE FROM summary_prompts WHERE id = ?1", rusqlite::params![uuid]).unwrap(); }
        w.write_state("prompts_cursor", "").unwrap();
        w.pull_collection("summary_prompts", "prompts_cursor", &auth.token, |v| w.apply_remote_prompt_json(v)).await.expect("pull");
        assert_eq!(scalar(&db, "SELECT content FROM summary_prompts WHERE id = ?1", &uuid).as_deref(), Some("Summarize tersely"));

        w.push_delete("summary_prompts", &uuid, &ws).await.expect("delete");
        w.write_state("prompts_cursor", "").unwrap();
        w.pull_collection("summary_prompts", "prompts_cursor", &auth.token, |v| w.apply_remote_prompt_json(v)).await.expect("pull2");
        assert_eq!(scalar(&db, "SELECT content FROM summary_prompts WHERE id = ?1", &uuid), None);
    }

    /// note_sessions metadata push + tombstone against a live PocketBase (#16).
    /// Skipped unless HUMLA_TEST_* are set. Requires the humla-cloud
    /// `note_sessions` collection (migration 1718900900) on the test server.
    /// Reconstruction/download live in the app (cloud.rs + sessions.rs) and are
    /// covered by their own unit tests, so this exercises the crate's slice: a
    /// take's manifest entry → note_sessions record → tombstone.
    #[tokio::test]
    async fn session_roundtrip() {
        let Some(mut config) = env_config() else {
            eprintln!("session_roundtrip: skipped (set HUMLA_TEST_*)");
            return;
        };
        // Stand up a temp recordings dir with a one-take manifest for the push
        // to read (mirrors what the post-stop chain writes on disk).
        let root = std::env::temp_dir().join(format!("humla-sess-it-{}", now_ms()));
        let note_uuid = format!("note-{}", now_ms());
        let sess_uuid = format!("sess-{}", now_ms());
        let note_dir = root.join(&note_uuid);
        std::fs::create_dir_all(&note_dir).unwrap();
        std::fs::write(
            note_dir.join("sessions.json"),
            format!(
                r#"{{"version":1,"sessions":[{{"id":"{sess_uuid}","index":1,"startedAt":"2026-07-09T10:00:00+00:00","durationMs":4200,"streams":["mic","sys"]}}]}}"#
            ),
        )
        .unwrap();
        config.recordings_dir = root.clone();

        let db = test_db();
        let w = worker(db.clone(), config);
        let ws = w.config.workspace_id.clone();
        {
            let conn = db.lock();
            conn.execute(
                "INSERT INTO notes (id, title, workspace_id, created_at, updated_at)
                 VALUES (?1, 'session IT', ?2, 10, 10)",
                rusqlite::params![note_uuid, ws],
            )
            .unwrap();
        }
        let auth = w.ensure_auth().await.expect("auth");
        // The parent note must exist first — push it, then the session.
        w.push_note(&note_uuid, &ws).await.expect("push_note");
        w.push_session(&note_uuid, &sess_uuid, &ws).await.expect("push_session");
        let found = w
            .find_remote_id("note_sessions", &sess_uuid, &ws, &auth.token)
            .await
            .expect("find");
        assert!(found.is_some(), "pushed session should exist remotely");
        assert!(!found.unwrap().1, "record should not be tombstoned yet");

        // Tombstone via the shared delete path, verify the flag flips.
        w.push_delete("note_sessions", &sess_uuid, &ws).await.expect("delete");
        let after = w
            .find_remote_id("note_sessions", &sess_uuid, &ws, &auth.token)
            .await
            .expect("find2");
        assert!(after.map(|(_, deleted, _)| deleted).unwrap_or(false), "session tombstoned");

        let _ = std::fs::remove_dir_all(&root);
    }
}
