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
        -- Multiple conversations ("sessions") per (tenant, scope, scope_id) are
        -- allowed since #61: the active one is the most-recently-updated row, and
        -- an explicit command creates a fresh one. The old UNIQUE index
        -- `idx_conversations_scope` that enforced one-per-scope is dropped below
        -- (idempotent migration); this non-unique index (a DIFFERENT name, so the
        -- DROP can't clobber it) keeps the most-recent lookup cheap.
        CREATE INDEX IF NOT EXISTS idx_conversations_recent
            ON conversations(tenant, scope, scope_id, updated_at DESC);

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

        -- Retrieval substrate for agentic chat (issue #47). Each Note is split
        -- into fixed-size chunks across its body / transcript / summary; the
        -- chunks are the unit of keyword search (and, in the next slice, of
        -- semantic embeddings). `note_chunks` holds the structured rows (source
        -- + order + text) for citations; `note_chunks_fts` is the FTS5 index the
        -- search tool queries. The two are kept in lockstep by `reindex_note`.
        -- `text_hash` (issue #48) content-addresses each chunk so its embedding
        -- survives a reindex (which churns chunk ids): unchanged text keeps its
        -- cached vector, only changed text re-embeds.
        CREATE TABLE IF NOT EXISTS note_chunks (
            id          TEXT PRIMARY KEY,
            note_id     TEXT NOT NULL,
            seq         INTEGER NOT NULL,
            source      TEXT NOT NULL,
            text        TEXT NOT NULL,
            text_hash   TEXT NOT NULL DEFAULT '',
            created_at  INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_note_chunks_note ON note_chunks(note_id);
        CREATE INDEX IF NOT EXISTS idx_note_chunks_hash ON note_chunks(text_hash);

        -- Standalone FTS5 index (not external-content, so plain DELETE/INSERT
        -- keep it in sync without triggers). `chunk_id`/`note_id` ride along
        -- UNINDEXED so a MATCH returns the joinable ids directly. unicode61 with
        -- diacritics folded suits mixed Norwegian/English notes.
        CREATE VIRTUAL TABLE IF NOT EXISTS note_chunks_fts USING fts5(
            text,
            chunk_id UNINDEXED,
            note_id UNINDEXED,
            tokenize = 'unicode61 remove_diacritics 2'
        );

        -- Content-addressed embedding cache (issue #48). Keyed by (text_hash,
        -- model): one vector per distinct chunk text per embedding model. A
        -- model switch just misses the cache and re-embeds under the new key
        -- (fixed-dim-per-index — a query only ever reads one model's vectors).
        -- Brute-force cosine in-process; no vector index.
        CREATE TABLE IF NOT EXISTS chunk_embeddings (
            text_hash   TEXT NOT NULL,
            model       TEXT NOT NULL,
            dims        INTEGER NOT NULL,
            vector      BLOB NOT NULL,
            created_at  INTEGER NOT NULL,
            PRIMARY KEY (text_hash, model)
        );
        "#,
    )?;
    // #48: add text_hash to note_chunks created by the #47 schema.
    let _ = conn.execute("ALTER TABLE note_chunks ADD COLUMN text_hash TEXT NOT NULL DEFAULT ''", []);
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
    // Workspace (Teams) chat (issue #50). A workspace conversation is
    // server-authoritative: the local row is just a (tenant, scope, scope_id)
    // handle whose `remote_id` maps to the humla-cloud conversation record id.
    // NULL until the first turn creates the server conversation. Personal
    // conversations never set it. Its messages live server-side (read-through),
    // never in the local `messages` table.
    let _ = conn.execute("ALTER TABLE conversations ADD COLUMN remote_id TEXT", []);
    // Persisted retrieval breadth per conversation (issue #58). The Scope chip
    // ("This note" / "Folder: X" / "All notes") used to be component-local UI
    // state that silently reset on note-change, tenant switch and panel remount;
    // storing it on the conversation makes the backend the single source of
    // truth so the chosen breadth survives turns, tab switches and restarts.
    // NOT NULL DEFAULT 'note' back-fills existing rows with the safe breadth.
    let _ = conn.execute(
        "ALTER TABLE conversations ADD COLUMN breadth TEXT NOT NULL DEFAULT 'note'",
        [],
    );
    // Chat sessions (issue #61). Two idempotent steps:
    //   1. Drop the old UNIQUE index that pinned one conversation per
    //      (tenant, scope, scope_id) — sessions need many per scope now. IF
    //      EXISTS makes this a no-op on fresh DBs and on reruns.
    //   2. Add a nullable `title` column. Nullable (no default) so a freshly
    //      created conversation carries NULL until its first user message sets
    //      it (`set_conversation_title`); the backfill below fills legacy rows.
    let _ = conn.execute("DROP INDEX IF EXISTS idx_conversations_scope", []);
    let _ = conn.execute("ALTER TABLE conversations ADD COLUMN title TEXT", []);
    // Back-fill titles for pre-#61 conversations ONCE — guarded by a settings
    // flag inside the fn so it can't re-run on later launches (that would
    // date-title empty conversations created after the migration and block their
    // first-message title). Existing threads keep their identity: title derived
    // from the oldest user message, date fallback when empty.
    backfill_conversation_titles(&conn)?;
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
    /// humla-cloud conversation record id for a workspace (Teams) conversation
    /// (issue #50); NULL for Personal conversations and until the first
    /// workspace turn creates the server record.
    pub remote_id: Option<String>,
    /// Persisted retrieval breadth (issue #58): "note" | "folder" | "all". The
    /// single source of truth for the Scope chip; a live filter on retrieval
    /// within the conversation, not a conversation-identity dimension.
    pub breadth: String,
    /// Session title (issue #61). NULL until the first user message sets it
    /// (personal scope) or the migration back-fills it; the session list falls
    /// back to a derived date label when it's still absent.
    pub title: Option<String>,
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
        remote_id: row.get(4)?,
        breadth: row.get(5)?,
        title: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

const CONVERSATION_COLS: &str =
    "id, scope, scope_id, tenant, remote_id, breadth, title, created_at, updated_at";

/// The most-recently-updated conversation ("active session") for a scope, or
/// None (issue #61). This replaces the old get-or-create: opening the Chat tab
/// resolves the active session with this and never creates a row as a side
/// effect. `updated_at` is bumped on every message append, so the newest thread
/// wins; `created_at` then `id` break ties deterministically.
pub fn latest_conversation(
    conn: &Connection,
    tenant: &str,
    scope: &str,
    scope_id: &str,
) -> Result<Option<Conversation>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {CONVERSATION_COLS} FROM conversations
         WHERE tenant = ?1 AND scope = ?2 AND scope_id = ?3
         ORDER BY updated_at DESC, created_at DESC, id DESC LIMIT 1",
    ))?;
    match stmt.query_row(params![tenant, scope, scope_id], map_conversation) {
        Ok(c) => Ok(Some(c)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// All conversations for a scope, most-recently-updated first (issue #61) — the
/// backing query for the session list.
pub fn list_conversations(
    conn: &Connection,
    tenant: &str,
    scope: &str,
    scope_id: &str,
) -> Result<Vec<Conversation>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {CONVERSATION_COLS} FROM conversations
         WHERE tenant = ?1 AND scope = ?2 AND scope_id = ?3
         ORDER BY updated_at DESC, created_at DESC, id DESC",
    ))?;
    let rows = stmt
        .query_map(params![tenant, scope, scope_id], map_conversation)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// One conversation by its primary key, or None (issue #61). Used to resolve an
/// explicit `conversation_id` from the frontend.
pub fn get_conversation_by_id(conn: &Connection, id: &str) -> Result<Option<Conversation>> {
    let mut stmt = conn
        .prepare_cached(&format!("SELECT {CONVERSATION_COLS} FROM conversations WHERE id = ?1"))?;
    match stmt.query_row(params![id], map_conversation) {
        Ok(c) => Ok(Some(c)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// The workspace handle whose `remote_id` maps to a given server conversation
/// (issue #61). Lets the session-list reconcile server-authoritative workspace
/// conversations back to their local handle rows without duplicating them.
pub fn get_conversation_by_remote_id(
    conn: &Connection,
    tenant: &str,
    remote_id: &str,
) -> Result<Option<Conversation>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {CONVERSATION_COLS} FROM conversations WHERE tenant = ?1 AND remote_id = ?2",
    ))?;
    match stmt.query_row(params![tenant, remote_id], map_conversation) {
        Ok(c) => Ok(Some(c)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Create a fresh conversation with an explicit initial breadth (issue #61),
/// returning the inserted row. Title starts NULL (set from the first user
/// message); remote_id starts NULL (set later for a workspace handle).
pub fn create_conversation(
    conn: &Connection,
    tenant: &str,
    scope: &str,
    scope_id: &str,
    breadth: &str,
) -> Result<Conversation> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_ms();
    conn.execute(
        "INSERT INTO conversations (id, scope, scope_id, tenant, breadth, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
        params![id, scope, scope_id, tenant, breadth, now],
    )?;
    get_conversation_by_id(conn, &id)?
        .ok_or_else(|| anyhow::anyhow!("conversation vanished after insert"))
}

/// Number of persisted messages in a conversation (issue #61) — feeds the
/// session-list `message_count` and the new-chat no-op guard.
pub fn conversation_message_count(conn: &Connection, conversation_id: &str) -> Result<i64> {
    let mut stmt = conn
        .prepare_cached("SELECT COUNT(*) FROM messages WHERE conversation_id = ?1")?;
    Ok(stmt.query_row(params![conversation_id], |r| r.get(0))?)
}

/// Set a conversation's title (issue #61). Personal scope sets this once from
/// the first user message at send time; the migration back-fill also calls it.
pub fn set_conversation_title(conn: &Connection, id: &str, title: &str) -> Result<()> {
    conn.execute(
        "UPDATE conversations SET title = ?1 WHERE id = ?2",
        params![title, id],
    )?;
    Ok(())
}

/// Back-fill titles for conversations that predate the `title` column (issue
/// #61). For each NULL-title row, derive a title from its oldest user message
/// (whitespace-collapsed, char-boundary-truncated) or a date fallback when the
/// conversation has no user message.
///
/// Guarded by a one-time `chat_titles_backfilled_v1` flag (the repo's migration-
/// flag pattern) so it runs EXACTLY ONCE, not on every launch. Running it every
/// open would date-title empty conversations created after the migration, and
/// that non-NULL date title would then block the send-time message-derived
/// title. After the one run, conversations created later keep NULL title until
/// their first user message sets it.
fn backfill_conversation_titles(conn: &Connection) -> Result<()> {
    const FLAG: &str = "chat_titles_backfilled_v1";
    if get_setting(conn, FLAG)?.as_deref() == Some("true") {
        return Ok(());
    }
    let pending: Vec<(String, i64)> = {
        let mut stmt =
            conn.prepare("SELECT id, created_at FROM conversations WHERE title IS NULL")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    for (conv_id, created_at) in pending {
        let content: Option<String> = conn
            .query_row(
                "SELECT content FROM messages WHERE conversation_id = ?1 AND role = 'user'
                 ORDER BY seq LIMIT 1",
                params![conv_id],
                |r| r.get(0),
            )
            .optional()?;
        let text = content.map(|c| crate::chat::parts_plain_text(&c));
        let title = crate::chat::derive_title(
            text.as_deref().filter(|t| !t.trim().is_empty()),
            created_at,
        );
        conn.execute(
            "UPDATE conversations SET title = ?1 WHERE id = ?2",
            params![title, conv_id],
        )?;
    }
    set_setting(conn, FLAG, "true")?;
    Ok(())
}

/// Record the humla-cloud conversation record id for a workspace conversation
/// (issue #50), so later turns resume it and read-through history can find it.
/// Idempotent — a repeat with the same id is a no-op write.
pub fn set_conversation_remote_id(conn: &Connection, id: &str, remote_id: &str) -> Result<()> {
    conn.execute(
        "UPDATE conversations SET remote_id = ?1 WHERE id = ?2",
        params![remote_id, id],
    )?;
    Ok(())
}

/// Persist a conversation's retrieval breadth (issue #58). The command layer
/// validates the value against the {note, folder, all} vocabulary before
/// calling this, so db.rs stays agnostic and just stores the string.
pub fn set_conversation_breadth(conn: &Connection, id: &str, breadth: &str) -> Result<()> {
    conn.execute(
        "UPDATE conversations SET breadth = ?1 WHERE id = ?2",
        params![breadth, id],
    )?;
    Ok(())
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

// ── Retrieval substrate: chunking + FTS5 keyword search (issue #47) ──────────

/// Target chunk size in characters. ~750 tokens at ~4 chars/token, comfortably
/// inside the 500–1000-token window the issue asks for. Chunks break on
/// paragraph boundaries; a paragraph longer than this is hard-split.
pub const CHUNK_TARGET_CHARS: usize = 3000;

/// A keyword-search hit: the matched chunk plus enough of the parent Note to
/// build a citation chip (title + date). `rank` is the raw bm25 score (lower =
/// better); callers order by it and otherwise treat it as opaque.
#[derive(Debug, Clone, Serialize)]
pub struct ChunkHit {
    pub note_id: String,
    pub note_title: String,
    pub note_created_at: i64,
    pub source: String,
    pub text: String,
    pub rank: f64,
}

/// Lightweight Note descriptor for the `list_notes` tool — just what a citation
/// or a "which note?" decision needs, never the full body/transcript.
#[derive(Debug, Clone, Serialize)]
pub struct NoteMeta {
    pub id: String,
    pub title: String,
    pub created_at: i64,
    pub folder_id: Option<String>,
    pub client_id: Option<String>,
}

/// Independent, combinable Note filters for retrieval. Every field is optional
/// and ANDs with the others — never a nested taxonomy. `note_id` backs the
/// chat "this Note" breadth clamp; `folder_id`/`client_id` back the "this
/// Folder" clamp and the model's own narrowing tool params.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoteFilter<'a> {
    pub note_id: Option<&'a str>,
    pub folder_id: Option<&'a str>,
    pub client_id: Option<&'a str>,
}

/// Split arbitrary text into ~`target`-char chunks, breaking on blank-line
/// paragraph boundaries where possible and hard-splitting any single paragraph
/// longer than `target`. Blank/whitespace input yields no chunks.
pub fn split_into_chunks(text: &str, target: usize) -> Vec<String> {
    let target = target.max(1);
    let mut chunks: Vec<String> = Vec::new();
    let mut cur = String::new();
    // Paragraphs are runs separated by blank lines; fall back to the whole
    // string if there are none.
    for para in text.split("\n\n") {
        let para = para.trim();
        if para.is_empty() {
            continue;
        }
        if para.chars().count() > target {
            // Flush what we have, then hard-split the oversized paragraph on
            // char boundaries.
            if !cur.is_empty() {
                chunks.push(std::mem::take(&mut cur));
            }
            let chars: Vec<char> = para.chars().collect();
            for window in chars.chunks(target) {
                chunks.push(window.iter().collect());
            }
            continue;
        }
        // Would appending this paragraph overflow the target? If so, flush.
        if !cur.is_empty() && cur.chars().count() + para.chars().count() + 2 > target {
            chunks.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push_str("\n\n");
        }
        cur.push_str(para);
    }
    if !cur.is_empty() {
        chunks.push(cur);
    }
    chunks
}

/// Produce the (source, text) chunk list for a Note. `body_text` must already be
/// plain text (HTML stripped by the caller). Sources are chunked independently
/// so a chunk never straddles e.g. transcript and summary.
pub fn note_chunk_texts(
    body_text: &str,
    transcript: &str,
    summary: &str,
) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    for (source, text) in [
        ("body", body_text),
        ("transcript", transcript),
        ("summary", summary),
    ] {
        for chunk in split_into_chunks(text, CHUNK_TARGET_CHARS) {
            out.push((source, chunk));
        }
    }
    out
}

/// Rebuild a Note's chunk rows + FTS index from its current content. Idempotent:
/// clears the Note's existing chunks first, so calling it on every
/// content-settled checkpoint keeps the index fresh without duplication.
/// Returns the number of chunks indexed.
pub fn reindex_note(
    conn: &Connection,
    note_id: &str,
    body_text: &str,
    transcript: &str,
    summary: &str,
) -> Result<usize> {
    remove_note_chunks(conn, note_id)?;
    let now = now_ms();
    let chunks = note_chunk_texts(body_text, transcript, summary);
    for (seq, (source, text)) in chunks.iter().enumerate() {
        let chunk_id = uuid::Uuid::new_v4().to_string();
        let hash = text_hash(text);
        conn.execute(
            "INSERT INTO note_chunks (id, note_id, seq, source, text, text_hash, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![chunk_id, note_id, seq as i64, source, text, hash, now],
        )?;
        conn.execute(
            "INSERT INTO note_chunks_fts (text, chunk_id, note_id) VALUES (?1, ?2, ?3)",
            params![text, chunk_id, note_id],
        )?;
    }
    Ok(chunks.len())
}

/// Stable content hash for a chunk's text — the embedding cache key. FNV-1a
/// (64-bit, hex): deterministic across runs/platforms, no dependency. Collision
/// probability at personal scale (thousands of chunks) is negligible, and the
/// only cost of one would be a single chunk borrowing another's vector.
pub fn text_hash(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Drop a Note's chunk + FTS rows (e.g. before a rebuild, or when a Note is
/// hard-deleted). Safe to call for a Note that has none.
pub fn remove_note_chunks(conn: &Connection, note_id: &str) -> Result<()> {
    conn.execute("DELETE FROM note_chunks WHERE note_id = ?1", params![note_id])?;
    conn.execute("DELETE FROM note_chunks_fts WHERE note_id = ?1", params![note_id])?;
    Ok(())
}

/// Ids of live notes needing a (re)index — the startup-backfill work-list.
/// Covers notes with no chunks (pre-#47) AND notes whose chunks predate the
/// #48 `text_hash` column (so re-chunking backfills the embedding key). Cheap
/// anti-join / existence check.
pub fn note_ids_needing_reindex(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT id FROM notes WHERE deleted_at IS NULL AND ( \
             id NOT IN (SELECT DISTINCT note_id FROM note_chunks) \
             OR id IN (SELECT DISTINCT note_id FROM note_chunks WHERE text_hash = '') \
         )",
    )?;
    let ids = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(ids)
}

/// Sanitise a free-text query into a safe FTS5 MATCH expression. FTS5 MATCH
/// syntax treats many characters as operators (`"`, `*`, `:`, `-`, `(`, `^`,
/// `AND`/`OR`/`NOT`), so a raw user/model string can be a syntax error. We keep
/// only alphanumeric runs as terms, wrap each in double quotes (defeating
/// operator interpretation), and join with `OR` for recall — bm25 still ranks
/// notes matching more/rarer terms higher. Returns `None` if no usable term
/// remains, which the caller surfaces as an empty-query error.
fn fts_match_query(query: &str) -> Option<String> {
    let terms: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{}\"", t.to_lowercase()))
        .collect();
    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" OR "))
    }
}

