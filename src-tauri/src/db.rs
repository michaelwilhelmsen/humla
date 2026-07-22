use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Note {
    pub id: String,
    pub title: String,
    pub body: String,
    pub transcript: String,
    pub summary: String,
    pub audio_path: Option<String>,
    pub summary_preset: String,
    pub folder_id: Option<String>,
    // Optional Client tag (issue #43). NULL = untagged. Independent of
    // `folder_id` — a Note can have any combination of the two.
    #[serde(default)]
    pub client_id: Option<String>,
    // Per-note transcription language. Empty string means "fall back to the
    // global language setting" — that's how pre-feature notes are handled
    // without a backfill migration.
    pub language: String,
    // Per-note summary provider override. Empty string means "fall back
    // to the global summary_provider setting" (same convention as `language`).
    // Populated values are "openai" or "local".
    pub summary_provider: String,
    // Optional speaker count hint, passed through to the offline diarizer
    // as `OfflineDiarizerConfig.withSpeakers(exactly: N)`. `None` (or 0)
    // means "let VBx auto-detect" — the default for fresh notes. A positive
    // value pins the cluster count, which is the most reliable fix for
    // dominant-speaker conversations where auto-detect collapses to 1.
    pub expected_speakers: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
    // Cloud sync: the PocketBase user id of the note's creator. Empty for
    // local-only / pre-sync notes; populated from the server on pull. Used for
    // "created by" attribution in shared workspaces. Preserved across edits
    // (the syncing user only becomes owner when creating, never by editing).
    #[serde(default)]
    pub owner: String,
    // Cloud sync: which workspace this note belongs to (PocketBase workspace
    // id). Empty = Personal / local-only. Note lists are scoped to the active
    // workspace by this field.
    #[serde(default)]
    pub workspace_id: String,
    // Soft-delete timestamp (ms). NULL = live; set = in Trash (recoverable).
    // Deleting a note sets this instead of dropping the row, so an accidental
    // delete can be restored; a remote tombstone also lands here.
    #[serde(default)]
    pub deleted_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Folder {
    pub id: String,
    pub name: String,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default)]
    pub workspace_id: String,
}

/// A Client (issue #43): who the other side of a meeting is. Same shape as
/// `Folder` — the proven sync-ready row (UUID id, name, LWW `updated_at`,
/// workspace scope). No local `deleted_at`; see the `clients` table comment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Client {
    pub id: String,
    pub name: String,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default)]
    pub workspace_id: String,
}

pub fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch(
        r#"
        PRAGMA journal_mode=WAL;
        PRAGMA foreign_keys=ON;

        CREATE TABLE IF NOT EXISTS notes (
            id              TEXT PRIMARY KEY,
            title           TEXT NOT NULL DEFAULT '',
            body            TEXT NOT NULL DEFAULT '',
            transcript      TEXT NOT NULL DEFAULT '',
            summary         TEXT NOT NULL DEFAULT '',
            audio_path      TEXT,
            summary_preset  TEXT NOT NULL DEFAULT 'meeting',
            folder_id       TEXT,
            language        TEXT NOT NULL DEFAULT '',
            created_at      INTEGER NOT NULL,
            updated_at      INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_notes_updated ON notes(updated_at DESC);

        CREATE TABLE IF NOT EXISTS folders (
            id          TEXT PRIMARY KEY,
            name        TEXT NOT NULL,
            created_at  INTEGER NOT NULL,
            updated_at  INTEGER NOT NULL
        );

        -- Client (issue #43): the real-world business relationship a Note is
        -- optionally about, distinct from Folder. Mirrors `folders` exactly —
        -- the proven sync-ready shape. Deliberately no local `deleted_at`:
        -- deletion is a local hard-delete + un-tag of notes, with the remote
        -- tombstone written by the sync outbox in the Client cloud-sync slice
        -- (#49), so no local migration is needed when sync lands.
        CREATE TABLE IF NOT EXISTS clients (
            id           TEXT PRIMARY KEY,
            name         TEXT NOT NULL,
            created_at   INTEGER NOT NULL,
            updated_at   INTEGER NOT NULL,
            workspace_id TEXT NOT NULL DEFAULT ''
        );

        CREATE TABLE IF NOT EXISTS settings (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS summary_prompts (
            id          TEXT PRIMARY KEY,
            name        TEXT NOT NULL,
            content     TEXT NOT NULL,
            created_at  INTEGER NOT NULL,
            updated_at  INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_summary_prompts_updated
            ON summary_prompts(updated_at DESC);

        CREATE TABLE IF NOT EXISTS note_revisions (
            id          TEXT PRIMARY KEY,
            note_id     TEXT NOT NULL,
            title       TEXT NOT NULL DEFAULT '',
            body        TEXT NOT NULL DEFAULT '',
            transcript  TEXT NOT NULL DEFAULT '',
            summary     TEXT NOT NULL DEFAULT '',
            created_at  INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_note_revisions_note
            ON note_revisions(note_id, created_at DESC);

        -- AI chat (issue #46). opencode's v2 model: a conversation row plus one
        -- messages row per message whose `content` is a typed JSON parts array
        -- ordered by a monotonic `seq` (no separate parts table). The
        -- conversation carries general scope/scope_id/tenant fields so later
        -- breadths (folder-, client-, workspace-scoped chats) and tenants add
        -- rows with no reshape. Slice-3 uses only scope='note' + tenant='personal'.
        CREATE TABLE IF NOT EXISTS conversations (
            id          TEXT PRIMARY KEY,
            scope       TEXT NOT NULL,
            scope_id    TEXT NOT NULL,
            tenant      TEXT NOT NULL,
            created_at  INTEGER NOT NULL,
            updated_at  INTEGER NOT NULL
        );
        -- One conversation per (tenant, scope, scope_id) — e.g. one Personal
        -- chat per Note. The get-or-create path relies on this uniqueness.
        CREATE UNIQUE INDEX IF NOT EXISTS idx_conversations_scope
            ON conversations(tenant, scope, scope_id);

        CREATE TABLE IF NOT EXISTS messages (
            id              TEXT PRIMARY KEY,
            conversation_id TEXT NOT NULL,
            seq             INTEGER NOT NULL,
            role            TEXT NOT NULL,
            content         TEXT NOT NULL,
            created_at      INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_messages_conversation
            ON messages(conversation_id, seq);
        "#,
    )?;
    // Idempotent migrations for older schemas. ALTER TABLE adds columns
    // that didn't exist in earlier versions; if they already exist, the
    // execute fails and we ignore.
    let _ = conn.execute(
        "ALTER TABLE notes ADD COLUMN summary_preset TEXT NOT NULL DEFAULT 'meeting'",
        [],
    );
    let _ = conn.execute("ALTER TABLE notes ADD COLUMN folder_id TEXT", []);
    // Client link (issue #43). Soft id reference, same shape as folder_id:
    // NULL = untagged. No FK constraint so a client hard-delete just leaves
    // dangling-free NULLs (delete_client un-tags first anyway).
    let _ = conn.execute("ALTER TABLE notes ADD COLUMN client_id TEXT", []);
    let _ = conn.execute(
        "ALTER TABLE notes ADD COLUMN language TEXT NOT NULL DEFAULT ''",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE notes ADD COLUMN summary_provider TEXT NOT NULL DEFAULT ''",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE notes ADD COLUMN expected_speakers INTEGER",
        [],
    );
    // Cloud sync: note creator (PocketBase user id). Empty for local/pre-sync
    // notes; populated from the server on pull.
    let _ = conn.execute(
        "ALTER TABLE notes ADD COLUMN owner TEXT NOT NULL DEFAULT ''",
        [],
    );
    // Cloud sync: which workspace a row belongs to (PocketBase workspace id).
    // Empty string = Personal / local-only (never synced). Existing rows
    // default to '' so nothing pre-sync silently uploads to a workspace.
    // Reads (note/folder lists) are scoped to the active workspace by this
    // column; the sync worker pushes each row to its OWN workspace.
    let _ = conn.execute(
        "ALTER TABLE notes ADD COLUMN workspace_id TEXT NOT NULL DEFAULT ''",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE folders ADD COLUMN workspace_id TEXT NOT NULL DEFAULT ''",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE summary_prompts ADD COLUMN workspace_id TEXT NOT NULL DEFAULT ''",
        [],
    );
    // Soft-delete (Trash). NULL = live, set (ms) = trashed. Deleting sets this
    // instead of dropping the row so it's recoverable; remote tombstones land
    // here too. Note lists filter `deleted_at IS NULL`.
    let _ = conn.execute("ALTER TABLE notes ADD COLUMN deleted_at INTEGER", []);
    // Index is created AFTER the ALTERs so it's safe on both fresh DBs and
    // older DBs that needed the column added.
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_notes_folder ON notes(folder_id)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_notes_client ON notes(client_id)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_notes_workspace ON notes(workspace_id)",
        [],
    )?;
    Ok(conn)
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

// NOTE: `client_id` is appended last (issue #43). It's selected here and set
// only via `set_note_client` — never in an INSERT column list, so create/apply
// paths default it to NULL safely. When Client cloud-sync lands (#49), the
// remote-note apply path must include `client_id` in its upsert or a pulled
// note would clobber a local tag.
const NOTE_COLS: &str = "id, title, body, transcript, summary, audio_path, summary_preset, folder_id, language, summary_provider, expected_speakers, created_at, updated_at, owner, workspace_id, deleted_at, client_id";

/// List live notes in the active workspace (`""` = Personal / local-only).
/// Excludes trashed notes (`deleted_at` set). Scoping by workspace keeps one
/// workspace's notes out of another's view and Personal out of a shared view.
pub fn list_notes(conn: &Connection, workspace: &str) -> Result<Vec<Note>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {NOTE_COLS} FROM notes WHERE workspace_id = ?1 AND deleted_at IS NULL ORDER BY updated_at DESC"
    ))?;
    let rows = stmt
        .query_map(params![workspace], map_note)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// List trashed notes in the active workspace, most-recently-deleted first.
pub fn list_trashed_notes(conn: &Connection, workspace: &str) -> Result<Vec<Note>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {NOTE_COLS} FROM notes WHERE workspace_id = ?1 AND deleted_at IS NOT NULL ORDER BY deleted_at DESC"
    ))?;
    let rows = stmt
        .query_map(params![workspace], map_note)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_note(conn: &Connection, id: &str) -> Result<Note> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {NOTE_COLS} FROM notes WHERE id = ?1"
    ))?;
    let n = stmt.query_row(params![id], map_note)?;
    Ok(n)
}

