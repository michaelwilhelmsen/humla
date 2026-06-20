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
}

enum Op {
    Note { id: String, delete: bool },
    Folder { id: String, delete: bool },
    Prompt { id: String, delete: bool },
}

/// Build the sync engine. Returns the handle plus the worker future; the
/// caller spawns the future on its runtime. In the Tauri app that means
/// `tauri::async_runtime::spawn(fut)` — NOT `tokio::spawn`, which panics when
/// called from Tauri's setup closure (no current runtime there).
pub fn start<N>(
    db: Db,
    config: Config,
    notify: N,
) -> Result<(Arc<CloudSync>, impl std::future::Future<Output = ()>)>
where
    N: Fn() + Send + Sync + 'static,
{
    init_tables(&db)?;
    let (tx, rx) = mpsc::unbounded_channel();
    let worker = Worker {
        db,
        config,
        http: reqwest::Client::new(),
        notify: Arc::new(notify),
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
    auth: Mutex<Option<Auth>>,
}

impl Worker {
    async fn run(self, mut rx: mpsc::UnboundedReceiver<Op>) {
        if let Err(e) = self.pull().await {
            eprintln!("cloud-sync: initial pull failed: {e:#}");
        }
        let mut tick = tokio::time::interval(self.config.poll_interval);
        tick.tick().await; // the first tick fires immediately; consume it

        loop {
            tokio::select! {
                op = rx.recv() => {
                    match op {
                        Some(op) => {
                            if let Err(e) = self.enqueue(op) {
                                eprintln!("cloud-sync: enqueue failed: {e:#}");
                            }
                            if let Err(e) = self.drain_outbox().await {
                                eprintln!("cloud-sync: push failed (will retry): {e:#}");
                            }
                        }
                        None => break, // every sender dropped → shut down
                    }
                }
                _ = tick.tick() => {
                    if let Err(e) = self.drain_outbox().await {
                        eprintln!("cloud-sync: push failed (will retry): {e:#}");
                    }
                    if let Err(e) = self.pull().await {
                        eprintln!("cloud-sync: pull failed (will retry): {e:#}");
                    }
                }
            }
        }
    }

    /// Persist an op to the durable outbox so a crash between enqueue and push
    /// doesn't lose the change.
    fn enqueue(&self, op: Op) -> Result<()> {
        let (entity, entity_id, kind, is_delete) = match op {
            Op::Note { id, delete } => ("note", id, if delete { "delete" } else { "upsert" }, delete),
            Op::Folder { id, delete } => ("folder", id, if delete { "delete" } else { "upsert" }, delete),
            Op::Prompt { id, delete } => ("prompt", id, if delete { "delete" } else { "upsert" }, delete),
        };
        let conn = self.db.lock();
        // Capture which workspace this op targets. Upsert → read the row's own
        // workspace_id so it pushes to the right tenant even if the active
        // workspace changes before it drains. Delete → the row is gone, so use
        // the active workspace (you can only delete what you can see, and reads
        // are workspace-scoped). Empty workspace = Personal → never pushed.
        let workspace = if is_delete {
            self.config.workspace_id.clone()
        } else {
            let table = match entity {
                "note" => "notes",
                "folder" => "folders",
                _ => "summary_prompts",
            };
            conn.query_row(
                &format!("SELECT workspace_id FROM {table} WHERE id = ?1"),
                rusqlite::params![entity_id],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .unwrap_or_default()
        };
        conn.execute(
            "INSERT INTO sync_outbox (entity, entity_id, op, workspace, enqueued_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![entity, entity_id, kind, workspace, now_ms()],
        )?;
        Ok(())
    }

    /// Push pending outbox rows oldest-first. Stops at the first failure so the
    /// row stays queued and ordering is preserved for the next attempt.
    async fn drain_outbox(&self) -> Result<()> {
        loop {
            let next: Option<(i64, String, String, String, String)> = {
                let conn = self.db.lock();
                conn.query_row(
                    "SELECT seq, entity, entity_id, op, workspace FROM sync_outbox ORDER BY seq LIMIT 1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                )
                .optional()?
            };
            let Some((seq, entity, entity_id, op, workspace)) = next else {
                return Ok(());
            };

            // Personal / local-only rows ('' workspace) never sync — drop them
            // without a network call. Otherwise push to the row's OWN workspace.
            if !workspace.is_empty() {
                match (entity.as_str(), op.as_str()) {
                    ("note", "upsert") => self.push_note(&entity_id, &workspace).await?,
                    ("note", "delete") => self.push_delete("notes", &entity_id, &workspace).await?,
                    ("folder", "upsert") => self.push_folder(&entity_id, &workspace).await?,
                    ("folder", "delete") => self.push_delete("folders", &entity_id, &workspace).await?,
                    ("prompt", "upsert") => self.push_prompt(&entity_id, &workspace).await?,
                    ("prompt", "delete") => self.push_delete("summary_prompts", &entity_id, &workspace).await?,
                    _ => eprintln!("cloud-sync: unknown outbox row {entity}/{op}; skipping"),
                }
            }

            let conn = self.db.lock();
            conn.execute("DELETE FROM sync_outbox WHERE seq = ?1", rusqlite::params![seq])?;
        }
    }

    async fn push_note(&self, uuid: &str, workspace: &str) -> Result<()> {
        let Some(note) = self.snapshot_note(uuid)? else {
            return Ok(()); // deleted locally before we got here; nothing to push
        };
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
            "client_updated_at": note.updated_at,
            "deleted": false,
        });
        self.upsert_record("notes", uuid, workspace, &auth.token, body).await
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

    /// Soft-delete (tombstone) a record by `client_id` so other devices pull
    /// the deletion instead of the row reappearing on their next pull.
    async fn push_delete(&self, collection: &str, uuid: &str, workspace: &str) -> Result<()> {
        let auth = self.ensure_auth().await?;
        let Some(pb_id) = self.find_remote_id(collection, uuid, workspace, &auth.token).await? else {
            return Ok(()); // never synced; nothing to tombstone
        };
        let resp = self
            .http
            .patch(format!("{}/api/collections/{}/records/{}", self.config.base_url, collection, pb_id))
            .bearer_auth(&auth.token)
            .json(&json!({ "deleted": true, "client_updated_at": now_ms() }))
            .send()
            .await?;
        ensure_ok(resp).await
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
            Some(pb_id) => {
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
        ensure_ok(resp).await
    }

    async fn find_remote_id(
        &self,
        collection: &str,
        client_id: &str,
        workspace: &str,
        token: &str,
    ) -> Result<Option<String>> {
        let filter = format!("client_id='{}' && workspace='{}'", client_id, workspace);
        let resp = self
            .http
            .get(format!("{}/api/collections/{}/records", self.config.base_url, collection))
            .bearer_auth(token)
            .query(&[("filter", filter.as_str()), ("perPage", "1"), ("fields", "id")])
            .send()
            .await?;
        let resp = error_for_pb(resp).await?;

        #[derive(Deserialize)]
        struct ListResp {
            items: Vec<Item>,
        }
        #[derive(Deserialize)]
        struct Item {
            id: String,
        }
        let list: ListResp = resp.json().await?;
        Ok(list.items.into_iter().next().map(|i| i.id))
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
        // re-pulls the wrong tenant's records under a shared cursor.
        let cursor_key = format!("{}:{}", cursor_key, self.config.workspace_id);
        let cursor = self.read_state(&cursor_key)?.unwrap_or_default();
        let mut newest = cursor.clone();
        let mut page: u32 = 1;

        loop {
            let filter = if cursor.is_empty() {
                format!("workspace='{}'", self.config.workspace_id)
            } else {
                format!("workspace='{}' && updated>'{}'", self.config.workspace_id, cursor)
            };
            let page_s = page.to_string();
            let resp = self
                .http
                .get(format!("{}/api/collections/{}/records", self.config.base_url, collection))
                .bearer_auth(token)
                .query(&[
                    ("filter", filter.as_str()),
                    ("sort", "updated"),
                    ("perPage", "200"),
                    ("page", page_s.as_str()),
                ])
                .send()
                .await?;
            let resp = error_for_pb(resp).await?;
            let list: serde_json::Value = resp.json().await?;

            let Some(items) = list.get("items").and_then(|v| v.as_array()) else {
                break;
            };
            if items.is_empty() {
                break;
            }
            for item in items {
                apply(item)?;
                if let Some(u) = item.get("updated").and_then(|v| v.as_str()) {
                    if u > newest.as_str() {
                        newest = u.to_string();
                    }
                }
            }
            let total_pages = list.get("totalPages").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
            if page >= total_pages {
                break;
            }
            page += 1;
        }

        if newest != cursor {
            self.write_state(&cursor_key, &newest)?;
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
        let conn = self.db.lock();
        if n.deleted {
            conn.execute("DELETE FROM notes WHERE id = ?1", rusqlite::params![n.client_id])?;
            return Ok(());
        }
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
                updated_at=excluded.updated_at
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
        Ok(())
    }

    fn apply_remote_folder(&self, f: &RemoteFolder) -> Result<()> {
        let conn = self.db.lock();
        if f.deleted {
            conn.execute("DELETE FROM folders WHERE id = ?1", rusqlite::params![f.client_id])?;
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
        let conn = self.db.lock();
        if p.deleted {
            conn.execute("DELETE FROM summary_prompts WHERE id = ?1", rusqlite::params![p.client_id])?;
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

    async fn authenticate(&self) -> Result<Auth> {
        let resp = self
            .http
            .post(format!("{}/api/collections/users/auth-with-password", self.config.base_url))
            .json(&json!({ "identity": self.config.email, "password": self.config.password }))
            .send()
            .await?;
        let resp = error_for_pb(resp).await?;

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
            enqueued_at INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS sync_state (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
         );",
    )?;
    // Idempotent migration for outboxes created before per-row workspace
    // targeting. Existing queued rows get '' → treated as Personal and dropped
    // (they'd have pushed to the then-active workspace anyway).
    let _ = conn.execute("ALTER TABLE sync_outbox ADD COLUMN workspace TEXT NOT NULL DEFAULT ''", []);
    Ok(())
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Turn a non-2xx PocketBase response into an error carrying the body text.
async fn error_for_pb(resp: reqwest::Response) -> Result<reqwest::Response> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    Err(anyhow!("pocketbase {status}: {body}"))
}

async fn ensure_ok(resp: reqwest::Response) -> Result<()> {
    error_for_pb(resp).await.map(|_| ())
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
                workspace_id TEXT NOT NULL DEFAULT '',
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
            auth: Mutex::new(None),
        }
    }

    fn scalar(db: &Db, sql: &str, id: &str) -> Option<String> {
        let conn = db.lock();
        conn.query_row(sql, rusqlite::params![id], |r| r.get(0)).optional().unwrap()
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
                "INSERT INTO notes (id, title, body, transcript, summary, summary_preset, language, created_at, updated_at)
                 VALUES (?1, 'IT title', '<p>b</p>', 'tx', 'sm', 'meeting', 'en', 10, 10)",
                rusqlite::params![uuid],
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
        assert_eq!(scalar(&db, "SELECT title FROM notes WHERE id = ?1", &uuid), None, "tombstone deletes locally");
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
}