// ── Semantic retrieval: embedding cache + hybrid (RRF) search (issue #48) ────

/// Encode an embedding vector as a little-endian f32 blob for storage.
fn vec_to_blob(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

/// Decode a stored little-endian f32 blob back to a vector. A blob whose length
/// isn't a multiple of 4 (corrupt) decodes to what fits, ignoring the tail.
fn blob_to_vec(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

/// Distinct (text_hash, text) for a Note's chunks that have no embedding under
/// `model` yet — the incremental re-embed work-list for a content-settled
/// checkpoint. Only changed/new text (cache-missing hashes) comes back.
pub fn note_texts_needing_embedding(
    conn: &Connection,
    note_id: &str,
    model: &str,
) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT c.text_hash, c.text FROM note_chunks c \
         WHERE c.note_id = ?1 AND c.text_hash <> '' \
         AND NOT EXISTS ( \
            SELECT 1 FROM chunk_embeddings e \
            WHERE e.text_hash = c.text_hash AND e.model = ?2 \
         )",
    )?;
    let rows = stmt
        .query_map(params![note_id, model], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Store one chunk's embedding under (text_hash, model). Idempotent (REPLACE),
/// so a re-embed of the same text overwrites rather than duplicates.
pub fn store_embedding(conn: &Connection, text_hash: &str, model: &str, vector: &[f32]) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO chunk_embeddings (text_hash, model, dims, vector, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![text_hash, model, vector.len() as i64, vec_to_blob(vector), now_ms()],
    )?;
    Ok(())
}

/// Ids of live notes that have at least one chunk with no embedding under
/// `model` — the work-list for the startup embed backfill, so cross-note
/// semantic search works day-one rather than only for touched notes.
pub fn note_ids_needing_embedding(conn: &Connection, model: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT c.note_id FROM note_chunks c \
         JOIN notes n ON n.id = c.note_id \
         WHERE n.deleted_at IS NULL AND c.text_hash <> '' \
         AND NOT EXISTS ( \
            SELECT 1 FROM chunk_embeddings e \
            WHERE e.text_hash = c.text_hash AND e.model = ?1 \
         )",
    )?;
    let ids = stmt
        .query_map(params![model], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(ids)
}

/// How many chunk embeddings exist for a model. Test-only for now (asserts the
/// incremental cache); promote to `pub` if telemetry ever needs it.
#[cfg(test)]
pub fn count_embeddings(conn: &Connection, model: &str) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM chunk_embeddings WHERE model = ?1",
        params![model],
        |r| r.get(0),
    )?)
}