pub fn create_note(
    conn: &Connection,
    default_language: &str,
    default_preset: &str,
    workspace: &str,
) -> Result<Note> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_ms();
    conn.execute(
        "INSERT INTO notes (id, title, body, transcript, summary, audio_path, summary_preset, folder_id, language, summary_provider, expected_speakers, created_at, updated_at, workspace_id)
         VALUES (?1, '', '', '', '', NULL, ?2, NULL, ?3, '', NULL, ?4, ?4, ?5)",
        params![id, default_preset, default_language, now, workspace],
    )?;
    get_note(conn, &id)
}

pub fn move_note(conn: &Connection, id: &str, folder_id: Option<&str>) -> Result<()> {
    let now = now_ms();
    conn.execute(
        "UPDATE notes SET folder_id = ?1, updated_at = ?2 WHERE id = ?3",
        params![folder_id, now, id],
    )?;
    Ok(())
}

/// Reassign a note to a different workspace (`""` = Personal/local-only).
///
/// Deliberately does NOT bump `updated_at`: a workspace move is not a content
/// edit, so the note keeps its place in the activity-sorted lists instead of
/// jumping to "Today". The sync layer still propagates the move (an explicit
/// tombstone-in-old + upsert-in-new), and the push is resurrect-aware
/// (`cloud_sync`'s `push_note`) so moving a note back into a workspace still
/// wins last-write-wins over an earlier tombstone without a bumped timestamp.
pub fn set_note_workspace(conn: &Connection, id: &str, workspace: &str) -> Result<()> {
    conn.execute(
        "UPDATE notes SET workspace_id = ?1 WHERE id = ?2",
        params![workspace, id],
    )?;
    Ok(())
}

pub fn list_folders(conn: &Connection, workspace: &str) -> Result<Vec<Folder>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, name, created_at, updated_at, workspace_id FROM folders WHERE workspace_id = ?1 ORDER BY name COLLATE NOCASE",
    )?;
    let rows = stmt
        .query_map(params![workspace], map_folder)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn create_folder(conn: &Connection, name: &str, workspace: &str) -> Result<Folder> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_ms();
    conn.execute(
        "INSERT INTO folders (id, name, created_at, updated_at, workspace_id) VALUES (?1, ?2, ?3, ?3, ?4)",
        params![id, name, now, workspace],
    )?;
    conn.query_row(
        "SELECT id, name, created_at, updated_at, workspace_id FROM folders WHERE id = ?1",
        params![id],
        map_folder,
    )
    .map_err(Into::into)
}

pub fn rename_folder(conn: &Connection, id: &str, name: &str) -> Result<()> {
    let now = now_ms();
    conn.execute(
        "UPDATE folders SET name = ?1, updated_at = ?2 WHERE id = ?3",
        params![name, now, id],
    )?;
    Ok(())
}

/// Delete a folder. Its notes fall back to root (`folder_id = NULL`) rather
/// than being deleted; their `updated_at` is bumped so a sync layer re-pushes
/// them. Returns the ids of the reparented notes so the caller can notify sync.
pub fn delete_folder(conn: &Connection, id: &str) -> Result<Vec<String>> {
    let now = now_ms();
    let reparented: Vec<String> = {
        let mut stmt = conn.prepare("SELECT id FROM notes WHERE folder_id = ?1")?;
        let rows = stmt
            .query_map(params![id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    conn.execute(
        "UPDATE notes SET folder_id = NULL, updated_at = ?2 WHERE folder_id = ?1",
        params![id, now],
    )?;
    conn.execute("DELETE FROM folders WHERE id = ?1", params![id])?;
    Ok(reparented)
}

fn map_folder(row: &rusqlite::Row) -> rusqlite::Result<Folder> {
    Ok(Folder {
        id: row.get(0)?,
        name: row.get(1)?,
        created_at: row.get(2)?,
        updated_at: row.get(3)?,
        workspace_id: row.get(4)?,
    })
}

// ── Clients (issue #43) ─────────────────────────────────────────────────────
// A direct mirror of the folder functions; kept separate (rather than a shared
// generic) so the two entities can diverge later without untangling one path.

pub fn list_clients(conn: &Connection, workspace: &str) -> Result<Vec<Client>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, name, created_at, updated_at, workspace_id FROM clients WHERE workspace_id = ?1 ORDER BY name COLLATE NOCASE",
    )?;
    let rows = stmt
        .query_map(params![workspace], map_client)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn create_client(conn: &Connection, name: &str, workspace: &str) -> Result<Client> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_ms();
    conn.execute(
        "INSERT INTO clients (id, name, created_at, updated_at, workspace_id) VALUES (?1, ?2, ?3, ?3, ?4)",
        params![id, name, now, workspace],
    )?;
    conn.query_row(
        "SELECT id, name, created_at, updated_at, workspace_id FROM clients WHERE id = ?1",
        params![id],
        map_client,
    )
    .map_err(Into::into)
}

pub fn rename_client(conn: &Connection, id: &str, name: &str) -> Result<()> {
    let now = now_ms();
    conn.execute(
        "UPDATE clients SET name = ?1, updated_at = ?2 WHERE id = ?3",
        params![name, now, id],
    )?;
    Ok(())
}

/// Delete a Client. Its notes fall back to no Client (`client_id = NULL`)
/// rather than being deleted; their `updated_at` is bumped so a sync layer
/// re-pushes them. Returns the ids of the un-tagged notes so the caller can
/// notify sync. Mirrors `delete_folder`.
pub fn delete_client(conn: &Connection, id: &str) -> Result<Vec<String>> {
    let now = now_ms();
    let untagged: Vec<String> = {
        let mut stmt = conn.prepare("SELECT id FROM notes WHERE client_id = ?1")?;
        let rows = stmt
            .query_map(params![id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    conn.execute(
        "UPDATE notes SET client_id = NULL, updated_at = ?2 WHERE client_id = ?1",
        params![id, now],
    )?;
    conn.execute("DELETE FROM clients WHERE id = ?1", params![id])?;
    Ok(untagged)
}

/// Assign or clear a note's Client. `None` writes SQL NULL (untag),
/// `Some(id)` assigns. Mirrors `move_note` for folders.
pub fn set_note_client(conn: &Connection, id: &str, client_id: Option<&str>) -> Result<()> {
    let now = now_ms();
    conn.execute(
        "UPDATE notes SET client_id = ?1, updated_at = ?2 WHERE id = ?3",
        params![client_id, now, id],
    )?;
    Ok(())
}

fn map_client(row: &rusqlite::Row) -> rusqlite::Result<Client> {
    Ok(Client {
        id: row.get(0)?,
        name: row.get(1)?,
        created_at: row.get(2)?,
        updated_at: row.get(3)?,
        workspace_id: row.get(4)?,
    })
}

#[derive(Debug, Default, Deserialize)]
pub struct NotePatch {
    pub title: Option<String>,
    pub body: Option<String>,
    pub transcript: Option<String>,
    pub summary: Option<String>,
    pub summary_preset: Option<String>,
    pub language: Option<String>,
    // Empty string clears the override. Same pattern as `language`.
    pub summary_provider: Option<String>,
    // `Some(Some(n))` writes a hint, `Some(None)` clears it back to
    // auto-detect, `None` leaves the existing value untouched. The double
    // `Option` is intentional — the outer one says "is the patch touching
    // this field?", the inner one says "what value to write?".
    #[serde(default, deserialize_with = "deserialize_optional_optional")]
    pub expected_speakers: Option<Option<i64>>,
}

/// Custom deserializer so the JSON shapes `{}`, `{"expectedSpeakers": null}`,
/// and `{"expectedSpeakers": 2}` map to `None`, `Some(None)`, and
/// `Some(Some(2))` respectively. Without this, serde collapses null and
/// missing into the same `None` and we lose the "clear the hint" signal.
fn deserialize_optional_optional<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Option<i64>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<i64>::deserialize(deserializer).map(Some)
}

pub fn update_note(conn: &Connection, id: &str, patch: &NotePatch) -> Result<()> {
    let now = now_ms();
    // Snapshot the pre-edit content for version history, but only when a content
    // field (title/body/transcript/summary) actually changes — skip no-op saves.
    // Best-effort: history must never break a save.
    let content_changes = conn
        .query_row(
            "SELECT title, body, transcript, summary FROM notes WHERE id = ?1",
            params![id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            },
        )
        .optional()
        .ok()
        .flatten()
        .map(|(t, b, tr, s)| {
            patch.title.as_ref().is_some_and(|v| v != &t)
                || patch.body.as_ref().is_some_and(|v| v != &b)
                || patch.transcript.as_ref().is_some_and(|v| v != &tr)
                || patch.summary.as_ref().is_some_and(|v| v != &s)
        })
        .unwrap_or(false);
    if content_changes {
        let _ = snapshot_revision(conn, id);
    }
    if let Some(t) = &patch.title {
        conn.execute("UPDATE notes SET title = ?1, updated_at = ?2 WHERE id = ?3", params![t, now, id])?;
    }
    if let Some(b) = &patch.body {
        conn.execute("UPDATE notes SET body = ?1, updated_at = ?2 WHERE id = ?3", params![b, now, id])?;
    }
    if let Some(t) = &patch.transcript {
        conn.execute("UPDATE notes SET transcript = ?1, updated_at = ?2 WHERE id = ?3", params![t, now, id])?;
    }
    if let Some(s) = &patch.summary {
        conn.execute("UPDATE notes SET summary = ?1, updated_at = ?2 WHERE id = ?3", params![s, now, id])?;
    }
    if let Some(p) = &patch.summary_preset {
        conn.execute("UPDATE notes SET summary_preset = ?1, updated_at = ?2 WHERE id = ?3", params![p, now, id])?;
    }
    if let Some(l) = &patch.language {
        conn.execute("UPDATE notes SET language = ?1, updated_at = ?2 WHERE id = ?3", params![l, now, id])?;
    }
    if let Some(sp) = &patch.summary_provider {
        conn.execute(
            "UPDATE notes SET summary_provider = ?1, updated_at = ?2 WHERE id = ?3",
            params![sp, now, id],
        )?;
    }
    if let Some(es) = &patch.expected_speakers {
        // Inner `None` writes SQL NULL (clears the hint back to auto). Inner
        // `Some(n)` writes the speaker count. `params![]` resolves both via
        // `ToSql` for `Option<i64>`.
        conn.execute(
            "UPDATE notes SET expected_speakers = ?1, updated_at = ?2 WHERE id = ?3",
            params![es, now, id],
        )?;
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoteRevision {
    pub id: String,
    pub note_id: String,
    pub title: String,
    pub body: String,
    pub transcript: String,
    pub summary: String,
    pub created_at: i64,
}

/// Save a snapshot of the note's CURRENT content into `note_revisions`, deduped
/// against the latest snapshot and capped at the newest 30 per note. Called
/// before an edit applies (so it captures the pre-edit state) and before a
/// restore (so the restore is itself undoable).
fn snapshot_revision(conn: &Connection, note_id: &str) -> Result<()> {
    let cur: Option<(String, String, String, String)> = conn
        .query_row(
            "SELECT title, body, transcript, summary FROM notes WHERE id = ?1",
            params![note_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some(cur) = cur else { return Ok(()) };
    let latest: Option<(String, String, String, String)> = conn
        .query_row(
            "SELECT title, body, transcript, summary FROM note_revisions
             WHERE note_id = ?1 ORDER BY created_at DESC, rowid DESC LIMIT 1",
            params![note_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    if latest.as_ref() == Some(&cur) {
        return Ok(()); // no-op save — nothing new to record
    }
    conn.execute(
        "INSERT INTO note_revisions (id, note_id, title, body, transcript, summary, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![uuid::Uuid::new_v4().to_string(), note_id, cur.0, cur.1, cur.2, cur.3, now_ms()],
    )?;
    conn.execute(
        "DELETE FROM note_revisions WHERE note_id = ?1 AND id NOT IN
           (SELECT id FROM note_revisions WHERE note_id = ?1 ORDER BY created_at DESC, rowid DESC LIMIT 30)",
        params![note_id],
    )?;
    Ok(())
}

pub fn list_note_revisions(conn: &Connection, note_id: &str) -> Result<Vec<NoteRevision>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, note_id, title, body, transcript, summary, created_at FROM note_revisions
         WHERE note_id = ?1 ORDER BY created_at DESC, rowid DESC",
    )?;
    let rows = stmt
        .query_map(params![note_id], |r| {
            Ok(NoteRevision {
                id: r.get(0)?,
                note_id: r.get(1)?,
                title: r.get(2)?,
                body: r.get(3)?,
                transcript: r.get(4)?,
                summary: r.get(5)?,
                created_at: r.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Restore a note to a saved revision's content. Snapshots the current state
/// first so the restore is itself undoable, then applies the revision and bumps
/// `updated_at` (so the restored content syncs).
pub fn restore_note_revision(conn: &Connection, note_id: &str, revision_id: &str) -> Result<()> {
    let rev: Option<(String, String, String, String)> = conn
        .query_row(
            "SELECT title, body, transcript, summary FROM note_revisions WHERE id = ?1 AND note_id = ?2",
            params![revision_id, note_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some(rev) = rev else {
        return Err(anyhow::anyhow!("revision not found"));
    };
    let _ = snapshot_revision(conn, note_id); // make the restore undoable
    conn.execute(
        "UPDATE notes SET title = ?1, body = ?2, transcript = ?3, summary = ?4, updated_at = ?5 WHERE id = ?6",
        params![rev.0, rev.1, rev.2, rev.3, now_ms(), note_id],
    )?;
    Ok(())
}

/// Append `text` to the note's transcript, inserting `separator` between
/// the existing text and the new content. Caller decides the separator —
/// " " for same-speaker continuation, "\n" for a speaker switch, "" when
/// the existing transcript is empty.
pub fn append_transcript(conn: &Connection, id: &str, text: &str, separator: &str) -> Result<String> {
    // Hot path — called once per chunk during recording. Cache both the
    // read and the write to avoid re-parsing the SQL each time.
    let mut current: String = {
        let mut stmt = conn.prepare_cached("SELECT transcript FROM notes WHERE id = ?1")?;
        stmt.query_row(params![id], |row| row.get(0))?
    };
    if !current.is_empty() {
        current.push_str(separator);
    }
    current.push_str(text);
    let now = now_ms();
    let mut stmt = conn.prepare_cached(
        "UPDATE notes SET transcript = ?1, updated_at = ?2 WHERE id = ?3",
    )?;
    stmt.execute(params![current, now, id])?;
    Ok(current)
}

/// Replace the note's transcript with `text`. Used by the offline
/// diarization step to rewrite a chunk-by-chunk transcript with
/// `Speaker N:` prefixes once the full audio has been clustered.
pub fn set_transcript(conn: &Connection, id: &str, text: &str) -> Result<()> {
    let now = now_ms();
    // Same SQL string as the transcript branch of update_note and the
    // tail of append_transcript — they share a single cached statement.
    let mut stmt = conn.prepare_cached(
        "UPDATE notes SET transcript = ?1, updated_at = ?2 WHERE id = ?3",
    )?;
    stmt.execute(params![text, now, id])?;
    Ok(())
}

/// Soft-delete: move a note to the Trash (recoverable) and bump `updated_at` so
/// the change syncs (as a tombstone). The row is kept so it can be restored.
pub fn delete_note(conn: &Connection, id: &str) -> Result<()> {
    let now = now_ms();
    conn.execute(
        "UPDATE notes SET deleted_at = ?1, updated_at = ?1 WHERE id = ?2",
        params![now, id],
    )?;
    Ok(())
}

/// Restore a note from the Trash. Bumps `updated_at` so a re-pushed (un-deleted)
/// version wins LWW and the note reappears for teammates too.
pub fn restore_note(conn: &Connection, id: &str) -> Result<()> {
    let now = now_ms();
    conn.execute(
        "UPDATE notes SET deleted_at = NULL, updated_at = ?1 WHERE id = ?2",
        params![now, id],
    )?;
    Ok(())
}

/// Permanently delete a note (hard DELETE) — removes the local row for good.
/// The server copy is already tombstoned from the soft-delete.
pub fn purge_note(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("DELETE FROM notes WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn get_setting(conn: &Connection, key: &str) -> Result<Option<String>> {
    // Hot path — called ~7 times per chunk inside transcribe_chunk's cfg
    // block. prepare_cached reuses the same prepared statement instead of
    // re-parsing the SQL on every call.
    let mut stmt = conn.prepare_cached("SELECT value FROM settings WHERE key = ?1")?;
    let v: rusqlite::Result<String> = stmt.query_row(params![key], |row| row.get(0));
    match v {
        Ok(s) => Ok(Some(s)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn set_setting(conn: &Connection, key: &str, value: &str) -> Result<()> {
    let mut stmt = conn.prepare_cached(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )?;
    stmt.execute(params![key, value])?;
    Ok(())
}

pub fn delete_setting(conn: &Connection, key: &str) -> Result<()> {
    let mut stmt = conn.prepare_cached("DELETE FROM settings WHERE key = ?1")?;
    stmt.execute(params![key])?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryPrompt {
    pub id: String,
    pub name: String,
    pub content: String,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default)]
    pub workspace_id: String,
}

// Prompts are scoped to the active workspace PLUS your personal ('') ones:
// personal prompts are always available as your reusable toolkit, while a
// workspace's shared prompts only appear in that workspace (so a teammate's
// custom prompt doesn't leak into everyone else's list). Each carries a
// workspace_id so the sync worker pushes it to its owning workspace.
pub fn list_summary_prompts(conn: &Connection, workspace: &str) -> Result<Vec<SummaryPrompt>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, name, content, created_at, updated_at, workspace_id FROM summary_prompts
         WHERE workspace_id = ?1 OR workspace_id = ''
         ORDER BY name COLLATE NOCASE",
    )?;
    let rows = stmt
        .query_map(params![workspace], map_summary_prompt)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn get_summary_prompt(conn: &Connection, id: &str) -> Result<SummaryPrompt> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, name, content, created_at, updated_at, workspace_id FROM summary_prompts WHERE id = ?1",
    )?;
    let p = stmt.query_row(params![id], map_summary_prompt)?;
    Ok(p)
}

pub fn create_summary_prompt(
    conn: &Connection,
    name: &str,
    content: &str,
    workspace: &str,
) -> Result<SummaryPrompt> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_ms();
    conn.execute(
        "INSERT INTO summary_prompts (id, name, content, created_at, updated_at, workspace_id)
         VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
        params![id, name, content, now, workspace],
    )?;
    get_summary_prompt(conn, &id)
}

pub fn update_summary_prompt(
    conn: &Connection,
    id: &str,
    name: &str,
    content: &str,
) -> Result<SummaryPrompt> {
    let now = now_ms();
    conn.execute(
        "UPDATE summary_prompts SET name = ?1, content = ?2, updated_at = ?3 WHERE id = ?4",
        params![name, content, now, id],
    )?;
    get_summary_prompt(conn, id)
}

pub fn delete_summary_prompt(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("DELETE FROM summary_prompts WHERE id = ?1", params![id])?;
    Ok(())
}

fn map_summary_prompt(row: &rusqlite::Row) -> rusqlite::Result<SummaryPrompt> {
    Ok(SummaryPrompt {
        id: row.get(0)?,
        name: row.get(1)?,
        content: row.get(2)?,
        created_at: row.get(3)?,
        updated_at: row.get(4)?,
        workspace_id: row.get(5)?,
    })
}

// ── AI chat (issue #46) ────────────────────────────────────────────────────
// Persistence for the chat skeleton: conversations + messages. See the schema
// note in `open()` for the storage-shape rationale (opencode v2 model).

/// General chat scope. Only `Note` is used in this slice; the enum exists so
/// the string stored in `conversations.scope` is chosen in one place and later
/// breadths (folder/client/workspace) add variants without touching call sites.
pub const CHAT_SCOPE_NOTE: &str = "note";
/// Only Personal is used in this slice; workspace tenants arrive with Teams.
pub const CHAT_TENANT_PERSONAL: &str = "personal";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub id: String,
    pub scope: String,
    pub scope_id: String,
    pub tenant: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// One persisted message. `content` is the raw JSON parts array — the chat
/// module owns the typed `Part` shape and (de)serialises it; db.rs stays
/// agnostic and just stores the string, ordered within a conversation by `seq`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: String,
    pub conversation_id: String,
    pub seq: i64,
    pub role: String,
    pub content: String,
    pub created_at: i64,
}

fn map_conversation(row: &rusqlite::Row) -> rusqlite::Result<Conversation> {
    Ok(Conversation {
        id: row.get(0)?,
        scope: row.get(1)?,
        scope_id: row.get(2)?,
        tenant: row.get(3)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
    })
}

const CONVERSATION_COLS: &str = "id, scope, scope_id, tenant, created_at, updated_at";

/// The existing conversation for a scope, or None. Used to reload history
/// without creating an empty conversation just for opening the Chat tab.
pub fn get_conversation(
    conn: &Connection,
    tenant: &str,
    scope: &str,
    scope_id: &str,
) -> Result<Option<Conversation>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {CONVERSATION_COLS} FROM conversations
         WHERE tenant = ?1 AND scope = ?2 AND scope_id = ?3",
    ))?;
    match stmt.query_row(params![tenant, scope, scope_id], map_conversation) {
        Ok(c) => Ok(Some(c)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Get the conversation for a scope, creating it lazily if absent. The unique
/// index on (tenant, scope, scope_id) makes this the single conversation for a
/// Note — there is no conversation-list / "new chat" concept in this slice.
pub fn get_or_create_conversation(
    conn: &Connection,
    tenant: &str,
    scope: &str,
    scope_id: &str,
) -> Result<Conversation> {
    if let Some(c) = get_conversation(conn, tenant, scope, scope_id)? {
        return Ok(c);
    }
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_ms();
    conn.execute(
        "INSERT INTO conversations (id, scope, scope_id, tenant, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
        params![id, scope, scope_id, tenant, now],
    )?;
    get_conversation(conn, tenant, scope, scope_id)?
        .ok_or_else(|| anyhow::anyhow!("conversation vanished after insert"))
}

fn map_chat_message(row: &rusqlite::Row) -> rusqlite::Result<ChatMessage> {
    Ok(ChatMessage {
        id: row.get(0)?,
        conversation_id: row.get(1)?,
        seq: row.get(2)?,
        role: row.get(3)?,
        content: row.get(4)?,
        created_at: row.get(5)?,
    })
}

const CHAT_MESSAGE_COLS: &str = "id, conversation_id, seq, role, content, created_at";

/// The next monotonic `seq` for a conversation (max + 1, or 0 for the first
/// message). Ordering is by `seq`, never by `created_at`, so two messages
/// written in the same millisecond still have a stable order.
pub fn next_message_seq(conn: &Connection, conversation_id: &str) -> Result<i64> {
    let mut stmt = conn
        .prepare_cached("SELECT COALESCE(MAX(seq) + 1, 0) FROM messages WHERE conversation_id = ?1")?;
    let seq: i64 = stmt.query_row(params![conversation_id], |r| r.get(0))?;
    Ok(seq)
}

/// Append a message with the given raw parts JSON. Also bumps the parent
/// conversation's `updated_at` so recency ordering (later breadths) is cheap.
pub fn insert_chat_message(
    conn: &Connection,
    conversation_id: &str,
    role: &str,
    content: &str,
) -> Result<ChatMessage> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_ms();
    let seq = next_message_seq(conn, conversation_id)?;
    conn.execute(
        "INSERT INTO messages (id, conversation_id, seq, role, content, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, conversation_id, seq, role, content, now],
    )?;
    conn.execute(
        "UPDATE conversations SET updated_at = ?1 WHERE id = ?2",
        params![now, conversation_id],
    )?;
    get_chat_message(conn, &id)
}

pub fn get_chat_message(conn: &Connection, id: &str) -> Result<ChatMessage> {
    let mut stmt = conn
        .prepare_cached(&format!("SELECT {CHAT_MESSAGE_COLS} FROM messages WHERE id = ?1"))?;
    Ok(stmt.query_row(params![id], map_chat_message)?)
}

/// Overwrite a message's parts JSON. Used to finalise an assistant message
/// once its streamed text is complete (the row is created empty so its id can
/// ride the streaming deltas).
pub fn update_chat_message_content(conn: &Connection, id: &str, content: &str) -> Result<()> {
    conn.execute(
        "UPDATE messages SET content = ?1 WHERE id = ?2",
        params![content, id],
    )?;
    Ok(())
}

/// Drop a message. Used to roll back an assistant row whose stream errored, so
/// reloaded history never shows an empty half-written turn.
pub fn delete_chat_message(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("DELETE FROM messages WHERE id = ?1", params![id])?;
    Ok(())
}

/// All messages in a conversation, oldest first (by `seq`).
pub fn list_chat_messages(conn: &Connection, conversation_id: &str) -> Result<Vec<ChatMessage>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {CHAT_MESSAGE_COLS} FROM messages WHERE conversation_id = ?1 ORDER BY seq",
    ))?;
    let rows = stmt
        .query_map(params![conversation_id], map_chat_message)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// One-time migration: pull the legacy single `summary_prompt` setting
/// into a row in `summary_prompts`, then rewrite any note whose preset
/// is the literal `"custom"` to `"custom:<new-id>"` so it points at
/// that row. Idempotent — guarded by the `summary_prompts_migrated`
/// setting flag, which we set once the migration completes (or
/// trivially completes when the legacy setting is empty).
///
/// We deliberately leave the legacy `summary_prompt` setting in place
/// instead of clearing it. Rolling back to an older app version would
/// otherwise lose the custom prompt entirely; keeping it around costs
/// nothing.
pub fn migrate_summary_prompts(conn: &Connection) -> Result<()> {
    let already = get_setting(conn, "summary_prompts_migrated")?.unwrap_or_default();
    if already == "true" {
        return Ok(());
    }
    let legacy = get_setting(conn, "summary_prompt")?.unwrap_or_default();
    if legacy.trim().is_empty() {
        // Nothing to migrate, but mark it done so we don't re-check on
        // every launch.
        set_setting(conn, "summary_prompts_migrated", "true")?;
        return Ok(());
    }
    let row = create_summary_prompt(conn, "Custom prompt (migrated)", &legacy, "")?;
    let new_value = format!("custom:{}", row.id);
    conn.execute(
        "UPDATE notes SET summary_preset = ?1 WHERE summary_preset = 'custom'",
        params![new_value],
    )?;
    set_setting(conn, "summary_prompts_migrated", "true")?;
    Ok(())
}

/// One-shot v0.23 migration: ensure `transcribe_config` is present (build
/// from legacy flat keys if missing) and then delete those legacy rows
/// so they can't drift out of sync with the typed config. Idempotent —
/// guarded by a flag in the settings table so re-running the app is a
/// no-op after the first successful run.
pub fn migrate_transcribe_config(conn: &Connection) -> Result<()> {
    const FLAG: &str = "migrated_transcribe_config_v3";
    if get_setting(conn, FLAG)?.as_deref() == Some("true") {
        return Ok(());
    }

    // If transcribe_config is absent, synthesise it from whatever legacy
    // keys exist. v0.22 users already have transcribe_config because the
    // Settings UI was double-writing; this branch covers v0.21 holdouts
    // who upgraded straight to v0.23 without ever opening Settings under
    // v0.22.
    if get_setting(conn, "transcribe_config")?.is_none() {
        let provider = get_setting(conn, "transcribe_provider")?;
        let model = get_setting(conn, "transcribe_model")?;
        let whisper_model = get_setting(conn, "local_whisper_model")?;
        let whisper_preset = get_setting(conn, "whisper_preset")?;
        let whisper_use_gpu = get_setting(conn, "local_whisper_use_gpu")?
            .and_then(|v| match v.as_str() {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            });
        let cfg = crate::stt::from_legacy_settings(
            provider.as_deref(),
            model.as_deref(),
            whisper_model.as_deref(),
            whisper_preset.as_deref(),
            whisper_use_gpu,
        );
        let json = serde_json::to_string(&cfg)
            .map_err(|e| anyhow::anyhow!("serialize transcribe_config: {e}"))?;
        set_setting(conn, "transcribe_config", &json)?;
    }

    for key in [
        "transcribe_provider",
        "transcribe_model",
        "whisper_preset",
        "local_whisper_model",
        "local_whisper_use_gpu",
        "deepgram_model",
        "groq_model",
    ] {
        delete_setting(conn, key)?;
    }
    set_setting(conn, FLAG, "true")?;
    Ok(())
}

/// One-shot v0.24 migration: wrap a bare `ProviderConfig` JSON in
/// `transcribe_config` into the new `TranscribeConfig { default,
/// per_language }` shape. Idempotent via the parse-as-TranscribeConfig
/// check — running twice is a no-op because the second pass parses
/// successfully and bails.
///
/// Unlike `migrate_transcribe_config`, this migration doesn't need a
/// flag row: the parse outcome itself encodes whether work is needed.
/// (v0.23 needed a flag because it deleted seven other rows whose
/// absence couldn't reliably distinguish "fresh install" from "already
/// migrated".)
pub fn migrate_per_language_v4(conn: &Connection) -> Result<()> {
    let Some(raw) = get_setting(conn, "transcribe_config")? else {
        // No transcribe_config row at all — fresh install, or v0.21
        // user who hasn't been touched by migrate_transcribe_config
        // yet (it runs first). Either way, nothing to wrap. The
        // read_transcribe_config fallback covers this user when the
        // app reads.
        return Ok(());
    };
    if serde_json::from_str::<crate::stt::TranscribeConfig>(&raw).is_ok() {
        // Already in the new shape — second-or-later run, no-op.
        return Ok(());
    }
    let Ok(legacy) = serde_json::from_str::<crate::stt::ProviderConfig>(&raw) else {
        // Row is neither a TranscribeConfig nor a bare ProviderConfig.
        // Probably a corrupt write. Don't touch it — leave the
        // read_transcribe_config fallback to recover. Caller logs.
        return Err(anyhow::anyhow!(
            "transcribe_config row is neither TranscribeConfig nor ProviderConfig — leaving untouched"
        ));
    };
    let wrapped = crate::stt::TranscribeConfig {
        default: legacy,
        per_language: std::collections::BTreeMap::new(),
    };
    let json = serde_json::to_string(&wrapped)
        .map_err(|e| anyhow::anyhow!("serialize wrapped TranscribeConfig: {e}"))?;
    set_setting(conn, "transcribe_config", &json)?;
    Ok(())
}

/// v0.31 grandfathering: mark existing installs so the first-run onboarding
/// wizard never appears for anyone already using Humla. Writes
/// `onboarding_completed = "true"` when the DB looks lived-in.
///
/// Grandfather predicate (either is sufficient):
///   - any notes exist (`COUNT(*) FROM notes > 0`, trashed included — a
///     trashed note still proves prior use), OR
///   - any local Whisper model file (`*.bin`) is present in `models_dir`.
///
/// Deliberately does **not** read the Keychain / API keys: a Keychain read at
/// startup can trigger a macOS auth prompt (notably in unsigned dev builds),
/// and we must never prompt the user just to decide whether to show a wizard.
/// A cloud-only / API-key-only user with zero notes and no local model is the
/// rare edge that (correctly) gets shown the wizard once — harmless, and every
/// step writes through to live state so nothing they configured is lost.
///
/// Idempotent: short-circuits if `onboarding_completed` is already set (either
/// by a prior run of this migration, or by the wizard itself completing). A
/// genuinely fresh install writes nothing and falls through — the frontend
/// takeover guard sees the unset key and shows the wizard.
pub fn migrate_grandfather_onboarding(conn: &Connection, models_dir: &Path) -> Result<()> {
    if get_setting(conn, "onboarding_completed")?.is_some() {
        return Ok(());
    }

    let note_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM notes", [], |r| r.get(0))?;
    let has_notes = note_count > 0;

    // A downloaded on-device model is stored as a `.bin` file in models_dir.
    // Any such file (regardless of which model) proves the user configured
    // local transcription. Missing dir / read error → treat as "no model".
    let has_local_model = std::fs::read_dir(models_dir)
        .map(|entries| {
            entries.flatten().any(|e| {
                e.path()
                    .extension()
                    .and_then(|x| x.to_str())
                    .map(|x| x.eq_ignore_ascii_case("bin"))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false);

    if has_notes || has_local_model {
        set_setting(conn, "onboarding_completed", "true")?;
    }
    Ok(())
}

fn map_note(row: &rusqlite::Row) -> rusqlite::Result<Note> {
    Ok(Note {
        id: row.get(0)?,
        title: row.get(1)?,
        body: row.get(2)?,
        transcript: row.get(3)?,
        summary: row.get(4)?,
        audio_path: row.get(5)?,
        summary_preset: row.get(6)?,
        folder_id: row.get(7)?,
        language: row.get(8)?,
        summary_provider: row.get(9)?,
        expected_speakers: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
        owner: row.get(13)?,
        workspace_id: row.get(14)?,
        deleted_at: row.get(15)?,
        client_id: row.get(16)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The core P0 guard: a note created in one workspace must not appear when
    /// listing another, and Personal ("") is its own bucket.
    #[test]
    fn list_notes_is_scoped_to_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("scope.sqlite")).unwrap();
        create_note(&conn, "en", "meeting", "wsA").unwrap();
        create_note(&conn, "en", "meeting", "wsA").unwrap();
        create_note(&conn, "en", "meeting", "").unwrap(); // Personal
        create_note(&conn, "en", "meeting", "wsB").unwrap();

        assert_eq!(list_notes(&conn, "wsA").unwrap().len(), 2, "wsA sees only its own");
        assert_eq!(list_notes(&conn, "wsB").unwrap().len(), 1, "wsB sees only its own");
        assert_eq!(list_notes(&conn, "").unwrap().len(), 1, "Personal sees only local");
        // Folders scope the same way.
        create_folder(&conn, "A folder", "wsA").unwrap();
        create_folder(&conn, "Personal folder", "").unwrap();
        assert_eq!(list_folders(&conn, "wsA").unwrap().len(), 1);
        assert_eq!(list_folders(&conn, "").unwrap().len(), 1);
        assert_eq!(list_folders(&conn, "wsB").unwrap().len(), 0);
    }

    #[test]
    fn client_crud_and_note_tagging_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("clients.sqlite")).unwrap();

        // create
        let acme = create_client(&conn, "Acme Inc", "").unwrap();
        let globex = create_client(&conn, "Globex", "").unwrap();
        assert_eq!(list_clients(&conn, "").unwrap().len(), 2);

        let note = create_note(&conn, "en", "meeting", "").unwrap();
        assert_eq!(get_note(&conn, &note.id).unwrap().client_id, None, "new note is untagged");

        // assign
        set_note_client(&conn, &note.id, Some(&acme.id)).unwrap();
        assert_eq!(
            get_note(&conn, &note.id).unwrap().client_id.as_deref(),
            Some(acme.id.as_str())
        );

        // reassign
        set_note_client(&conn, &note.id, Some(&globex.id)).unwrap();
        assert_eq!(
            get_note(&conn, &note.id).unwrap().client_id.as_deref(),
            Some(globex.id.as_str())
        );

        // unassign
        set_note_client(&conn, &note.id, None).unwrap();
        assert_eq!(get_note(&conn, &note.id).unwrap().client_id, None);

        // rename
        rename_client(&conn, &acme.id, "Acme LLC").unwrap();
        let renamed = list_clients(&conn, "").unwrap().into_iter().find(|c| c.id == acme.id).unwrap();
        assert_eq!(renamed.name, "Acme LLC");

        // delete un-tags the client's notes (never deletes them) and returns
        // the affected note ids.
        set_note_client(&conn, &note.id, Some(&globex.id)).unwrap();
        let untagged = delete_client(&conn, &globex.id).unwrap();
        assert_eq!(untagged, vec![note.id.clone()], "delete returns the un-tagged note ids");
        assert!(
            list_clients(&conn, "").unwrap().iter().all(|c| c.id != globex.id),
            "the client row is gone"
        );
        assert!(get_note(&conn, &note.id).is_ok(), "the note survives the client delete");
        assert_eq!(
            get_note(&conn, &note.id).unwrap().client_id,
            None,
            "the note falls back to no client"
        );
    }

    #[test]
    fn list_clients_is_scoped_to_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("client_scope.sqlite")).unwrap();
        create_client(&conn, "WS A client", "wsA").unwrap();
        create_client(&conn, "Personal client", "").unwrap();
        assert_eq!(list_clients(&conn, "wsA").unwrap().len(), 1);
        assert_eq!(list_clients(&conn, "").unwrap().len(), 1);
        assert_eq!(list_clients(&conn, "wsB").unwrap().len(), 0);
    }

    #[test]
    fn note_revisions_snapshot_dedup_and_restore() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("rev.sqlite")).unwrap();
        let id = create_note(&conn, "en", "meeting", "").unwrap().id;

        let title = |t: &str| NotePatch { title: Some(t.into()), ..Default::default() };
        update_note(&conn, &id, &title("v1")).unwrap(); // snapshots the empty pre-edit state
        update_note(&conn, &id, &title("v2")).unwrap(); // snapshots "v1"
        update_note(&conn, &id, &title("v2")).unwrap(); // no content change → deduped

        let revs = list_note_revisions(&conn, &id).unwrap();
        assert_eq!(revs.len(), 2, "two distinct prior states, no dup for the no-op save");
        assert_eq!(revs[0].title, "v1", "newest revision is the most recent prior state");
        assert_eq!(revs[1].title, "", "oldest revision is the initial empty state");

        // A non-content edit (language) must not create a revision.
        update_note(&conn, &id, &NotePatch { language: Some("no".into()), ..Default::default() }).unwrap();
        assert_eq!(list_note_revisions(&conn, &id).unwrap().len(), 2, "non-content edit doesn't snapshot");

        // Restore to the initial empty state; the current "v2" is snapshotted so
        // the restore is undoable.
        let oldest = list_note_revisions(&conn, &id).unwrap().last().unwrap().id.clone();
        restore_note_revision(&conn, &id, &oldest).unwrap();
        assert_eq!(get_note(&conn, &id).unwrap().title, "", "note restored to the empty version");
        assert!(
            list_note_revisions(&conn, &id).unwrap().iter().any(|r| r.title == "v2"),
            "pre-restore state is snapshotted (restore is undoable)"
        );
    }

    fn settings_only_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )
        .unwrap();
        conn
    }

    fn settings_keys(conn: &Connection) -> Vec<String> {
        let mut stmt = conn.prepare("SELECT key FROM settings ORDER BY key").unwrap();
        let rows: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        rows
    }

    #[test]
    fn delete_setting_is_idempotent() {
        let conn = settings_only_conn();
        set_setting(&conn, "k", "v").unwrap();
        assert_eq!(get_setting(&conn, "k").unwrap().as_deref(), Some("v"));
        delete_setting(&conn, "k").unwrap();
        assert!(get_setting(&conn, "k").unwrap().is_none());
        // Second delete on a missing key is a no-op (rusqlite returns
        // 0 affected rows; we don't surface that as an error).
        delete_setting(&conn, "k").unwrap();
    }

    #[test]
    fn migrate_transcribe_config_v22_user_keeps_typed_drops_legacy() {
        // Simulates a user upgrading from v0.22.x: typed transcribe_config
        // already exists (Settings UI was double-writing) AND legacy keys
        // still present. Migration must keep the typed value untouched
        // and delete every legacy row.
        let conn = settings_only_conn();
        set_setting(
            &conn,
            "transcribe_config",
            r#"{"provider":"deepgram","model":"nova-3"}"#,
        )
        .unwrap();
        set_setting(&conn, "transcribe_provider", "deepgram").unwrap();
        set_setting(&conn, "transcribe_model", "whisper-1").unwrap();
        set_setting(&conn, "whisper_preset", "quality").unwrap();
        set_setting(&conn, "local_whisper_model", "large-v3-turbo-q5").unwrap();
        set_setting(&conn, "local_whisper_use_gpu", "true").unwrap();
        set_setting(&conn, "deepgram_model", "nova-3").unwrap();
        set_setting(&conn, "groq_model", "whisper-large-v3-turbo").unwrap();

        migrate_transcribe_config(&conn).unwrap();

        assert_eq!(
            settings_keys(&conn),
            vec![
                "migrated_transcribe_config_v3".to_string(),
                "transcribe_config".to_string(),
            ],
        );
        assert_eq!(
            get_setting(&conn, "transcribe_config").unwrap().unwrap(),
            r#"{"provider":"deepgram","model":"nova-3"}"#,
        );
        assert_eq!(
            get_setting(&conn, "migrated_transcribe_config_v3")
                .unwrap()
                .as_deref(),
            Some("true"),
        );
    }

    #[test]
    fn migrate_transcribe_config_v21_user_synthesises_typed_then_drops_legacy() {
        // Simulates a user who upgraded straight from v0.21 to v0.23,
        // skipping v0.22 entirely. Only legacy keys exist; migration
        // must build transcribe_config from them, then delete the
        // legacy rows.
        let conn = settings_only_conn();
        set_setting(&conn, "transcribe_provider", "local").unwrap();
        set_setting(&conn, "local_whisper_model", "large-v3-turbo-q5").unwrap();
        set_setting(&conn, "whisper_preset", "balanced").unwrap();
        set_setting(&conn, "local_whisper_use_gpu", "false").unwrap();

        migrate_transcribe_config(&conn).unwrap();

        assert_eq!(
            settings_keys(&conn),
            vec![
                "migrated_transcribe_config_v3".to_string(),
                "transcribe_config".to_string(),
            ],
        );
        let cfg_json = get_setting(&conn, "transcribe_config").unwrap().unwrap();
        let cfg: crate::stt::ProviderConfig = serde_json::from_str(&cfg_json).unwrap();
        match cfg {
            crate::stt::ProviderConfig::Local(c) => {
                assert_eq!(c.model_id, "large-v3-turbo-q5");
                assert_eq!(c.preset, "balanced");
                assert!(!c.use_gpu);
            }
            _ => panic!("expected Local"),
        }
    }

    #[test]
    fn migrate_transcribe_config_fresh_install_writes_default() {
        // Fresh install: no transcribe_config, no legacy keys at all.
        // Migration synthesises an OpenAI/whisper-1 default and marks
        // the flag so subsequent launches no-op.
        let conn = settings_only_conn();
        migrate_transcribe_config(&conn).unwrap();

        assert_eq!(
            settings_keys(&conn),
            vec![
                "migrated_transcribe_config_v3".to_string(),
                "transcribe_config".to_string(),
            ],
        );
        let cfg: crate::stt::ProviderConfig =
            serde_json::from_str(&get_setting(&conn, "transcribe_config").unwrap().unwrap())
                .unwrap();
        match cfg {
            crate::stt::ProviderConfig::OpenAi(c) => {
                assert_eq!(c.model, "whisper-1");
                assert_eq!(c.base_url, None);
            }
            _ => panic!("expected OpenAi default"),
        }
    }

    #[test]
    fn migrate_transcribe_config_is_idempotent() {
        // Running the migration twice must not change state. The flag
        // short-circuits before any read or write.
        let conn = settings_only_conn();
        set_setting(
            &conn,
            "transcribe_config",
            r#"{"provider":"groq","model":"whisper-large-v3-turbo"}"#,
        )
        .unwrap();
        migrate_transcribe_config(&conn).unwrap();
        let after_first = get_setting(&conn, "transcribe_config").unwrap();
        // Re-introduce a stray legacy row to prove the second pass
        // really does no-op (a re-run would otherwise delete it).
        set_setting(&conn, "transcribe_provider", "openai").unwrap();
        migrate_transcribe_config(&conn).unwrap();
        assert_eq!(get_setting(&conn, "transcribe_config").unwrap(), after_first);
        assert_eq!(
            get_setting(&conn, "transcribe_provider").unwrap().as_deref(),
            Some("openai"),
            "second pass must not touch state — the flag short-circuits before any work",
        );
    }

    #[test]
    fn migrate_per_language_v4_wraps_bare_provider_config() {
        // v0.23 user upgrading: typed transcribe_config exists as a
        // bare ProviderConfig. Migration wraps into TranscribeConfig.
        let conn = settings_only_conn();
        set_setting(
            &conn,
            "transcribe_config",
            r#"{"provider":"deepgram","model":"nova-3"}"#,
        )
        .unwrap();
        migrate_per_language_v4(&conn).unwrap();
        let after = get_setting(&conn, "transcribe_config").unwrap().unwrap();
        let parsed: crate::stt::TranscribeConfig = serde_json::from_str(&after).unwrap();
        assert_eq!(parsed.default.provider_id(), "deepgram");
        assert!(parsed.per_language.is_empty());
    }

    #[test]
    fn migrate_per_language_v4_is_idempotent() {
        let conn = settings_only_conn();
        set_setting(
            &conn,
            "transcribe_config",
            r#"{"provider":"openai","model":"whisper-1"}"#,
        )
        .unwrap();
        migrate_per_language_v4(&conn).unwrap();
        let after_first = get_setting(&conn, "transcribe_config").unwrap();
        migrate_per_language_v4(&conn).unwrap();
        let after_second = get_setting(&conn, "transcribe_config").unwrap();
        assert_eq!(after_first, after_second, "second run must be a no-op");
    }

    #[test]
    fn migrate_per_language_v4_skips_when_row_absent() {
        // Fresh install: no transcribe_config yet. Migration finds
        // nothing to wrap; the runtime fallback in read_transcribe_config
        // handles this user.
        let conn = settings_only_conn();
        migrate_per_language_v4(&conn).unwrap();
        assert!(get_setting(&conn, "transcribe_config").unwrap().is_none());
    }

    #[test]
    fn migrate_per_language_v4_preserves_existing_overrides_on_rerun() {
        // v0.24 user re-runs the migration on every launch. The row
        // already has `per_language` entries; they must survive.
        let conn = settings_only_conn();
        set_setting(
            &conn,
            "transcribe_config",
            r#"{"default":{"provider":"openai","model":"whisper-1"},"per_language":{"no":{"provider":"local","model_id":"nb-whisper-large-q5","preset":"quality","use_gpu":true}}}"#,
        )
        .unwrap();
        migrate_per_language_v4(&conn).unwrap();
        let after = get_setting(&conn, "transcribe_config").unwrap().unwrap();
        let parsed: crate::stt::TranscribeConfig = serde_json::from_str(&after).unwrap();
        assert_eq!(parsed.per_language.len(), 1);
        assert_eq!(parsed.per_language.get("no").unwrap().provider_id(), "local");
    }

    #[test]
    fn migrate_per_language_v4_errors_on_garbage_row() {
        let conn = settings_only_conn();
        set_setting(&conn, "transcribe_config", r#"{"bogus":true}"#).unwrap();
        // Not a fatal failure for the user — caller logs and falls
        // through; read_transcribe_config recovers via its own
        // fallback. We assert the error type only to document
        // behaviour, not to require the caller to surface it.
        assert!(migrate_per_language_v4(&conn).is_err());
    }

    #[test]
    fn grandfather_onboarding_marks_install_with_notes() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("gf.sqlite")).unwrap();
        create_note(&conn, "en", "meeting", "").unwrap();
        // Empty models dir → grandfather solely on the note.
        let models = dir.path().join("models");
        std::fs::create_dir_all(&models).unwrap();
        migrate_grandfather_onboarding(&conn, &models).unwrap();
        assert_eq!(
            get_setting(&conn, "onboarding_completed").unwrap().as_deref(),
            Some("true"),
        );
    }

    #[test]
    fn grandfather_onboarding_marks_install_with_trashed_note() {
        // A trashed note still proves prior use — COUNT(*) includes it.
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("gf.sqlite")).unwrap();
        let id = create_note(&conn, "en", "meeting", "").unwrap().id;
        delete_note(&conn, &id).unwrap();
        let models = dir.path().join("models");
        std::fs::create_dir_all(&models).unwrap();
        migrate_grandfather_onboarding(&conn, &models).unwrap();
        assert_eq!(
            get_setting(&conn, "onboarding_completed").unwrap().as_deref(),
            Some("true"),
        );
    }

    #[test]
    fn grandfather_onboarding_marks_install_with_local_model() {
        // No notes, but a downloaded model .bin present → grandfather.
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("gf.sqlite")).unwrap();
        let models = dir.path().join("models");
        std::fs::create_dir_all(&models).unwrap();
        std::fs::write(models.join("ggml-large-v3-turbo-q5_0.bin"), b"x").unwrap();
        migrate_grandfather_onboarding(&conn, &models).unwrap();
        assert_eq!(
            get_setting(&conn, "onboarding_completed").unwrap().as_deref(),
            Some("true"),
        );
    }

    #[test]
    fn grandfather_onboarding_leaves_fresh_install_unset() {
        // Zero notes, no model file, models dir absent entirely →
        // the key stays unset and the frontend shows the wizard.
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("gf.sqlite")).unwrap();
        let models = dir.path().join("models"); // never created
        migrate_grandfather_onboarding(&conn, &models).unwrap();
        assert!(get_setting(&conn, "onboarding_completed").unwrap().is_none());
    }

    #[test]
    fn grandfather_onboarding_ignores_non_bin_files() {
        // A stray non-.bin file in models/ must not count as a model.
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("gf.sqlite")).unwrap();
        let models = dir.path().join("models");
        std::fs::create_dir_all(&models).unwrap();
        std::fs::write(models.join(".DS_Store"), b"x").unwrap();
        migrate_grandfather_onboarding(&conn, &models).unwrap();
        assert!(get_setting(&conn, "onboarding_completed").unwrap().is_none());
    }

    #[test]
    fn grandfather_onboarding_is_idempotent_and_respects_prior_completion() {
        // Once set (by the wizard completing, say to "true", or a Skip),
        // a fresh-install-looking DB must not be re-marked or overwritten.
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("gf.sqlite")).unwrap();
        set_setting(&conn, "onboarding_completed", "true").unwrap();
        let models = dir.path().join("models"); // empty / absent
        migrate_grandfather_onboarding(&conn, &models).unwrap();
        assert_eq!(
            get_setting(&conn, "onboarding_completed").unwrap().as_deref(),
            Some("true"),
            "prior value preserved; migration short-circuits",
        );
    }
}