/// Cosine similarity of two equal-length vectors in [-1, 1]. Returns 0 for a
/// length mismatch or a zero-magnitude vector (degenerate, not comparable).
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// Reciprocal Rank Fusion. Each input list is item ids in rank order (best
/// first). An item's fused score is Σ 1/(k + rank+1) over the lists it appears
/// in; higher = better. Returns ids with scores, best first. `k` damps the
/// contribution of low ranks (60 is the standard default).
pub fn rrf_fuse(lists: &[Vec<String>], k: f64) -> Vec<(String, f64)> {
    use std::collections::HashMap;
    let mut scores: HashMap<String, f64> = HashMap::new();
    for list in lists {
        for (rank, id) in list.iter().enumerate() {
            *scores.entry(id.clone()).or_insert(0.0) += 1.0 / (k + (rank as f64) + 1.0);
        }
    }
    let mut fused: Vec<(String, f64)> = scores.into_iter().collect();
    // Sort by score desc; tie-break by id for determinism.
    fused.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal).then(a.0.cmp(&b.0)));
    fused
}

/// A chunk hit that carries its stable chunk id (for fusion) alongside the
/// citation fields.
struct IdentifiedHit {
    chunk_id: String,
    hit: ChunkHit,
}

/// Keyword-ranked chunks (bm25, best first) with their chunk ids — the FTS half
/// of hybrid search. Applies the same combinable note filters as the rest.
fn keyword_ranked(
    conn: &Connection,
    query: &str,
    filter: NoteFilter<'_>,
    workspace: &str,
    limit: usize,
) -> Result<Vec<IdentifiedHit>> {
    let Some(match_expr) = fts_match_query(query) else {
        return Ok(Vec::new());
    };
    use rusqlite::types::Value;
    let mut sql = String::from(
        "SELECT c.id, n.id, n.title, n.created_at, c.source, c.text, bm25(note_chunks_fts) AS rank \
         FROM note_chunks_fts f \
         JOIN note_chunks c ON c.id = f.chunk_id \
         JOIN notes n ON n.id = c.note_id \
         WHERE note_chunks_fts MATCH ?1 AND n.workspace_id = ?2 AND n.deleted_at IS NULL",
    );
    let mut args: Vec<Value> = vec![Value::Text(match_expr), Value::Text(workspace.to_string())];
    push_note_filters(&mut sql, &mut args, filter, "n");
    args.push(Value::Integer(limit as i64));
    sql.push_str(&format!(" ORDER BY rank LIMIT ?{}", args.len()));
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(args), |row| {
            Ok(IdentifiedHit {
                chunk_id: row.get(0)?,
                hit: ChunkHit {
                    note_id: row.get(1)?,
                    note_title: row.get(2)?,
                    note_created_at: row.get(3)?,
                    source: row.get(4)?,
                    text: row.get(5)?,
                    rank: row.get(6)?,
                },
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Semantic-ranked chunks (cosine vs `query_vec`, best first) for chunks that
/// have an embedding under `model`. Brute-force over all in-scope chunks — no
/// vector index (issue #48).
fn semantic_ranked(
    conn: &Connection,
    query_vec: &[f32],
    model: &str,
    filter: NoteFilter<'_>,
    workspace: &str,
    limit: usize,
) -> Result<Vec<IdentifiedHit>> {
    use rusqlite::types::Value;
    let mut sql = String::from(
        "SELECT c.id, n.id, n.title, n.created_at, c.source, c.text, e.vector \
         FROM note_chunks c \
         JOIN chunk_embeddings e ON e.text_hash = c.text_hash AND e.model = ?1 \
         JOIN notes n ON n.id = c.note_id \
         WHERE n.workspace_id = ?2 AND n.deleted_at IS NULL",
    );
    let mut args: Vec<Value> = vec![Value::Text(model.to_string()), Value::Text(workspace.to_string())];
    push_note_filters(&mut sql, &mut args, filter, "n");
    let mut stmt = conn.prepare(&sql)?;
    let mut scored: Vec<(f32, IdentifiedHit)> = stmt
        .query_map(rusqlite::params_from_iter(args), |row| {
            let blob: Vec<u8> = row.get(6)?;
            let sim = cosine(query_vec, &blob_to_vec(&blob));
            Ok((
                sim,
                IdentifiedHit {
                    chunk_id: row.get(0)?,
                    hit: ChunkHit {
                        note_id: row.get(1)?,
                        note_title: row.get(2)?,
                        note_created_at: row.get(3)?,
                        source: row.get(4)?,
                        text: row.get(5)?,
                        rank: 0.0,
                    },
                },
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(limit);
    Ok(scored.into_iter().map(|(sim, mut ih)| {
        ih.hit.rank = sim as f64; // carry the cosine similarity for reference
        ih
    }).collect())
}

/// Append the combinable note filters to a query being built, using positional
/// params against alias `t`. Shared by keyword + semantic + list queries.
fn push_note_filters(
    sql: &mut String,
    args: &mut Vec<rusqlite::types::Value>,
    filter: NoteFilter<'_>,
    t: &str,
) {
    use rusqlite::types::Value;
    if let Some(id) = filter.note_id {
        args.push(Value::Text(id.to_string()));
        sql.push_str(&format!(" AND {t}.id = ?{}", args.len()));
    }
    if let Some(f) = filter.folder_id {
        args.push(Value::Text(f.to_string()));
        sql.push_str(&format!(" AND {t}.folder_id = ?{}", args.len()));
    }
    if let Some(cl) = filter.client_id {
        args.push(Value::Text(cl.to_string()));
        sql.push_str(&format!(" AND {t}.client_id = ?{}", args.len()));
    }
}

/// The size of each per-signal candidate pool fused by RRF. Bigger than the
/// final `limit` so a chunk ranked mid-pack by one signal can still win on the
/// other.
const HYBRID_POOL: usize = 20;
/// RRF damping constant (standard default).
const RRF_K: f64 = 60.0;

/// Hybrid keyword+semantic search. With `query_vec = Some`, fuses BM25 and
/// cosine rankings via RRF; with `None` (embedding unavailable), degrades to
/// keyword-only so chat still works (issue #48 graceful degradation). Returns
/// the top `limit` chunk hits.
pub fn hybrid_search_chunks(
    conn: &Connection,
    query: &str,
    query_vec: Option<&[f32]>,
    model: &str,
    filter: NoteFilter<'_>,
    workspace: &str,
    limit: usize,
) -> Result<Vec<ChunkHit>> {
    let keyword = keyword_ranked(conn, query, filter, workspace, HYBRID_POOL)?;
    let Some(qv) = query_vec else {
        // Keyword-only fallback.
        return Ok(keyword.into_iter().take(limit).map(|ih| ih.hit).collect());
    };
    let semantic = semantic_ranked(conn, qv, model, filter, workspace, HYBRID_POOL)?;
    if semantic.is_empty() {
        // No vectors yet (e.g. not embedded) → keyword-only.
        return Ok(keyword.into_iter().take(limit).map(|ih| ih.hit).collect());
    }

    // Fuse by chunk id; keep a lookup so the winners map back to their hits.
    use std::collections::HashMap;
    let mut hits: HashMap<String, ChunkHit> = HashMap::new();
    let kw_ids: Vec<String> = keyword.into_iter().map(|ih| { hits.insert(ih.chunk_id.clone(), ih.hit); ih.chunk_id }).collect();
    let sem_ids: Vec<String> = semantic.into_iter().map(|ih| { hits.entry(ih.chunk_id.clone()).or_insert(ih.hit); ih.chunk_id }).collect();
    let fused = rrf_fuse(&[kw_ids, sem_ids], RRF_K);
    Ok(fused
        .into_iter()
        .take(limit)
        .filter_map(|(id, score)| hits.remove(&id).map(|mut h| {
            h.rank = score; // fused RRF score (higher = better)
            h
        }))
        .collect())
}

/// List live Notes in a workspace, optionally narrowed by folder and/or client,
/// most-recent first. Backs the `list_notes` retrieval tool.
pub fn list_notes_filtered(
    conn: &Connection,
    filter: NoteFilter<'_>,
    workspace: &str,
    limit: usize,
) -> Result<Vec<NoteMeta>> {
    use rusqlite::types::Value;
    let mut sql = String::from(
        "SELECT id, title, created_at, folder_id, client_id FROM notes \
         WHERE workspace_id = ?1 AND deleted_at IS NULL",
    );
    let mut args: Vec<Value> = vec![Value::Text(workspace.to_string())];
    if let Some(id) = filter.note_id {
        args.push(Value::Text(id.to_string()));
        sql.push_str(&format!(" AND id = ?{}", args.len()));
    }
    if let Some(f) = filter.folder_id {
        args.push(Value::Text(f.to_string()));
        sql.push_str(&format!(" AND folder_id = ?{}", args.len()));
    }
    if let Some(cl) = filter.client_id {
        args.push(Value::Text(cl.to_string()));
        sql.push_str(&format!(" AND client_id = ?{}", args.len()));
    }
    args.push(Value::Integer(limit as i64));
    sql.push_str(&format!(" ORDER BY updated_at DESC LIMIT ?{}", args.len()));

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(args), |row| {
            Ok(NoteMeta {
                id: row.get(0)?,
                title: row.get(1)?,
                created_at: row.get(2)?,
                folder_id: row.get(3)?,
                client_id: row.get(4)?,
            })
        })?
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

    /// A workspace (Teams) conversation is keyed by tenant and carries a
    /// remote_id once the server creates it (issue #50). Personal and workspace
    /// conversations for the same Note are distinct rows; remote_id starts NULL.
    #[test]
    fn conversation_remote_id_round_trips_and_tenant_scopes() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("conv.sqlite")).unwrap();

        let personal = create_conversation(&conn, CHAT_TENANT_PERSONAL, CHAT_SCOPE_NOTE, "note1", "note").unwrap();
        let workspace = create_conversation(&conn, "wsA", CHAT_SCOPE_NOTE, "note1", "note").unwrap();
        assert_ne!(personal.id, workspace.id, "same Note, different tenant → distinct conversations");
        assert!(workspace.remote_id.is_none(), "no server id until the first turn");

        set_conversation_remote_id(&conn, &workspace.id, "srvConv123").unwrap();
        let reloaded = latest_conversation(&conn, "wsA", CHAT_SCOPE_NOTE, "note1").unwrap().unwrap();
        assert_eq!(reloaded.remote_id.as_deref(), Some("srvConv123"));
        // Round-trips by its server id too (the workspace session-list reconcile).
        let by_remote = get_conversation_by_remote_id(&conn, "wsA", "srvConv123").unwrap().unwrap();
        assert_eq!(by_remote.id, workspace.id);
        // The personal conversation is untouched by the workspace one's remote id.
        let personal_reload = latest_conversation(&conn, CHAT_TENANT_PERSONAL, CHAT_SCOPE_NOTE, "note1").unwrap().unwrap();
        assert!(personal_reload.remote_id.is_none());
    }

    /// A conversation defaults to "note" breadth and the stored value survives a
    /// reload (issue #58). Re-running `open()` on the same file is idempotent —
    /// the breadth ALTER is a no-op the second time and the persisted value is
    /// intact (proving the column back-fills without clobbering existing rows).
    #[test]
    fn conversation_breadth_defaults_and_persists_idempotently() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("breadth.sqlite");
        let conv_id = {
            let conn = open(&path).unwrap();
            let conv =
                create_conversation(&conn, CHAT_TENANT_PERSONAL, CHAT_SCOPE_NOTE, "note1", "note")
                    .unwrap();
            assert_eq!(conv.breadth, "note", "new conversations default to note breadth");
            set_conversation_breadth(&conn, &conv.id, "all").unwrap();
            let reloaded =
                latest_conversation(&conn, CHAT_TENANT_PERSONAL, CHAT_SCOPE_NOTE, "note1")
                    .unwrap()
                    .unwrap();
            assert_eq!(reloaded.breadth, "all", "the stored breadth round-trips");
            conv.id
        };
        // Re-open the same DB file: migrations must be idempotent and the stored
        // breadth must not be reset by the (repeated) ALTER TABLE.
        let conn = open(&path).unwrap();
        let reloaded = latest_conversation(&conn, CHAT_TENANT_PERSONAL, CHAT_SCOPE_NOTE, "note1")
            .unwrap()
            .unwrap();
        assert_eq!(reloaded.id, conv_id);
        assert_eq!(reloaded.breadth, "all", "breadth survives a re-open (idempotent migration)");
    }

    /// Migration idempotency + first-session preservation (issue #61). Seeds a
    /// pre-#61 DB — the old UNIQUE `idx_conversations_scope`, no `title` column,
    /// a conversation with a user message and an empty one — then runs `open()`
    /// (the migrations) twice and asserts: the unique index is dropped, titles
    /// back-fill (from the oldest user message, char-truncated; date fallback for
    /// the empty one), and the existing thread is preserved as the note's single
    /// (first) session. Reopening is a no-op on already-titled rows.
    #[test]
    fn migration_drops_unique_index_and_backfills_titles_idempotently() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mig.sqlite");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE conversations (
                    id TEXT PRIMARY KEY, scope TEXT NOT NULL, scope_id TEXT NOT NULL,
                    tenant TEXT NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
                    remote_id TEXT, breadth TEXT NOT NULL DEFAULT 'note'
                );
                CREATE UNIQUE INDEX idx_conversations_scope
                    ON conversations(tenant, scope, scope_id);
                CREATE TABLE messages (
                    id TEXT PRIMARY KEY, conversation_id TEXT NOT NULL, seq INTEGER NOT NULL,
                    role TEXT NOT NULL, content TEXT NOT NULL, created_at INTEGER NOT NULL
                );",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO conversations (id, scope, scope_id, tenant, created_at, updated_at)
                 VALUES ('conv1','note','n1','personal',1000,2000)",
                [],
            )
            .unwrap();
            let parts = crate::chat::text_parts_json("b0", "  Discuss the Q3 roadmap  and next steps ");
            conn.execute(
                "INSERT INTO messages (id, conversation_id, seq, role, content, created_at)
                 VALUES ('m1','conv1',0,'user',?1,1500)",
                params![parts],
            )
            .unwrap();
            // An empty conversation (no messages) → date fallback from created_at.
            conn.execute(
                "INSERT INTO conversations (id, scope, scope_id, tenant, created_at, updated_at)
                 VALUES ('conv2','note','n2','personal',0,0)",
                [],
            )
            .unwrap();
        }

        // Run migrations twice — must be idempotent.
        for _ in 0..2 {
            let conn = open(&path).unwrap();
            let has_unique: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master
                     WHERE type = 'index' AND name = 'idx_conversations_scope'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(has_unique, 0, "the old unique index is dropped");
        }

        let conn = open(&path).unwrap();
        // Title back-filled from the oldest user message: whitespace collapsed.
        let conv1 = get_conversation_by_id(&conn, "conv1").unwrap().unwrap();
        assert_eq!(conv1.title.as_deref(), Some("Discuss the Q3 roadmap and next steps"));
        // The existing thread is preserved as the note's single (first) session.
        let latest = latest_conversation(&conn, "personal", "note", "n1").unwrap().unwrap();
        assert_eq!(latest.id, "conv1");
        assert_eq!(list_conversations(&conn, "personal", "note", "n1").unwrap().len(), 1);
        // The empty conversation gets the date fallback (created_at = epoch 0).
        let conv2 = get_conversation_by_id(&conn, "conv2").unwrap().unwrap();
        assert_eq!(conv2.title.as_deref(), Some("Chat 1970-01-01"));

        // The backfill ran ONCE and set its flag.
        assert_eq!(get_setting(&conn, "chat_titles_backfilled_v1").unwrap().as_deref(), Some("true"));

        // A conversation created AFTER the backfill flag is set must NOT be
        // date-titled by a later launch — it keeps NULL title until its first
        // user message titles it (the hazard fix).
        let fresh = create_conversation(&conn, "personal", "note", "n3", "note").unwrap();
        assert!(fresh.title.is_none(), "a new empty conversation starts untitled");
        drop(conn);
        let conn = open(&path).unwrap(); // reopen: backfill is skipped by the flag
        let fresh = get_conversation_by_id(&conn, &fresh.id).unwrap().unwrap();
        assert!(fresh.title.is_none(), "reopen must not date-title a post-migration conversation");
        // First user message titles it (mirrors chat_send's send-time titling).
        let title = crate::chat::derive_title(Some("What is the plan?"), fresh.created_at);
        set_conversation_title(&conn, &fresh.id, &title).unwrap();
        let titled = get_conversation_by_id(&conn, &fresh.id).unwrap().unwrap();
        assert_eq!(titled.title.as_deref(), Some("What is the plan?"), "message-derived title, not a date");
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

    // ── Retrieval substrate: chunking + FTS5 search (issue #47) ──────────────

    #[test]
    fn split_into_chunks_breaks_on_paragraphs_and_hard_splits_long_ones() {
        assert!(split_into_chunks("   \n\n  ", 100).is_empty(), "blank yields nothing");

        // Three short paragraphs, tiny target → one chunk each (each alone fits,
        // but two together overflow).
        let text = "alpha para\n\nbeta para\n\ngamma para";
        let chunks = split_into_chunks(text, 12);
        assert_eq!(chunks.len(), 3, "each paragraph its own chunk under a tight budget");
        assert_eq!(chunks[0], "alpha para");

        // A single paragraph longer than the target is hard-split on char count.
        let long = "x".repeat(250);
        let chunks = split_into_chunks(&long, 100);
        assert_eq!(chunks.len(), 3, "250 chars / 100 target = 3 windows");
        assert_eq!(chunks[0].chars().count(), 100);
        assert_eq!(chunks[2].chars().count(), 50);

        // Small paragraphs that fit together stay merged.
        let merged = split_into_chunks("one\n\ntwo", 1000);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0], "one\n\ntwo");
    }

    #[test]
    fn note_chunk_texts_tags_each_source() {
        let chunks = note_chunk_texts("body words", "transcript words", "summary words");
        let sources: Vec<&str> = chunks.iter().map(|(s, _)| *s).collect();
        assert_eq!(sources, vec!["body", "transcript", "summary"]);
        // Blank sources contribute nothing.
        let sparse = note_chunk_texts("only body", "", "");
        assert_eq!(sparse.len(), 1);
        assert_eq!(sparse[0].0, "body");
    }

    #[test]
    fn reindex_and_search_finds_and_ranks_notes() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("search.sqlite")).unwrap();

        let a = create_note(&conn, "en", "meeting", "").unwrap();
        update_note(
            &conn,
            &a.id,
            &NotePatch {
                title: Some("Budget planning".into()),
                transcript: Some("We discussed the quarterly budget and the marketing spend.".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let b = create_note(&conn, "en", "meeting", "").unwrap();
        update_note(
            &conn,
            &b.id,
            &NotePatch {
                title: Some("Hiring".into()),
                transcript: Some("Reviewed candidates for the engineering role.".into()),
                ..Default::default()
            },
        )
        .unwrap();

        let na = get_note(&conn, &a.id).unwrap();
        let nb = get_note(&conn, &b.id).unwrap();
        reindex_note(&conn, &a.id, &na.body, &na.transcript, &na.summary).unwrap();
        reindex_note(&conn, &b.id, &nb.body, &nb.transcript, &nb.summary).unwrap();

        let hits = hybrid_search_chunks(&conn, "budget", None, "", NoteFilter::default(), "", 10).unwrap();
        assert_eq!(hits.len(), 1, "only the budget note matches");
        assert_eq!(hits[0].note_id, a.id);
        assert_eq!(hits[0].note_title, "Budget planning");
        assert_eq!(hits[0].source, "transcript");

        // Punctuation / operator chars in the query don't blow up FTS5.
        let safe =
            hybrid_search_chunks(&conn, "budget: \"marketing\" -foo*", None, "", NoteFilter::default(), "", 10).unwrap();
        assert!(!safe.is_empty(), "sanitised query still matches");

        // A gibberish query returns nothing rather than erroring.
        assert!(hybrid_search_chunks(&conn, "!!!@@@", None, "", NoteFilter::default(), "", 10).unwrap().is_empty());
        assert!(hybrid_search_chunks(&conn, "zzzzznope", None, "", NoteFilter::default(), "", 10).unwrap().is_empty());
    }

    #[test]
    fn reindex_is_idempotent_and_reflects_edits() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("reindex.sqlite")).unwrap();
        let n = create_note(&conn, "en", "meeting", "").unwrap();

        reindex_note(&conn, &n.id, "hello world", "", "").unwrap();
        reindex_note(&conn, &n.id, "hello world", "", "").unwrap(); // twice
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM note_chunks WHERE note_id = ?1", params![n.id], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "reindex clears before rebuild — no duplication");

        // Old content is no longer findable after an edit; new content is.
        reindex_note(&conn, &n.id, "hello world", "", "").unwrap();
        assert_eq!(hybrid_search_chunks(&conn, "world", None, "", NoteFilter::default(), "", 10).unwrap().len(), 1);
        reindex_note(&conn, &n.id, "totally different", "", "").unwrap();
        assert!(hybrid_search_chunks(&conn, "world", None, "", NoteFilter::default(), "", 10).unwrap().is_empty());
        assert_eq!(hybrid_search_chunks(&conn, "different", None, "", NoteFilter::default(), "", 10).unwrap().len(), 1);
    }

    #[test]
    fn search_and_list_respect_folder_and_client_filters() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("filters.sqlite")).unwrap();
        let folder = create_folder(&conn, "Work", "").unwrap();
        let client = create_client(&conn, "Acme", "").unwrap();

        // Note in the folder AND tagged the client.
        let inside = create_note(&conn, "en", "meeting", "").unwrap();
        move_note(&conn, &inside.id, Some(&folder.id)).unwrap();
        set_note_client(&conn, &inside.id, Some(&client.id)).unwrap();
        update_note(
            &conn,
            &inside.id,
            &NotePatch { transcript: Some("shared keyword here".into()), ..Default::default() },
        )
        .unwrap();
        // Note with the same keyword but no folder/client.
        let outside = create_note(&conn, "en", "meeting", "").unwrap();
        update_note(
            &conn,
            &outside.id,
            &NotePatch { transcript: Some("shared keyword here too".into()), ..Default::default() },
        )
        .unwrap();

        for id in [&inside.id, &outside.id] {
            let nn = get_note(&conn, id).unwrap();
            reindex_note(&conn, id, &nn.body, &nn.transcript, &nn.summary).unwrap();
        }

        let f_folder = NoteFilter { folder_id: Some(&folder.id), ..Default::default() };
        let f_client = NoteFilter { client_id: Some(&client.id), ..Default::default() };
        let f_both =
            NoteFilter { folder_id: Some(&folder.id), client_id: Some(&client.id), ..Default::default() };
        let f_note = NoteFilter { note_id: Some(&outside.id), ..Default::default() };

        assert_eq!(hybrid_search_chunks(&conn, "keyword", None, "", NoteFilter::default(), "", 10).unwrap().len(), 2, "unfiltered sees both");
        let by_folder = hybrid_search_chunks(&conn, "keyword", None, "", f_folder, "", 10).unwrap();
        assert_eq!(by_folder.len(), 1);
        assert_eq!(by_folder[0].note_id, inside.id);
        // folder AND client combine (both narrow to the same single note).
        assert_eq!(hybrid_search_chunks(&conn, "keyword", None, "", f_both, "", 10).unwrap().len(), 1);
        // note_id clamp (the "this Note" breadth) pins search to one note.
        let by_note = hybrid_search_chunks(&conn, "keyword", None, "", f_note, "", 10).unwrap();
        assert_eq!(by_note.len(), 1);
        assert_eq!(by_note[0].note_id, outside.id);

        // list_notes_filtered mirrors the same combinable filters.
        assert_eq!(list_notes_filtered(&conn, NoteFilter::default(), "", 10).unwrap().len(), 2);
        assert_eq!(list_notes_filtered(&conn, f_folder, "", 10).unwrap().len(), 1);
        assert_eq!(list_notes_filtered(&conn, f_client, "", 10).unwrap().len(), 1);
    }

    // ── Semantic retrieval: embeddings + hybrid RRF (issue #48) ──────────────

    #[test]
    fn cosine_handles_identical_orthogonal_and_mismatched() {
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
        assert!((cosine(&[1.0, 2.0, 3.0], &[2.0, 4.0, 6.0]) - 1.0).abs() < 1e-6, "scale-invariant");
        assert_eq!(cosine(&[1.0, 0.0], &[1.0, 0.0, 0.0]), 0.0, "length mismatch → 0");
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 1.0]), 0.0, "zero vector → 0");
    }

    #[test]
    fn rrf_ranks_items_strong_in_both_lists_highest() {
        let kw = vec!["X".to_string(), "A".into(), "B".into()];
        let sem = vec!["X".to_string(), "C".into(), "D".into()];
        let fused = rrf_fuse(&[kw, sem], 60.0);
        assert_eq!(fused[0].0, "X", "top-of-both wins");
        // X's score is strictly greater than any single-list item's.
        let x = fused[0].1;
        assert!(fused[1..].iter().all(|(_, s)| *s < x));
    }

    #[test]
    fn embedding_cache_is_incremental_across_reindex() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("embed.sqlite")).unwrap();
        let n = create_note(&conn, "en", "meeting", "").unwrap();

        // transcript + summary chunk independently → two chunks.
        let write = |transcript: &str, summary: &str| {
            update_note(
                &conn,
                &n.id,
                &NotePatch {
                    transcript: Some(transcript.into()),
                    summary: Some(summary.into()),
                    ..Default::default()
                },
            )
            .unwrap();
            let fresh = get_note(&conn, &n.id).unwrap();
            reindex_note(&conn, &n.id, &fresh.body, &fresh.transcript, &fresh.summary).unwrap();
        };
        write("transcript about budgets", "summary about hiring");

        // Both chunks need embedding; embed them.
        let need = note_texts_needing_embedding(&conn, &n.id, "m").unwrap();
        assert_eq!(need.len(), 2);
        for (hash, _text) in &need {
            store_embedding(&conn, hash, "m", &[1.0, 0.0, 0.0]).unwrap();
        }
        assert!(note_texts_needing_embedding(&conn, &n.id, "m").unwrap().is_empty(), "all cached now");
        assert_eq!(count_embeddings(&conn, "m").unwrap(), 2);

        // Edit ONLY the summary; the transcript chunk is unchanged. Reindex
        // churns chunk ids, but the content-addressed cache means only the
        // changed text re-embeds.
        write("transcript about budgets", "GAMMA totally new summary text");
        let need2 = note_texts_needing_embedding(&conn, &n.id, "m").unwrap();
        assert_eq!(need2.len(), 1, "only the changed chunk re-embeds");
        assert!(need2[0].1.contains("GAMMA"));

        // A different model misses the cache entirely (fixed-dim-per-index).
        assert_eq!(note_texts_needing_embedding(&conn, &n.id, "other").unwrap().len(), 2);
    }

    #[test]
    fn hybrid_falls_back_to_keyword_without_a_query_vector() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("hybrid_kw.sqlite")).unwrap();
        let a = create_note(&conn, "en", "meeting", "").unwrap();
        update_note(&conn, &a.id, &NotePatch { transcript: Some("quarterly financial planning".into()), ..Default::default() }).unwrap();
        let na = get_note(&conn, &a.id).unwrap();
        reindex_note(&conn, &a.id, &na.body, &na.transcript, &na.summary).unwrap();

        // No query vector → keyword-only, still finds the term.
        let hits = hybrid_search_chunks(&conn, "financial", None, "m", NoteFilter::default(), "", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].note_id, a.id);
    }

    #[test]
    fn hybrid_surfaces_a_semantic_only_match_via_rrf() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("hybrid_sem.sqlite")).unwrap();
        // note A matches the keyword "budget"; note B does not, but is a near-
        // synonym match by meaning (we give it a vector identical to the query).
        let a = create_note(&conn, "en", "meeting", "").unwrap();
        update_note(&conn, &a.id, &NotePatch { transcript: Some("the budget was approved".into()), ..Default::default() }).unwrap();
        let b = create_note(&conn, "en", "meeting", "").unwrap();
        update_note(&conn, &b.id, &NotePatch { transcript: Some("we discussed spending limits".into()), ..Default::default() }).unwrap();
        for id in [&a.id, &b.id] {
            let nn = get_note(&conn, id).unwrap();
            reindex_note(&conn, id, &nn.body, &nn.transcript, &nn.summary).unwrap();
        }

        // Query vector = [1,0,0]. Give B's chunk that exact vector (cosine 1.0),
        // A's an orthogonal one (cosine 0) — so semantic ranks B above A.
        let embed = |note_id: &str, v: &[f32]| {
            for (hash, _t) in note_texts_needing_embedding(&conn, note_id, "m").unwrap() {
                store_embedding(&conn, &hash, "m", v).unwrap();
            }
        };
        embed(&a.id, &[0.0, 1.0, 0.0]);
        embed(&b.id, &[1.0, 0.0, 0.0]);

        let qv = [1.0f32, 0.0, 0.0];
        let hits = hybrid_search_chunks(&conn, "budget", Some(&qv), "m", NoteFilter::default(), "", 10).unwrap();
        let note_ids: Vec<&str> = hits.iter().map(|h| h.note_id.as_str()).collect();
        // Keyword finds A; semantic surfaces B. Hybrid returns BOTH — the
        // semantic-only match B would be invisible to keyword search alone.
        assert!(note_ids.contains(&a.id.as_str()), "keyword match present");
        assert!(note_ids.contains(&b.id.as_str()), "semantic-only match surfaced by RRF");
    }
}
