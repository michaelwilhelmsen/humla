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
    // Speaker-aware retrieval (issue #104). Two derived columns, both written only
    // by `reindex_note` from the transcript text, both delimiter-wrapped by
    // `encode_speakers`: `notes.speakers` backs a note-level filter and lets
    // listing rows name who spoke (NoteMeta deliberately excludes the transcript),
    // `note_chunks.speakers` backs the chunk-level one.
    //
    // Local-only and NOT synced, deliberately: the server derives its own from the
    // transcript it already holds, so no new PB field exists and no structured list
    // of named third parties enters the shared collection (ADR-0002). They are a
    // cache, not a record — rebuildable from the text at any time, and destroyed
    // with the note that produced them.
    let _ = conn.execute("ALTER TABLE notes ADD COLUMN speakers TEXT NOT NULL DEFAULT ''", []);
    let _ =
        conn.execute("ALTER TABLE note_chunks ADD COLUMN speakers TEXT NOT NULL DEFAULT ''", []);
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
    // A pinned authorship filter (#103). Empty = off, which is the only safe
    // back-fill: an existing conversation was answered unfiltered, so it must keep
    // answering unfiltered.
    let _ = conn.execute(
        "ALTER TABLE conversations ADD COLUMN owner_filter TEXT NOT NULL DEFAULT ''",
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

/// One folder's display name, or `None` when the id matches nothing.
///
/// Added for #113's breadth disclosure: the chat prompt has to NAME the folder it is
/// confined to, and `ToolScope::Folder` carries only the id. `Ok(None)` for a missing
/// row rather than an error, because a stale scope id must degrade to saying nothing
/// — never to failing the turn.
pub fn folder_name(conn: &Connection, id: &str) -> Result<Option<String>> {
    let found = conn
        .query_row("SELECT name FROM folders WHERE id = ?1", params![id], |r| {
            r.get::<_, String>(0)
        })
        .optional()?;
    Ok(found)
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
    // Take the derived index with it (issue #121). Purge is the point of no return,
    // and until this landed the note's chunk + FTS rows survived as orphans: never
    // reachable by search (the hit query inner-joins live notes) but still holding
    // the transcript text, speaker names included, after the user performed the one
    // action the UI presents as permanent.
    //
    // ADR-0002 makes this a rule rather than tidiness: "delete the note" is the
    // answer to erasing a person, which only holds if derived person data is
    // destroyed rather than hidden. The cloud indexer already did this on a
    // tombstone; this is local catching up.
    //
    // `chunk_embeddings` is deliberately left alone — keyed `(text_hash, model)`
    // with no note linkage, so it is shared by any notes with identical text and
    // cannot be deleted per note. It stores a vector, not text, so it holds no name.
    remove_note_chunks(conn, id)?;
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

/// General chat scope — the string stored in `conversations.scope`, chosen in
/// one place so later scopes (folder/client) add variants without touching call
/// sites. `global` arrived with #93; folder and client are still unbuilt.
pub const CHAT_SCOPE_NOTE: &str = "note";
pub const CHAT_SCOPE_GLOBAL: &str = "global";
/// `scope_id` for the one global conversation set per tenant. A fixed sentinel
/// rather than an empty string: the composite key is `(tenant, scope, scope_id)`,
/// and an empty id already reads as "absent" in too many places to also mean
/// "the whole library". Never shown to the user, never sent to the server — the
/// cloud carries its own `scope` field (humla-cloud#26).
pub const CHAT_GLOBAL_SCOPE_ID: &str = "__global__";
/// Only Personal is used in this slice; workspace tenants arrive with Teams.
pub const CHAT_TENANT_PERSONAL: &str = "personal";

/// What a chat conversation is *about*: one Note, or the whole library.
///
/// This is the single place `(scope, scope_id)` is derived, so a caller can no
/// longer accidentally pair a global scope with a note's id. Constructed from the
/// IPC boundary via [`ChatTarget::from_note_id`], where **absent** means global
/// and **empty** is an error — the distinction that keeps `""` from quietly
/// becoming "everything" (the same trap humla-cloud#26 avoided server-side).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatTarget {
    Note(String),
    Global,
}

impl ChatTarget {
    /// Parse the optional note id an IPC command received.
    pub fn from_note_id(note_id: Option<String>) -> Result<Self, String> {
        match note_id {
            None => Ok(Self::Global),
            Some(id) if id.trim().is_empty() => Err(
                "A chat target needs a note id, or none at all for the whole library — \
                 an empty id is neither."
                    .into(),
            ),
            Some(id) => Ok(Self::Note(id)),
        }
    }

    pub fn scope(&self) -> &'static str {
        match self {
            Self::Note(_) => CHAT_SCOPE_NOTE,
            Self::Global => CHAT_SCOPE_GLOBAL,
        }
    }

    pub fn scope_id(&self) -> &str {
        match self {
            Self::Note(id) => id,
            Self::Global => CHAT_GLOBAL_SCOPE_ID,
        }
    }

    /// The anchor note id, or None for a global target. Callers that genuinely
    /// need a note (grounding, folder breadth) go through this and handle None
    /// rather than reaching for `scope_id`.
    pub fn note_id(&self) -> Option<&str> {
        match self {
            Self::Note(id) => Some(id),
            Self::Global => None,
        }
    }
}

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
    /// A pinned authorship filter (#103): the user id whose notes this
    /// conversation retrieves from, or empty for no filter.
    ///
    /// A user id rather than a flag, because a workspace's conversation list is
    /// visible to every member — a boolean "only mine" would mean different notes
    /// to different readers of the same thread. Storing the person keeps one
    /// meaning per conversation, and lets the chip name them ("Created by Anna")
    /// instead of implying "you".
    pub owner_filter: String,
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
        owner_filter: row.get(6)?,
        title: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

const CONVERSATION_COLS: &str =
    "id, scope, scope_id, tenant, remote_id, breadth, owner_filter, title, created_at, updated_at";

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
/// A window over a conversation list, most-recent first (issue #95).
///
/// `/chat` lists conversations in the sidebar with no cap — a workspace could
/// accumulate hundreds — so the list is fetched a page at a time as the user
/// scrolls rather than all at once.
#[derive(Debug, Clone, Copy)]
pub struct Page {
    pub limit: i64,
    pub offset: i64,
}

/// Which conversations a listing should include (issue #120).
///
/// `WithMessages` hides threads that hold nothing. It exists because a
/// library-wide pane now opens on an unsaved draft, so an empty row on `/chat`
/// can only be residue — one left by an older client, or by a breadth chosen and
/// then abandoned — never something the user can return to. In a Note the
/// opposite is true (an empty thread is exactly the draft the tab resumes), which
/// is why this is a parameter and not the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListFilter {
    All,
    WithMessages,
}

pub fn list_conversations(
    conn: &Connection,
    tenant: &str,
    scope: &str,
    scope_id: &str,
    page: Option<Page>,
    filter: ListFilter,
) -> Result<Vec<Conversation>> {
    // SQLite reads a negative LIMIT as "no limit", so an absent page needs no
    // second query shape — one prepared statement serves both callers.
    let (limit, offset) = page.map_or((-1, 0), |p| (p.limit, p.offset));
    // Filtered in SQL, deliberately, not by the caller after the fact: the sidebar
    // reads a short page as the end of the list, so dropping rows from an
    // already-windowed page would let one hidden draft masquerade as the end and
    // truncate everything below it.
    let having = match filter {
        ListFilter::All => "",
        ListFilter::WithMessages => {
            " AND EXISTS (SELECT 1 FROM messages WHERE messages.conversation_id = conversations.id)"
        }
    };
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {CONVERSATION_COLS} FROM conversations
         WHERE tenant = ?1 AND scope = ?2 AND scope_id = ?3{having}
         ORDER BY updated_at DESC, created_at DESC, id DESC
         LIMIT ?4 OFFSET ?5",
    ))?;
    let rows = stmt
        .query_map(params![tenant, scope, scope_id, limit, offset], map_conversation)?
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

/// Delete a conversation and its messages (issue #109).
///
/// A HARD delete, unlike [`delete_note`]'s `deleted_at` tombstone, and the
/// asymmetry is deliberate. A note tombstone exists to serve two things this
/// table doesn't have: a Trash the user can restore from, and last-write-wins
/// reconciliation against a synced copy. Conversations are never touched by the
/// sync worker — a Personal conversation exists nowhere else, and a workspace
/// one is server-authoritative with this row acting only as a handle — so a
/// tombstone here would buy nothing and leave rows the list has to filter
/// forever. The confirm step in the UI is what stands in for the missing undo.
///
/// Messages go first so a failure can't strand them: the orphan check is
/// `conversation_id`, and a message whose conversation is gone would never be
/// read or cleaned up again. Deleting a row that doesn't exist is a no-op, which
/// keeps the command idempotent under a double-click or a retry.
pub fn delete_conversation(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("DELETE FROM messages WHERE conversation_id = ?1", params![id])?;
    conn.execute("DELETE FROM conversations WHERE id = ?1", params![id])?;
    Ok(())
}

/// Rename a conversation (issue #109) — the user's override of the title
/// `derive_title` guessed from the first turn.
///
/// Bumps `updated_at`, which needs saying because it has a visible cost: the
/// lists order by it, so a rename moves the row to the top. That's the right
/// trade — `updated_at` is what the workspace read-through and the recency
/// ordering both key off, and a rename that left it stale would let a later
/// reconcile treat the server's copy as newer and overwrite the new title.
///
/// No `title_locked` column is needed to protect this from a later turn: the
/// send path only titles a conversation when `resolved_title_is_unset` (see
/// `commands/chat.rs`), so a non-empty title is already never rewritten.
pub fn rename_conversation(conn: &Connection, id: &str, title: &str) -> Result<()> {
    conn.execute(
        "UPDATE conversations SET title = ?1, updated_at = ?2 WHERE id = ?3",
        params![title, now_ms(), id],
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

/// Pin (or clear) the conversation's authorship filter (#103). `None` clears.
/// Stored as the user id whose notes the conversation retrieves — see
/// [`Conversation::owner_filter`] for why it isn't a boolean.
pub fn set_conversation_owner_filter(
    conn: &Connection,
    id: &str,
    owner: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE conversations SET owner_filter = ?1, updated_at = ?2 WHERE id = ?3",
        params![owner.unwrap_or(""), now_ms(), id],
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
    /// Position of this chunk within its Note. Carried so a grouped result can put a
    /// note's excerpts back into READING order (#104's seventh decision): ranked
    /// order scrambles them, which garbles any "what was decided" narrative the
    /// model tries to reconstruct from several excerpts of one meeting.
    pub seq: i64,
}

/// Lightweight Note descriptor for the `list_notes` tool — just what a citation
/// or a "which note?" decision needs, never the full body/transcript. `summary`
/// is the one exception (#81): it lets the model skim candidates without spending
/// a `get_note` on each, and the tool layer caps how much of it is shown.
#[derive(Debug, Clone, Serialize)]
pub struct NoteMeta {
    pub id: String,
    pub title: String,
    pub created_at: i64,
    pub folder_id: Option<String>,
    pub client_id: Option<String>,
    /// The tagged Client's display name, resolved at query time. Carried because a
    /// bare `client_id` is unusable to the model: it can only pass an id it has
    /// seen, and a per-client answer has to name the client in prose (#105).
    pub client_name: Option<String>,
    pub summary: String,
    /// Who spoke in this note, from the derived `notes.speakers` column (#104).
    /// Carried for the same reason as `client_name`: `NoteMeta` deliberately
    /// excludes the transcript, so without this a listing row cannot name a single
    /// speaker — and the model can then only pass a `speaker` filter for a name it
    /// has actually seen, rather than one it guessed (#105's lesson).
    pub speakers: Vec<String>,
}

/// A search's hits *and* how many notes actually matched — two different numbers
/// the model needs separately (#106). Eight excerpts out of forty matching notes
/// and eight out of eight look identical otherwise, and absence from a top-k list
/// reads exactly like absence from the library.
///
/// `matched_notes` is exact (see [`count_matching_notes`]); `None` means the query
/// had no countable predicate, which callers report as unknown, never as zero.
#[derive(Debug, Clone)]
pub struct SearchOutcome {
    pub hits: Vec<ChunkHit>,
    pub matched_notes: Option<usize>,
    /// How many candidate excerpts each note contributed BEFORE the per-note
    /// diversity cap trimmed them, keyed by note id (#104).
    ///
    /// Needed so a grouped result can say "2 of 7" honestly and name the cap as the
    /// reason — without it, showing 2 of a note's excerpts reads as "the other 5
    /// didn't match" when the diversity cap is the real limiter. Measures the same
    /// fused candidate set the hits are drawn from, so the two numbers describe one
    /// set rather than two (#106's counting trap).
    pub per_note_matched: std::collections::HashMap<String, usize>,
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
    /// Optional lower bound on note creation time (ms epoch) — backs the retrieval
    /// tools' relative date window (#81). Narrows only; the breadth clamp still wins.
    pub since_ms: Option<i64>,
    /// Optional UPPER bound on note creation time (ms epoch), EXCLUSIVE — the other
    /// half of a bounded window (#106). Exclusive so successive windows tile without
    /// double-counting the note that sits exactly on the boundary.
    pub until_ms: Option<i64>,
    /// Optional speaker label (#104). Matched EXACTLY against the wrapped derived
    /// column, so `Michael` never reaches `Michael Berg` — substring matching would
    /// merge two people into one confident wrong answer.
    ///
    /// Applied at both levels: notes where they spoke (listings, counts) and, in
    /// chunk search, the passages they spoke in. So a hit means *"they spoke here"*,
    /// not *"they said this"* — right for "what did Hege commit to", wrong for
    /// counting who talked most.
    pub speaker: Option<&'a str>,
    /// A SECOND label that counts as the same person as `speaker` (#104).
    ///
    /// Exists for the `You:` sentinel. Mic chunks on remote calls are labelled with
    /// the literal "You", so the app user's own speech is stored under two different
    /// labels across a library: "You" wherever the diarizer wrote it, and their real
    /// name wherever they renamed it. Filtering for one and silently missing the other
    /// is a wrong answer that looks complete, so a filter may name both.
    ///
    /// Ignored unless `speaker` is set. The transcript text is never rewritten — this
    /// is the query-side half of resolving the sentinel.
    pub speaker_alias: Option<&'a str>,
}

/// The maximum length of a speaker label, in chars. Mirrors the `{1,40}` bound in
/// the frontend's `extractSpeakerLabels` regex — see [`parse_speaker_turns`].
const SPEAKER_LABEL_MAX_CHARS: usize = 40;

/// One line of a transcript, split into its speaker label (if it has one) and the
/// full original text of the line.
///
/// `text` is verbatim and INCLUDES the label, because labels stay inline: a chunk
/// built from these turns self-describes to the model with no rewriting, and stays
/// a faithful slice of what the user reads (issue #104).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    /// The asserted speaker, or `None` for a line with no label — an import, or a
    /// live transcript before the post-stop diarize pass adds labels.
    pub speaker: Option<String>,
    pub text: String,
}

/// A chunk of one Note source, with the speakers it contains.
///
/// `speakers` is derived from the text and only ever populated for the transcript
/// source; it is empty for body and summary, which have no turn structure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteChunk {
    pub source: &'static str,
    pub text: String,
    /// Distinct speakers in this chunk, in first-encounter order.
    pub speakers: Vec<String>,
}

/// Append `value` unless it is already present, preserving first-encounter order.
///
/// Speaker lists are short (a handful per note) and their ORDER is meaningful — the
/// first voice is usually whoever opened the meeting — so a linear scan over a `Vec`
/// beats a `HashSet` plus a second pass to recover ordering. Used wherever speakers
/// are accumulated, so "distinct, in first-encounter order" is defined once.
fn push_distinct(out: &mut Vec<String>, value: &str) {
    if !out.iter().any(|seen| seen == value) {
        out.push(value.to_string());
    }
}

/// Wrap a chunk's speakers for storage as `|Michael|Hege|`, or `""` for none.
///
/// Delimiter-wrapped rather than a join, so an exact-label match is a single
/// `LIKE '%|Michael|%'` that cannot also match `Michael Berg` — substring matching
/// would merge two people into one confident wrong answer (#104). The cost is that
/// the column can't be substring-indexed, which is why #116's autocomplete wants
/// its own derived table rather than querying this one.
///
/// A `|` inside a label would break the encoding, so it is normalised to a space
/// **in this derived column only** — the transcript text is never rewritten.
pub fn encode_speakers(speakers: &[String]) -> String {
    if speakers.is_empty() {
        return String::new();
    }
    let mut out = String::from("|");
    for s in speakers {
        out.push_str(&s.replace('|', " "));
        out.push('|');
    }
    out
}

/// Read back what [`encode_speakers`] wrote. Tolerates `""` and a bare unwrapped
/// value, so a hand-edited or half-migrated row can't panic a search.
pub fn decode_speakers(encoded: &str) -> Vec<String> {
    encoded
        .split('|')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Split a transcript into speaker turns, one per line.
///
/// **This is one of three copies of the same parse** — the others are
/// `extractSpeakerLabels` in `src/lib/speakers.ts` (frontend) and the indexer's
/// parse in `humla-cloud/chat-service`. They must agree, so the rule is pinned
/// here verbatim and asserted by [`tests::parse_speaker_turns_mirrors_the_frontend_label_rule`]:
///
/// > a label is up to 40 non-colon chars at the start of the line (after leading
/// > whitespace), followed by a colon and then whitespace.
///
/// The trailing-whitespace requirement is what stops `12:30 standup` and
/// `https://example.com` being read as speakers.
pub fn parse_speaker_turns(transcript: &str) -> Vec<Turn> {
    transcript
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| Turn { speaker: speaker_label(line), text: line.to_string() })
        .collect()
}

/// The speaker label of one line, or `None`. See [`parse_speaker_turns`] for the
/// rule this pins.
fn speaker_label(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    // Take up to the bound in chars (not bytes — a Norwegian name is not ASCII).
    let mut label = String::new();
    let mut chars = trimmed.chars();
    loop {
        match chars.next() {
            // A colon closes the candidate label; it must be followed by whitespace.
            Some(':') => {
                return match chars.next() {
                    Some(next) if next.is_whitespace() => {
                        let label = label.trim();
                        (!label.is_empty()).then(|| label.to_string())
                    }
                    _ => None,
                };
            }
            Some(c) if label.chars().count() < SPEAKER_LABEL_MAX_CHARS => label.push(c),
            // Ran past the bound, or ran out of line, without finding a colon.
            _ => return None,
        }
    }
}

/// Pack whole turns into ~`target`-char chunks, so a chunk boundary never lands
/// mid-turn and every chunk carries the labels of the speech inside it (#104).
///
/// A turn longer than `target` is hard-split, and its continuations are prefixed
/// `Name (continued): ` so a mid-turn chunk still says who is speaking. That
/// prefix is the one place a chunk's text is not verbatim, and it pushes a
/// continuation slightly past `target` — acceptable, since `target` is a soft
/// budget well inside the model's window.
pub fn pack_turns(turns: &[Turn], target: usize) -> Vec<NoteChunk> {
    let target = target.max(1);
    let mut out: Vec<NoteChunk> = Vec::new();
    let mut lines: Vec<&str> = Vec::new();
    let mut speakers: Vec<String> = Vec::new();
    let mut len = 0usize;

    for turn in turns {
        let turn_len = turn.text.chars().count();

        // An overlong turn cannot share a chunk with anything: close what's open,
        // then hard-split it on its own.
        if turn_len > target {
            if !lines.is_empty() {
                out.push(transcript_chunk(&lines, std::mem::take(&mut speakers)));
                lines.clear();
                len = 0;
            }
            out.extend(split_long_turn(turn, target));
            continue;
        }

        // `+ 1` for the newline this turn would be joined on.
        if !lines.is_empty() && len + 1 + turn_len > target {
            out.push(transcript_chunk(&lines, std::mem::take(&mut speakers)));
            lines.clear();
            len = 0;
        }

        if !lines.is_empty() {
            len += 1;
        }
        len += turn_len;
        lines.push(&turn.text);
        if let Some(s) = &turn.speaker {
            push_distinct(&mut speakers, s);
        }
    }
    if !lines.is_empty() {
        out.push(transcript_chunk(&lines, speakers));
    }
    out
}

/// Assemble one transcript chunk, rejoining turns on the single newline they were
/// separated by.
///
/// The text is the transcript's own words, unaltered, but not a byte-for-byte slice:
/// `parse_speaker_turns` drops blank lines (they carry no speech, and the paragraph
/// splitter dropped them too), so a transcript containing them — an import, or
/// pasted text — rejoins without them. Speech is never changed; only empty lines go.
/// The other, louder departure is the `(continued)` prefix in [`split_long_turn`].
fn transcript_chunk(lines: &[&str], speakers: Vec<String>) -> NoteChunk {
    NoteChunk { source: "transcript", text: lines.join("\n"), speakers }
}

/// Hard-split one over-budget turn, naming the speaker on every continuation so a
/// mid-turn chunk is still attributable. The first piece needs no prefix — it
/// already opens with the real label.
fn split_long_turn(turn: &Turn, target: usize) -> Vec<NoteChunk> {
    let speakers: Vec<String> = turn.speaker.clone().map(|s| vec![s]).unwrap_or_default();
    let prefix = turn.speaker.as_ref().map(|s| format!("{s} (continued): "));
    let chars: Vec<char> = turn.text.chars().collect();
    chars
        .chunks(target)
        .enumerate()
        .map(|(i, window)| {
            let body: String = window.iter().collect();
            let text = match (i, prefix.as_deref()) {
                (0, _) | (_, None) => body,
                (_, Some(p)) => format!("{p}{body}"),
            };
            NoteChunk { source: "transcript", text, speakers: speakers.clone() }
        })
        .collect()
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
/// The transcript takes a different path from body and summary, and that asymmetry
/// is the point (#104): its turns are separated by SINGLE newlines, which the
/// blank-line paragraph splitter cannot see — so a transcript arrived as one
/// "paragraph", blew past the target, and fell through to arbitrary char slicing
/// that cut mid-word and stranded each speaker's label in a previous chunk. Prose
/// really does have blank lines, so it keeps the splitter that suits it, and claims
/// no speakers: `Note: buy milk` typed in a body is not a person who spoke.
pub fn note_chunk_texts(body_text: &str, transcript: &str, summary: &str) -> Vec<NoteChunk> {
    let mut out: Vec<NoteChunk> = Vec::new();
    for chunk in split_into_chunks(body_text, CHUNK_TARGET_CHARS) {
        out.push(NoteChunk { source: "body", text: chunk, speakers: Vec::new() });
    }
    out.extend(pack_turns(&parse_speaker_turns(transcript), CHUNK_TARGET_CHARS));
    for chunk in split_into_chunks(summary, CHUNK_TARGET_CHARS) {
        out.push(NoteChunk { source: "summary", text: chunk, speakers: Vec::new() });
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
    // The note-level set is the union of what the chunks carry, in first-encounter
    // order — derived from the same parse, so the two columns cannot disagree.
    let mut note_speakers: Vec<String> = Vec::new();
    for chunk in &chunks {
        for s in &chunk.speakers {
            push_distinct(&mut note_speakers, s);
        }
    }
    for (seq, chunk) in chunks.iter().enumerate() {
        let NoteChunk { source, text, speakers } = chunk;
        let chunk_id = uuid::Uuid::new_v4().to_string();
        let hash = text_hash(text);
        conn.execute(
            "INSERT INTO note_chunks (id, note_id, seq, source, text, text_hash, created_at, speakers)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![chunk_id, note_id, seq as i64, source, text, hash, now, encode_speakers(speakers)],
        )?;
        conn.execute(
            "INSERT INTO note_chunks_fts (text, chunk_id, note_id) VALUES (?1, ?2, ?3)",
            params![text, chunk_id, note_id],
        )?;
    }
    // Deliberately does NOT touch `updated_at`. Reindex runs on every settled
    // content change, so bumping it would mark the note dirty over derived data and
    // hand the sync worker an endless stream of no-op pushes.
    conn.execute(
        "UPDATE notes SET speakers = ?1 WHERE id = ?2",
        params![encode_speakers(&note_speakers), note_id],
    )?;
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

/// Every live note's id, oldest first — the work-list for the user-triggered
/// "rebuild search index" action (#104).
///
/// Distinct from [`note_ids_needing_reindex`], which is a *lazy* startup backfill
/// keyed on sentinels for notes that were never indexed. A chunking-shape change
/// invalidates notes that look perfectly indexed, and there is no sentinel for
/// "chunked before turn-packing" — so repairing an existing library means walking
/// all of them. Deliberately NOT wired into startup: re-chunking changes every
/// chunk's `text_hash`, which misses the embedding cache and re-embeds the whole
/// library on the user's own API key. Cheap in absolute terms (cents), but not
/// something to spend unasked, so the user asks for it.
///
/// Oldest first because old meetings are exactly what a "briefing on X" query
/// needs, and they are the ones that would otherwise never be re-opened.
pub fn live_note_ids(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT id FROM notes WHERE deleted_at IS NULL ORDER BY created_at ASC")?;
    let ids = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(ids)
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

/// The keyword leg's FROM/WHERE, shared by the ranking query and the match count so
/// the two can't drift apart on what "matching" means — the count would then describe
/// a different set from the hits it is printed above. `?1` is the FTS match
/// expression, `?2` the workspace.
const KEYWORD_FROM_WHERE: &str = "FROM note_chunks_fts f \
     JOIN note_chunks c ON c.id = f.chunk_id \
     JOIN notes n ON n.id = c.note_id \
     WHERE note_chunks_fts MATCH ?1 AND n.workspace_id = ?2 AND n.deleted_at IS NULL";

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
    let mut sql = format!(
        "SELECT c.id, n.id, n.title, n.created_at, c.source, c.text, bm25(note_chunks_fts) AS rank, \
                c.seq \
         {KEYWORD_FROM_WHERE}"
    );
    let mut args: Vec<Value> = vec![Value::Text(match_expr), Value::Text(workspace.to_string())];
    push_chunk_filters(&mut sql, &mut args, filter);
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
                    seq: row.get(7)?,
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
        "SELECT c.id, n.id, n.title, n.created_at, c.source, c.text, e.vector, c.seq \
         FROM note_chunks c \
         JOIN chunk_embeddings e ON e.text_hash = c.text_hash AND e.model = ?1 \
         JOIN notes n ON n.id = c.note_id \
         WHERE n.workspace_id = ?2 AND n.deleted_at IS NULL",
    );
    let mut args: Vec<Value> = vec![Value::Text(model.to_string()), Value::Text(workspace.to_string())];
    push_chunk_filters(&mut sql, &mut args, filter);
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
                        seq: row.get(7)?,
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
    // Date window on `created_at` (when the meeting happened), not `updated_at` —
    // "what came up last week" asks about meetings, not about when a note was
    // last edited.
    if let Some(since) = filter.since_ms {
        args.push(Value::Integer(since));
        sql.push_str(&format!(" AND {t}.created_at >= ?{}", args.len()));
    }
    // The upper bound is EXCLUSIVE against the lower bound's inclusive `>=`, so
    // adjacent windows ([a,b) then [b,c)) tile with neither a gap nor a note
    // counted twice — the property "compare this week with the previous four
    // weeks" depends on (#106).
    if let Some(until) = filter.until_ms {
        args.push(Value::Integer(until));
        sql.push_str(&format!(" AND {t}.created_at < ?{}", args.len()));
    }
    if let Some(speaker) = filter.speaker {
        push_speaker_clause(sql, args, speaker, filter.speaker_alias, t);
    }
}

/// The filter set for a query that selects CHUNKS (`c`) joined to notes (`n`).
///
/// Every note-level narrowing applies as usual, but `speaker` moves to the chunk:
/// an excerpt should come back only if that speaker spoke in that passage, where
/// note-level would return every excerpt from any meeting they attended. All three
/// chunk queries — keyword hits, semantic candidates and the match count — must
/// narrow identically, or the count describes a different set from the hits (#106),
/// so they share this one call rather than three copies of the same two steps.
fn push_chunk_filters(
    sql: &mut String,
    args: &mut Vec<rusqlite::types::Value>,
    filter: NoteFilter<'_>,
) {
    push_note_filters(sql, args, NoteFilter { speaker: None, speaker_alias: None, ..filter }, "n");
    if let Some(speaker) = filter.speaker {
        push_speaker_clause(sql, args, speaker, filter.speaker_alias, "c");
    }
}

/// Narrow to rows whose derived `speakers` column contains `speaker` as a WHOLE
/// label, against either alias — `n` for notes ("they spoke in this meeting"), `c`
/// for chunks ("they spoke in this passage").
///
/// The wrapping is what makes it exact: the needle is built as `%|Label|%`, and
/// since [`encode_speakers`] wraps every stored label in the same delimiters,
/// `|Michael|` cannot occur inside `|Michael Berg|`.
///
/// Case folding is SQLite `LIKE`'s, which is ASCII-only — so `hege`/`Hege` match
/// but a label differing only in the case of a non-ASCII character (`Ærlig`/`ærlig`)
/// would not. Accepted: labels reach this filter having been read off a listing
/// row, so they arrive spelled as stored, and the alternative is a second
/// lowercased mirror column purely for folding.
fn push_speaker_clause(
    sql: &mut String,
    args: &mut Vec<rusqlite::types::Value>,
    speaker: &str,
    alias: Option<&str>,
    t: &str,
) {
    use rusqlite::types::Value;
    // Neutralise LIKE's own wildcards so a name containing % or _ can't widen the
    // match, via an explicit ESCAPE clause.
    let mut clauses: Vec<String> = Vec::new();
    for label in [Some(speaker), alias].into_iter().flatten() {
        let escaped = label
            .trim()
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
            .replace('|', " ");
        if escaped.is_empty() {
            continue;
        }
        args.push(Value::Text(format!("%|{escaped}|%")));
        clauses.push(format!("{t}.speakers LIKE ?{} ESCAPE '\\'", args.len()));
    }
    if clauses.is_empty() {
        return;
    }
    // Parenthesised: an unbracketed OR would let the second label escape every
    // preceding AND and match notes the breadth clamp had already excluded.
    sql.push_str(&format!(" AND ({})", clauses.join(" OR ")));
}

/// The distinct speakers present in a scope, so an unmatched `speaker` argument can
/// be answered with the names that DO exist rather than an empty result (#106: a
/// model handed a bare zero reports absence; one handed the real options
/// self-corrects). Ordered for a stable, readable list.
pub fn speakers_in_scope(
    conn: &Connection,
    filter: NoteFilter<'_>,
    workspace_id: &str,
) -> Result<Vec<String>> {
    let mut sql = String::from(
        "SELECT speakers FROM notes n WHERE n.workspace_id = ?1 AND n.deleted_at IS NULL \
         AND n.speakers != ''",
    );
    let mut args: Vec<rusqlite::types::Value> =
        vec![rusqlite::types::Value::Text(workspace_id.to_string())];
    // The speaker field itself is dropped: we are asking what else is there.
    push_note_filters(&mut sql, &mut args, NoteFilter { speaker: None, speaker_alias: None, ..filter }, "n");

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(args), |r| r.get::<_, String>(0))?;
    let mut out: Vec<String> = Vec::new();
    for encoded in rows {
        for s in decode_speakers(&encoded?) {
            push_distinct(&mut out, &s);
        }
    }
    out.sort_unstable();
    Ok(out)
}

/// The size of each per-signal candidate pool fused by RRF. Bigger than the
/// final `limit` so a chunk ranked mid-pack by one signal can still win on the
/// other.
/// Matches the cloud index's candidate pool (`K = 200` in `store.ts`), so the
/// diversity pass below chooses from a comparably wide field on both sides —
/// otherwise the same per-note cap yields materially less coverage locally.
const HYBRID_POOL: usize = 200;
/// RRF damping constant (standard default).
const RRF_K: f64 = 60.0;
/// Max chunks any ONE note may contribute to a result's first pass (#81).
const PER_NOTE_CAP: usize = 2;

/// Two-pass take over rank-ordered hits: first at most `per_note_cap` per note,
/// then backfill with the best remaining hits until `limit`.
///
/// Chunks are per-section, so a plain `take(limit)` can spend every slot on the
/// two or three notes that happen to rank best — narrow coverage for a question
/// spanning the whole library. The cap fixes that; the backfill is what stops it
/// costing recall when only one note matches (a note-scoped search must still be
/// able to return several excerpts of its one note).
fn diversify(
    ranked: Vec<ChunkHit>,
    limit: usize,
    per_note_cap: usize,
) -> (Vec<ChunkHit>, std::collections::HashMap<String, usize>) {
    use std::collections::HashMap;
    // Counted over the WHOLE ranked candidate set, before any capping or the early
    // return below, so it stays an honest denominator for "showing N of M".
    let mut matched: HashMap<String, usize> = HashMap::new();
    for hit in &ranked {
        *matched.entry(hit.note_id.clone()).or_insert(0) += 1;
    }
    let mut out: Vec<ChunkHit> = Vec::with_capacity(limit.min(ranked.len()));
    let mut held: Vec<ChunkHit> = Vec::new();
    let mut per_note: HashMap<String, usize> = HashMap::new();
    for hit in ranked {
        if out.len() >= limit {
            return (out, matched);
        }
        let taken = per_note.entry(hit.note_id.clone()).or_insert(0);
        if *taken < per_note_cap {
            *taken += 1;
            out.push(hit);
        } else {
            held.push(hit);
        }
    }
    for hit in held {
        if out.len() >= limit {
            break;
        }
        out.push(hit);
    }
    (out, matched)
}

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
) -> Result<SearchOutcome> {
    // Counted under the SAME filter as the hits, in the same call, so the two
    // numbers can never describe different question shapes.
    let matched_notes = count_matching_notes(conn, query, filter, workspace)?;
    let keyword = keyword_ranked(conn, query, filter, workspace, HYBRID_POOL)?;
    // Keyword-only: no query embedding, or nothing embedded yet under this model
    // (issue #48 graceful degradation).
    let keyword_only = |kw: Vec<IdentifiedHit>| {
        let (hits, per_note_matched) =
            diversify(kw.into_iter().map(|ih| ih.hit).collect(), limit, PER_NOTE_CAP);
        Ok(SearchOutcome { hits, matched_notes, per_note_matched })
    };
    let Some(qv) = query_vec else {
        return keyword_only(keyword);
    };
    let semantic = semantic_ranked(conn, qv, model, filter, workspace, HYBRID_POOL)?;
    if semantic.is_empty() {
        return keyword_only(keyword);
    }

    // Fuse by chunk id; keep a lookup so the winners map back to their hits.
    use std::collections::HashMap;
    let mut hits: HashMap<String, ChunkHit> = HashMap::new();
    let kw_ids: Vec<String> = keyword.into_iter().map(|ih| { hits.insert(ih.chunk_id.clone(), ih.hit); ih.chunk_id }).collect();
    let sem_ids: Vec<String> = semantic.into_iter().map(|ih| { hits.entry(ih.chunk_id.clone()).or_insert(ih.hit); ih.chunk_id }).collect();
    let fused = rrf_fuse(&[kw_ids, sem_ids], RRF_K);
    let ranked: Vec<ChunkHit> = fused
        .into_iter()
        .filter_map(|(id, score)| hits.remove(&id).map(|mut h| {
            h.rank = score; // fused RRF score (higher = better)
            h
        }))
        .collect();
    let (hits, per_note_matched) = diversify(ranked, limit, PER_NOTE_CAP);
    Ok(SearchOutcome { hits, matched_notes, per_note_matched })
}

/// How many DISTINCT live notes the keyword predicate matches, under the same
/// filter as the search itself — exactly, with no pool ceiling, so the caller
/// never has to hedge the number as a floor.
///
/// Deliberately the KEYWORD leg only. The semantic leg is a *ranking*, not a
/// predicate: a KNN query returns its k nearest chunks for any input, so every
/// note "matches" it and a count over the fused pool would report roughly the
/// whole library for every question — which is worse than no count at all.
/// `None` means there is no predicate to count (the query carries no usable FTS
/// term), which the caller must report as *unknown* rather than as zero.
fn count_matching_notes(
    conn: &Connection,
    query: &str,
    filter: NoteFilter<'_>,
    workspace: &str,
) -> Result<Option<usize>> {
    let Some(match_expr) = fts_match_query(query) else {
        return Ok(None);
    };
    use rusqlite::types::Value;
    let mut sql = format!("SELECT COUNT(DISTINCT c.note_id) {KEYWORD_FROM_WHERE}");
    let mut args: Vec<Value> = vec![Value::Text(match_expr), Value::Text(workspace.to_string())];
    push_chunk_filters(&mut sql, &mut args, filter);
    let count: i64 = conn.query_row(&sql, rusqlite::params_from_iter(args), |row| row.get(0))?;
    Ok(Some(count.max(0) as usize))
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
    // LEFT JOIN, not an inner one: an untagged note (the common case) must still be
    // listed, and so must a note whose Client row hasn't arrived from sync yet.
    let mut sql = String::from(
        "SELECT n.id, n.title, n.created_at, n.folder_id, n.client_id, cl.name, n.summary, \
                n.speakers \
         FROM notes n LEFT JOIN clients cl ON cl.id = n.client_id \
         WHERE n.workspace_id = ?1 AND n.deleted_at IS NULL",
    );
    let mut args: Vec<Value> = vec![Value::Text(workspace.to_string())];
    // Shares the one filter builder with the keyword + semantic queries, so a new
    // narrowing (like #81's date window) can't reach search but miss listing.
    push_note_filters(&mut sql, &mut args, filter, "n");
    args.push(Value::Integer(limit as i64));
    // Ordered by `created_at`, the same field the date window filters and the tool
    // layer displays. Ordering by `updated_at` instead let a re-edited old note
    // outrank a newer meeting, so a capped "last week" listing could drop the very
    // notes it was asked for.
    sql.push_str(&format!(" ORDER BY n.created_at DESC LIMIT ?{}", args.len()));

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(args), |row| {
            Ok(NoteMeta {
                id: row.get(0)?,
                title: row.get(1)?,
                created_at: row.get(2)?,
                folder_id: row.get(3)?,
                client_id: row.get(4)?,
                client_name: row.get(5)?,
                summary: row.get(6)?,
                speakers: decode_speakers(&row.get::<_, String>(7)?),
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

    /// `ListFilter::WithMessages` hides conversations that hold no messages, and
    /// the paging stays honest while it does (#120).
    ///
    /// The paging half is the part worth a test: `/chat`'s sidebar treats a short
    /// page as "you have reached the end", so filtering AFTER the window would let
    /// one hidden draft in a full page look like the end of the list and silently
    /// truncate everything below it. Filtering in SQL is what keeps a page full.
    #[test]
    fn empty_conversations_can_be_excluded_without_breaking_paging() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("emptyfilter.sqlite")).unwrap();

        // Six global conversations, every other one left empty, stamped so the
        // intended order is unambiguous.
        let mut with_messages = Vec::new();
        for i in 0..6 {
            let c = create_conversation(
                &conn,
                CHAT_TENANT_PERSONAL,
                CHAT_SCOPE_GLOBAL,
                CHAT_GLOBAL_SCOPE_ID,
                "all",
            )
            .unwrap();
            conn.execute(
                "UPDATE conversations SET updated_at = ?1 WHERE id = ?2",
                params![1_000 + i, c.id],
            )
            .unwrap();
            if i % 2 == 0 {
                insert_chat_message(&conn, &c.id, "user", "[]").unwrap();
                with_messages.push(c.id);
            }
        }
        with_messages.reverse(); // most-recently-updated first

        let all = list_conversations(
            &conn,
            CHAT_TENANT_PERSONAL,
            CHAT_SCOPE_GLOBAL,
            CHAT_GLOBAL_SCOPE_ID,
            None,
            ListFilter::All,
        )
        .unwrap();
        assert_eq!(all.len(), 6, "unfiltered still sees every conversation");

        let non_empty = |page: Option<Page>| {
            list_conversations(
                &conn,
                CHAT_TENANT_PERSONAL,
                CHAT_SCOPE_GLOBAL,
                CHAT_GLOBAL_SCOPE_ID,
                page,
                ListFilter::WithMessages,
            )
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect::<Vec<_>>()
        };

        assert_eq!(non_empty(None), with_messages, "empties are hidden, order preserved");
        // Paging tiles the FILTERED list: a full page means more may follow, and
        // the pages concatenate to exactly the filtered set with no gaps.
        assert_eq!(non_empty(Some(Page { limit: 2, offset: 0 })), with_messages[0..2]);
        assert_eq!(non_empty(Some(Page { limit: 2, offset: 2 })), with_messages[2..3]);
        assert!(non_empty(Some(Page { limit: 2, offset: 3 })).is_empty(), "past the end");
    }

    /// Deleting a conversation takes its messages with it and leaves its
    /// neighbours — including the same Note's OTHER tenant — untouched (#109).
    #[test]
    fn delete_conversation_removes_messages_and_only_that_row() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("conv.sqlite")).unwrap();

        let doomed =
            create_conversation(&conn, CHAT_TENANT_PERSONAL, CHAT_SCOPE_NOTE, "note1", "note")
                .unwrap();
        let keep =
            create_conversation(&conn, CHAT_TENANT_PERSONAL, CHAT_SCOPE_NOTE, "note1", "note")
                .unwrap();
        // Same Note, other tenant: a workspace handle must survive a Personal delete.
        let other_tenant =
            create_conversation(&conn, "wsA", CHAT_SCOPE_NOTE, "note1", "note").unwrap();
        for c in [&doomed, &keep, &other_tenant] {
            insert_chat_message(&conn, &c.id, "user", "[]").unwrap();
        }

        delete_conversation(&conn, &doomed.id).unwrap();

        assert!(get_conversation_by_id(&conn, &doomed.id).unwrap().is_none());
        assert_eq!(
            conversation_message_count(&conn, &doomed.id).unwrap(),
            0,
            "messages go with the conversation — an orphan would never be read or cleaned up"
        );
        assert!(get_conversation_by_id(&conn, &keep.id).unwrap().is_some());
        assert_eq!(conversation_message_count(&conn, &keep.id).unwrap(), 1);
        assert!(get_conversation_by_id(&conn, &other_tenant.id).unwrap().is_some());
        assert_eq!(conversation_message_count(&conn, &other_tenant.id).unwrap(), 1);

        // Idempotent: a double-click or a retry must not error.
        delete_conversation(&conn, &doomed.id).unwrap();
    }

    /// A rename persists, bumps `updated_at` so the row sorts as freshly touched,
    /// and is not overwritten by the send path's title derivation (#109). That
    /// last part is the acceptance criterion, and it holds because the send path
    /// only titles a conversation whose title is unset.
    #[test]
    fn rename_conversation_persists_and_survives_title_derivation() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("conv.sqlite")).unwrap();

        let conv =
            create_conversation(&conn, CHAT_TENANT_PERSONAL, CHAT_SCOPE_NOTE, "note1", "note")
                .unwrap();
        assert!(conv.title.is_none(), "a fresh conversation is untitled");
        let created_updated_at = conv.updated_at;

        rename_conversation(&conn, &conv.id, "Q3 pricing").unwrap();
        let renamed = get_conversation_by_id(&conn, &conv.id).unwrap().unwrap();
        assert_eq!(renamed.title.as_deref(), Some("Q3 pricing"));
        assert!(
            renamed.updated_at >= created_updated_at,
            "a rename touches the row, so recency ordering reflects it"
        );

        // The send path's guard: a titled conversation is never re-titled, so the
        // user's override outlives the next turn.
        assert!(
            renamed.title.as_deref().is_some_and(|t| !t.trim().is_empty()),
            "non-empty title → resolved_title_is_unset() is false → derive_title never runs"
        );
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

    /// The authorship pin (#103) defaults to off, round-trips, clears, and — the
    /// part that matters — survives a re-open. An existing conversation must
    /// back-fill to "" rather than to anything that would narrow retrieval:
    /// its scrollback was written over the whole workspace.
    #[test]
    fn conversation_owner_filter_defaults_off_and_persists_idempotently() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("owner.sqlite");
        let reload = |conn: &Connection| {
            latest_conversation(conn, CHAT_TENANT_PERSONAL, CHAT_SCOPE_NOTE, "note1")
                .unwrap()
                .unwrap()
        };
        {
            let conn = open(&path).unwrap();
            let conv =
                create_conversation(&conn, CHAT_TENANT_PERSONAL, CHAT_SCOPE_NOTE, "note1", "note")
                    .unwrap();
            assert_eq!(conv.owner_filter, "", "a new conversation is unpinned");
            set_conversation_owner_filter(&conn, &conv.id, Some("u-anna")).unwrap();
            assert_eq!(reload(&conn).owner_filter, "u-anna", "the pin round-trips");
            set_conversation_owner_filter(&conn, &conv.id, None).unwrap();
            assert_eq!(reload(&conn).owner_filter, "", "None clears the pin");
            set_conversation_owner_filter(&conn, &conv.id, Some("u-anna")).unwrap();
        }
        let conn = open(&path).unwrap();
        assert_eq!(
            reload(&conn).owner_filter,
            "u-anna",
            "the pin survives a re-open (idempotent migration)"
        );
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
        assert_eq!(list_conversations(&conn, "personal", "note", "n1", None, ListFilter::All).unwrap().len(), 1);
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

    // ── Speaker-aware chunking (issue #104) ──────────────────────────────────

    /// Pinned parity with the frontend parse (issue #104).
    ///
    /// The same label rule now exists three times: here, in `extractSpeakerLabels`
    /// (`src/lib/speakers.ts`, which decides the rename UI's chip strip), and in
    /// humla-cloud's indexer. They MUST agree — a one-sided change means a chunk is
    /// attributed to someone the UI never shows as a speaker, or vice versa. #105's
    /// `client_id` drift passed every test on both sides, so the mitigation is the
    /// one used for the tool schemas: pin the identical case table in each suite.
    ///
    /// The mirror of this table is `PINNED_LABEL_CASES` in `src/lib/speakers.test.ts`.
    /// Change one, change both, or the pair stops meaning anything.
    #[test]
    fn parse_speaker_turns_mirrors_the_frontend_label_rule() {
        let over_bound = format!("{}: over the bound", "x".repeat(41));
        let at_bound = format!("{}: at the bound", "x".repeat(40));
        let forty = "x".repeat(40);
        let pinned_label_cases: &[(&str, Option<&str>)] = &[
            ("Michael: hello", Some("Michael")),
            ("  Michael: hello", Some("Michael")),
            ("Alice : hi", Some("Alice")),
            ("Hege Tronshaugen: ja", Some("Hege Tronshaugen")),
            ("Speaker 1: hi", Some("Speaker 1")),
            ("You: hi", Some("You")),
            ("12:30 standup", None),
            ("see https://example.com now", None),
            ("Michael:hello", None),
            ("no colon at all", None),
            (over_bound.as_str(), None),
            (at_bound.as_str(), Some(forty.as_str())),
        ];

        for (input, expected) in pinned_label_cases {
            let got = parse_speaker_turns(input).into_iter().next().and_then(|t| t.speaker);
            assert_eq!(
                got.as_deref(),
                *expected,
                "pinned label case {input:?} disagrees with the frontend"
            );
        }

        // Blank lines are dropped, and text is verbatim including the label.
        let turns = parse_speaker_turns("Michael: one\n\n   \nHege: two");
        assert_eq!(turns.len(), 2, "blank lines contribute no turns");
        assert_eq!(turns[0].text, "Michael: one", "text keeps the label inline");
    }

    #[test]
    fn pack_turns_keeps_whole_turns_together_and_records_their_speakers() {
        let turns = parse_speaker_turns("Michael: aaaa\nHege: bbbb\nMichael: cccc");

        // Tight budget: each turn is its own chunk, and carries its own speaker.
        let tight = pack_turns(&turns, 14);
        assert_eq!(tight.len(), 3, "a boundary never lands mid-turn");
        assert_eq!(tight[0].text, "Michael: aaaa");
        assert_eq!(tight[0].speakers, vec!["Michael".to_string()]);
        assert_eq!(tight[1].speakers, vec!["Hege".to_string()]);

        // Generous budget: all three pack into one chunk joined by the original
        // single newline, with distinct speakers in first-encounter order.
        let merged = pack_turns(&turns, 1000);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].text, "Michael: aaaa\nHege: bbbb\nMichael: cccc");
        assert_eq!(
            merged[0].speakers,
            vec!["Michael".to_string(), "Hege".to_string()],
            "distinct, first-encounter order, not repeated"
        );

        assert!(pack_turns(&[], 100).is_empty(), "no turns, no chunks");
    }

    #[test]
    fn pack_turns_hard_splits_an_overlong_turn_and_says_who_is_still_speaking() {
        // One turn far past the budget — the case that today falls through to
        // arbitrary char slicing with the label stranded in the first piece.
        let long = format!("Hege: {}", "x".repeat(120));
        let turns = parse_speaker_turns(&long);
        let chunks = pack_turns(&turns, 50);

        assert!(chunks.len() > 1, "an overlong turn is split, not emitted whole");
        assert!(chunks[0].text.starts_with("Hege: "), "the first piece keeps the real label");
        for (i, c) in chunks.iter().enumerate().skip(1) {
            assert!(
                c.text.starts_with("Hege (continued): "),
                "piece {i} must still name the speaker, got {:?}",
                c.text
            );
        }
        for c in &chunks {
            assert_eq!(c.speakers, vec!["Hege".to_string()], "every piece is attributed");
        }

        // Reassembling the pieces recovers the original speech exactly — the
        // prefix is additive, never a rewrite of the user's words.
        let rejoined: String = chunks
            .iter()
            .map(|c| c.text.trim_start_matches("Hege (continued): ").to_string())
            .collect();
        assert_eq!(rejoined, long, "no speech is lost or altered by splitting");
    }

    #[test]
    fn pack_turns_handles_a_transcript_with_no_labels_at_all() {
        // An import, or a live transcript before the post-stop diarize pass. This
        // is strictly better than today's behaviour: unlabelled transcripts are
        // one blank-line-free "paragraph", so they hard-split mid-word.
        let turns = parse_speaker_turns("first line here\nsecond line here\nthird line here");
        let chunks = pack_turns(&turns, 32);

        assert!(chunks.len() > 1, "packs by line even with nothing to attribute");
        for c in &chunks {
            assert!(c.speakers.is_empty(), "nothing to attribute, so no speakers claimed");
        }
        let rejoined = chunks.iter().map(|c| c.text.as_str()).collect::<Vec<_>>().join("\n");
        assert_eq!(rejoined, "first line here\nsecond line here\nthird line here");
    }

    #[test]
    fn note_chunk_texts_packs_the_transcript_by_turn_but_leaves_prose_alone() {
        // The transcript's turns are separated by SINGLE newlines, which is exactly
        // what the paragraph splitter cannot see — hence the separate path.
        let transcript = "Michael: alpha\nHege: beta";
        let chunks = note_chunk_texts("body one\n\nbody two", transcript, "summary text");

        let transcript_chunks: Vec<&NoteChunk> =
            chunks.iter().filter(|c| c.source == "transcript").collect();
        assert_eq!(transcript_chunks.len(), 1);
        assert_eq!(
            transcript_chunks[0].speakers,
            vec!["Michael".to_string(), "Hege".to_string()],
            "transcript chunks carry speakers"
        );

        // Body and summary keep the paragraph splitter and claim no speakers —
        // a line in a typed note that happens to read "Note: buy milk" is not a
        // person who spoke.
        for c in chunks.iter().filter(|c| c.source != "transcript") {
            assert!(c.speakers.is_empty(), "{} must claim no speakers", c.source);
        }
    }

    #[test]
    fn live_note_ids_covers_every_live_note_and_no_trashed_one() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("t.sqlite")).unwrap();
        let keep_a = create_note(&conn, "en", "meeting", "").unwrap().id;
        let keep_b = create_note(&conn, "en", "meeting", "").unwrap().id;
        let trashed = create_note(&conn, "en", "meeting", "").unwrap().id;
        delete_note(&conn, &trashed).unwrap();

        let ids = live_note_ids(&conn).unwrap();
        assert!(ids.contains(&keep_a) && ids.contains(&keep_b));
        assert!(!ids.contains(&trashed), "a trashed note is not reindexed");

        // Unlike the lazy startup backfill, this includes notes that ALREADY have
        // chunks — a chunking-shape change invalidates rows that look fine, and
        // there is no sentinel for "chunked before turn-packing".
        reindex_note(&conn, &keep_a, "", "Michael: hi", "").unwrap();
        assert!(
            live_note_ids(&conn).unwrap().contains(&keep_a),
            "an already-indexed note still needs rebuilding"
        );
        assert!(
            !note_ids_needing_reindex(&conn).unwrap().contains(&keep_a),
            "...which is exactly what the lazy work-list does NOT cover"
        );
    }

    /// Issue #121, which ADR-0002 turns from untidiness into a rule: derived person
    /// data must be destroyed with its source, not merely hidden from queries, or
    /// "delete the note" is not an honest answer to erasing someone.
    #[test]
    fn purging_a_note_takes_its_derived_chunks_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("t.sqlite")).unwrap();
        let doomed = create_note(&conn, "en", "meeting", "").unwrap().id;
        let keeper = create_note(&conn, "en", "meeting", "").unwrap().id;
        reindex_note(&conn, &doomed, "", "Hege: something private", "").unwrap();
        reindex_note(&conn, &keeper, "", "Michael: something else", "").unwrap();

        let count = |note_id: &str| -> i64 {
            conn.query_row(
                "SELECT COUNT(*) FROM note_chunks WHERE note_id = ?1",
                params![note_id],
                |r| r.get(0),
            )
            .unwrap()
        };
        let fts_count = |note_id: &str| -> i64 {
            conn.query_row(
                "SELECT COUNT(*) FROM note_chunks_fts WHERE note_id = ?1",
                params![note_id],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert!(count(&doomed) > 0 && fts_count(&doomed) > 0, "indexed to begin with");

        purge_note(&conn, &doomed).unwrap();

        assert_eq!(count(&doomed), 0, "purge must clear the chunk rows, not orphan them");
        assert_eq!(fts_count(&doomed), 0, "and the FTS rows with them");
        assert!(count(&keeper) > 0, "another note's chunks are untouched");
    }

    /// Soft delete is the opposite case and must NOT clear anything — a Trash
    /// restore has to bring the note back searchable, and the `deleted_at IS NULL`
    /// join already keeps it out of results meanwhile.
    #[test]
    fn soft_deleting_a_note_keeps_its_chunks_for_restore() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("t.sqlite")).unwrap();
        let id = create_note(&conn, "en", "meeting", "").unwrap().id;
        reindex_note(&conn, &id, "", "Hege: still here", "").unwrap();

        delete_note(&conn, &id).unwrap();

        let remaining: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM note_chunks WHERE note_id = ?1",
                params![&id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(remaining > 0, "soft delete keeps the index so restore is lossless");
    }

    /// Seeds two notes whose speakers deliberately overlap by prefix — the case
    /// that decides whether the filter is exact or quietly merges two people.
    fn seed_speaker_notes(conn: &Connection) -> (String, String) {
        let a = create_note(conn, "en", "meeting", "").unwrap().id;
        update_note(
            conn,
            &a,
            &NotePatch { title: Some("K2 kickoff".into()), ..Default::default() },
        )
        .unwrap();
        reindex_note(
            conn,
            &a,
            "",
            "Michael: we should scope the pilot\nHege: not without the security review",
            "",
        )
        .unwrap();

        let b = create_note(conn, "en", "meeting", "").unwrap().id;
        update_note(
            conn,
            &b,
            &NotePatch { title: Some("Berg sync".into()), ..Default::default() },
        )
        .unwrap();
        reindex_note(conn, &b, "", "Michael Berg: the pilot looks fine to me", "").unwrap();
        (a, b)
    }

    #[test]
    fn the_speaker_filter_is_exact_and_never_merges_two_people() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("t.sqlite")).unwrap();
        let (a, b) = seed_speaker_notes(&conn);

        // "Michael" must not reach "Michael Berg" — the wrong answer here is a
        // confident one, which is the whole reason matching is exact (#104).
        let only_michael = NoteFilter { speaker: Some("Michael"), ..Default::default() };
        let notes = list_notes_filtered(&conn, only_michael, "", 50).unwrap();
        let ids: Vec<&str> = notes.iter().map(|n| n.id.as_str()).collect();
        assert!(ids.contains(&a.as_str()), "Michael spoke in the kickoff");
        assert!(!ids.contains(&b.as_str()), "Michael must NOT match Michael Berg");

        // And the longer label finds only its own note.
        let berg = NoteFilter { speaker: Some("Michael Berg"), ..Default::default() };
        let berg_ids: Vec<String> =
            list_notes_filtered(&conn, berg, "", 50).unwrap().into_iter().map(|n| n.id).collect();
        assert_eq!(berg_ids, vec![b.clone()]);

        // Case-insensitive, so a model echoing a name in different case still hits.
        let lower = NoteFilter { speaker: Some("hege"), ..Default::default() };
        assert_eq!(list_notes_filtered(&conn, lower, "", 50).unwrap().len(), 1, "case folded");

        // A name nobody has is an honest empty, not a widened search.
        let absent = NoteFilter { speaker: Some("Nobody"), ..Default::default() };
        assert!(list_notes_filtered(&conn, absent, "", 50).unwrap().is_empty());
    }

    #[test]
    fn listing_rows_carry_who_spoke_so_the_model_can_only_name_real_speakers() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("t.sqlite")).unwrap();
        let (a, _) = seed_speaker_notes(&conn);

        let notes = list_notes_filtered(&conn, NoteFilter::default(), "", 50).unwrap();
        let kickoff = notes.iter().find(|n| n.id == a).unwrap();
        assert_eq!(
            kickoff.speakers,
            vec!["Michael".to_string(), "Hege".to_string()],
            "a listing row names its speakers, since NoteMeta excludes the transcript"
        );
    }

    /// The filter is CHUNK-level, and that has a precise meaning worth pinning
    /// because it is easy to over-read: a hit means *"they spoke in this passage"*,
    /// NOT *"they said this"*. A chunk holds several turns, so a chunk attributed to
    /// Hege can still contain Michael's words — which is exactly why labels stay
    /// inline in the text, so the model reads who said what rather than trusting the
    /// filter to have separated it. Right for "what did Hege commit to", wrong for
    /// counting who talked most (#104's own Risks note).
    #[test]
    fn the_speaker_filter_narrows_to_passages_not_to_sentences() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("t.sqlite")).unwrap();
        let id = create_note(&conn, "en", "meeting", "").unwrap().id;

        // One turn long enough to fill chunks on its own, so Michael's speech and
        // Hege's land in DIFFERENT chunks and chunk-level narrowing is observable.
        let long_michael = format!("Michael: {} pilot", "padding words ".repeat(400));
        let transcript = format!("{long_michael}\nHege: not without the security review");
        reindex_note(&conn, &id, "", &transcript, "").unwrap();

        let chunk_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM note_chunks WHERE note_id = ?1 AND source = 'transcript'",
                params![&id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(chunk_count > 1, "precondition: the turns must not share a chunk");

        // "pilot" sits in Michael's speech. Filtering to Hege finds nothing, because
        // the passage she spoke in doesn't contain it.
        let hege = NoteFilter { speaker: Some("Hege"), ..Default::default() };
        let hits = hybrid_search_chunks(&conn, "pilot", None, "", hege, "", 10).unwrap().hits;
        assert!(hits.is_empty(), "Hege's passages do not mention the pilot: {hits:?}");

        // Same query, no filter: found. So the emptiness above is the filter working
        // rather than the query failing.
        let unfiltered =
            hybrid_search_chunks(&conn, "pilot", None, "", NoteFilter::default(), "", 10)
                .unwrap()
                .hits;
        assert!(!unfiltered.is_empty(), "the query itself is sound");

        // And filtering to Michael does find it.
        let michael = NoteFilter { speaker: Some("Michael"), ..Default::default() };
        let his = hybrid_search_chunks(&conn, "pilot", None, "", michael, "", 10).unwrap().hits;
        assert!(!his.is_empty(), "Michael did say it");
    }

    /// The other half of the same boundary, stated as a fact rather than a hope: a
    /// chunk returned for one speaker may carry another's words. If this ever starts
    /// failing, the filter has silently become sentence-level and the header/citation
    /// copy that says "passages" needs revisiting.
    #[test]
    fn a_chunk_attributed_to_one_speaker_may_still_contain_anothers_words() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("t.sqlite")).unwrap();
        let id = create_note(&conn, "en", "meeting", "").unwrap().id;
        reindex_note(
            &conn,
            &id,
            "",
            "Michael: we should scope the pilot\nHege: not without the security review",
            "",
        )
        .unwrap();

        let hege = NoteFilter { speaker: Some("Hege"), ..Default::default() };
        let hits = hybrid_search_chunks(&conn, "pilot", None, "", hege, "", 10).unwrap().hits;
        assert_eq!(hits.len(), 1, "short turns share a chunk, and Hege spoke in it");
        assert!(
            hits[0].text.contains("Michael: we should scope the pilot"),
            "the excerpt carries Michael's label inline, so the model can attribute it"
        );
    }

    #[test]
    fn an_unmatched_speaker_can_be_answered_with_the_names_that_do_exist() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("t.sqlite")).unwrap();
        seed_speaker_notes(&conn);

        // #106's doctrine: a miss that reports the available names lets the model
        // self-correct, where a bare empty result invites it to report absence.
        let present = speakers_in_scope(&conn, NoteFilter::default(), "").unwrap();
        assert!(present.contains(&"Michael".to_string()));
        assert!(present.contains(&"Hege".to_string()));
        assert!(present.contains(&"Michael Berg".to_string()));
        assert_eq!(present.len(), 3, "distinct, no repeats: {present:?}");
    }

    #[test]
    fn speakers_encoding_isolates_one_label_from_another() {
        assert_eq!(encode_speakers(&[]), "", "no speakers is empty, not a bare delimiter");
        assert_eq!(encode_speakers(&["Michael".into()]), "|Michael|");
        assert_eq!(encode_speakers(&["Michael".into(), "Hege".into()]), "|Michael|Hege|");

        // The property the whole filter rests on: a wrapped exact label cannot be a
        // substring of a DIFFERENT wrapped label, so "Michael" never matches
        // "Michael Berg" and two people stay two people (#104).
        let berg = encode_speakers(&["Michael Berg".into()]);
        assert!(!berg.contains("|Michael|"), "{berg} must not contain the shorter label");
        assert!(encode_speakers(&["Michael".into()]).contains("|Michael|"));

        // A `|` in a label would forge a boundary, so it is normalised away here.
        let odd = encode_speakers(&["A|B".into()]);
        assert_eq!(decode_speakers(&odd), vec!["A B".to_string()], "pipe normalised to a space");

        assert!(decode_speakers("").is_empty());
        assert_eq!(decode_speakers("|Michael|Hege|"), vec!["Michael".to_string(), "Hege".to_string()]);
        assert_eq!(decode_speakers("Michael"), vec!["Michael".to_string()], "tolerates unwrapped");
    }

    #[test]
    fn reindex_note_records_speakers_on_chunks_and_on_the_note() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("t.sqlite")).unwrap();
        let id = create_note(&conn, "en", "meeting", "").unwrap().id;
        let transcript = "Michael: shall we start\nHege: yes\nMichael: good";

        reindex_note(&conn, &id, "some body", transcript, "a summary").unwrap();

        // Note level: the union of everyone who spoke, first-encounter order.
        let note_speakers: String = conn
            .query_row("SELECT speakers FROM notes WHERE id = ?1", params![&id], |r| r.get(0))
            .unwrap();
        assert_eq!(decode_speakers(&note_speakers), vec!["Michael".to_string(), "Hege".to_string()]);

        // Chunk level: only the transcript chunk claims speakers.
        let mut stmt = conn
            .prepare("SELECT source, speakers FROM note_chunks WHERE note_id = ?1 ORDER BY seq")
            .unwrap();
        let rows: Vec<(String, String)> = stmt
            .query_map(params![&id], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        for (source, speakers) in &rows {
            if source == "transcript" {
                assert!(!speakers.is_empty(), "a transcript chunk must be attributed");
            } else {
                assert!(speakers.is_empty(), "{source} must claim no speakers");
            }
        }
    }

    #[test]
    fn reindex_note_is_the_only_writer_so_a_rename_cannot_drift() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("t.sqlite")).unwrap();
        let id = create_note(&conn, "en", "meeting", "").unwrap().id;

        reindex_note(&conn, &id, "", "Speaker 1: hello", "").unwrap();
        let before: String = conn
            .query_row("SELECT speakers FROM notes WHERE id = ?1", params![&id], |r| r.get(0))
            .unwrap();
        assert_eq!(decode_speakers(&before), vec!["Speaker 1".to_string()]);

        // The user renames the speaker, which rewrites the transcript text. The
        // derived columns follow on the next reindex — they cannot disagree with
        // what the user reads, because the text is their only source.
        reindex_note(&conn, &id, "", "Hege Tronshaugen: hello", "").unwrap();
        let after: String = conn
            .query_row("SELECT speakers FROM notes WHERE id = ?1", params![&id], |r| r.get(0))
            .unwrap();
        assert_eq!(decode_speakers(&after), vec!["Hege Tronshaugen".to_string()]);

        // Reindexing must NOT mark the note dirty: it runs on every content settle,
        // and bumping updated_at would re-sync the note forever over derived data.
        let touched: i64 = conn
            .query_row("SELECT updated_at FROM notes WHERE id = ?1", params![&id], |r| r.get(0))
            .unwrap();
        reindex_note(&conn, &id, "", "Hege Tronshaugen: hello", "").unwrap();
        let after_reindex: i64 = conn
            .query_row("SELECT updated_at FROM notes WHERE id = ?1", params![&id], |r| r.get(0))
            .unwrap();
        assert_eq!(touched, after_reindex, "reindex must not bump updated_at");
    }

    #[test]
    fn note_chunk_texts_tags_each_source() {
        let chunks = note_chunk_texts("body words", "transcript words", "summary words");
        let sources: Vec<&str> = chunks.iter().map(|c| c.source).collect();
        assert_eq!(sources, vec!["body", "transcript", "summary"]);
        // Blank sources contribute nothing.
        let sparse = note_chunk_texts("only body", "", "");
        assert_eq!(sparse.len(), 1);
        assert_eq!(sparse[0].source, "body");
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

        let hits = hybrid_search_chunks(&conn, "budget", None, "", NoteFilter::default(), "", 10).unwrap().hits;
        assert_eq!(hits.len(), 1, "only the budget note matches");
        assert_eq!(hits[0].note_id, a.id);
        assert_eq!(hits[0].note_title, "Budget planning");
        assert_eq!(hits[0].source, "transcript");

        // Punctuation / operator chars in the query don't blow up FTS5.
        let safe =
            hybrid_search_chunks(&conn, "budget: \"marketing\" -foo*", None, "", NoteFilter::default(), "", 10).unwrap().hits;
        assert!(!safe.is_empty(), "sanitised query still matches");

        // A gibberish query returns nothing rather than erroring.
        assert!(hybrid_search_chunks(&conn, "!!!@@@", None, "", NoteFilter::default(), "", 10).unwrap().hits.is_empty());
        assert!(hybrid_search_chunks(&conn, "zzzzznope", None, "", NoteFilter::default(), "", 10).unwrap().hits.is_empty());
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
        assert_eq!(hybrid_search_chunks(&conn, "world", None, "", NoteFilter::default(), "", 10).unwrap().hits.len(), 1);
        reindex_note(&conn, &n.id, "totally different", "", "").unwrap();
        assert!(hybrid_search_chunks(&conn, "world", None, "", NoteFilter::default(), "", 10).unwrap().hits.is_empty());
        assert_eq!(hybrid_search_chunks(&conn, "different", None, "", NoteFilter::default(), "", 10).unwrap().hits.len(), 1);
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

        assert_eq!(hybrid_search_chunks(&conn, "keyword", None, "", NoteFilter::default(), "", 10).unwrap().hits.len(), 2, "unfiltered sees both");
        let by_folder = hybrid_search_chunks(&conn, "keyword", None, "", f_folder, "", 10).unwrap().hits;
        assert_eq!(by_folder.len(), 1);
        assert_eq!(by_folder[0].note_id, inside.id);
        // folder AND client combine (both narrow to the same single note).
        assert_eq!(hybrid_search_chunks(&conn, "keyword", None, "", f_both, "", 10).unwrap().hits.len(), 1);
        // note_id clamp (the "this Note" breadth) pins search to one note.
        let by_note = hybrid_search_chunks(&conn, "keyword", None, "", f_note, "", 10).unwrap().hits;
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
        let hits = hybrid_search_chunks(&conn, "financial", None, "m", NoteFilter::default(), "", 10).unwrap().hits;
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
        let hits = hybrid_search_chunks(&conn, "budget", Some(&qv), "m", NoteFilter::default(), "", 10).unwrap().hits;
        let note_ids: Vec<&str> = hits.iter().map(|h| h.note_id.as_str()).collect();
        // Keyword finds A; semantic surfaces B. Hybrid returns BOTH — the
        // semantic-only match B would be invisible to keyword search alone.
        assert!(note_ids.contains(&a.id.as_str()), "keyword match present");
        assert!(note_ids.contains(&b.id.as_str()), "semantic-only match surfaced by RRF");
    }

    // ── #81: hit diversity + date window ────────────────────────────────────

    fn hit(note_id: &str, rank: f64) -> ChunkHit {
        ChunkHit {
            note_id: note_id.to_string(),
            note_title: note_id.to_string(),
            note_created_at: 0,
            source: "transcript".into(),
            text: format!("{note_id}#{rank}"),
            rank,
            seq: 0,
        }
    }

    /// The selection half of [`diversify`], for the tests that predate it also
    /// reporting per-note candidate counts.
    fn diversify_hits(
        ranked: Vec<ChunkHit>,
        limit: usize,
        per_note_cap: usize,
    ) -> Vec<ChunkHit> {
        diversify(ranked, limit, per_note_cap).0
    }

    fn note_ids_of(hits: &[ChunkHit]) -> Vec<&str> {
        hits.iter().map(|h| h.note_id.as_str()).collect()
    }

    #[test]
    fn diversify_spreads_a_scarce_result_set_across_notes() {
        // Note A ranks best on four chunks; unbounded it takes every slot and B
        // and C are never seen.
        let ranked =
            vec![hit("a", 9.0), hit("a", 8.0), hit("a", 7.0), hit("a", 6.0), hit("b", 5.0), hit("c", 4.0)];
        assert_eq!(note_ids_of(&diversify_hits(ranked, 4, 2)), vec!["a", "a", "b", "c"]);
    }

    #[test]
    fn diversify_uses_spare_slots_on_the_best_remaining_hits() {
        // With room for everything, nothing is dropped — the cap reorders
        // (coverage first), it does not discard.
        let ranked =
            vec![hit("a", 9.0), hit("a", 8.0), hit("a", 7.0), hit("a", 6.0), hit("b", 5.0), hit("c", 4.0)];
        assert_eq!(note_ids_of(&diversify_hits(ranked, 6, 2)), vec!["a", "a", "b", "c", "a", "a"]);
    }

    /// The backfill is what stops the per-note cap costing recall: a note-scoped
    /// search must still return several excerpts of its one note.
    #[test]
    fn diversify_keeps_full_recall_when_only_one_note_matches() {
        let ranked = vec![hit("a", 9.0), hit("a", 8.0), hit("a", 7.0)];
        assert_eq!(diversify_hits(ranked.clone(), 6, 2).len(), 3);
        assert_eq!(diversify_hits(ranked, 2, 2).len(), 2);
    }

    #[test]
    fn diversify_never_exceeds_the_limit() {
        let ranked: Vec<ChunkHit> =
            (0..30).map(|i| hit(&format!("n{}", i % 3), 30.0 - f64::from(i))).collect();
        assert_eq!(diversify_hits(ranked, 8, 2).len(), 8);
    }

    #[test]
    fn since_ms_filters_search_and_listing_by_note_creation() {
        const NOW: i64 = 1_785_024_000_000; // 2026-07-26T00:00:00Z
        const DAY: i64 = 86_400_000;
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("since.sqlite")).unwrap();

        let mut ids = Vec::new();
        for (title, age_days) in [("Recent budget", 2), ("Ancient budget", 90)] {
            let n = create_note(&conn, "en", "meeting", "").unwrap();
            update_note(
                &conn,
                &n.id,
                &NotePatch {
                    title: Some(title.into()),
                    transcript: Some("the budget came up".into()),
                    ..Default::default()
                },
            )
            .unwrap();
            conn.execute(
                "UPDATE notes SET created_at = ?1 WHERE id = ?2",
                rusqlite::params![NOW - age_days * DAY, &n.id],
            )
            .unwrap();
            let fresh = get_note(&conn, &n.id).unwrap();
            reindex_note(&conn, &n.id, &fresh.body, &fresh.transcript, &fresh.summary).unwrap();
            ids.push(n.id);
        }
        let (recent, ancient) = (&ids[0], &ids[1]);

        let unfiltered = NoteFilter::default();
        let windowed = NoteFilter { since_ms: Some(NOW - 7 * DAY), ..Default::default() };

        let all = hybrid_search_chunks(&conn, "budget", None, "", unfiltered, "", 10).unwrap().hits;
        assert_eq!(all.len(), 2, "unfiltered sees both");
        let recent_only = hybrid_search_chunks(&conn, "budget", None, "", windowed, "", 10).unwrap().hits;
        assert_eq!(note_ids_of(&recent_only), vec![recent.as_str()]);

        // The listing shares the one filter builder, so the window reaches it too.
        assert_eq!(list_notes_filtered(&conn, unfiltered, "", 10).unwrap().len(), 2);
        let listed = list_notes_filtered(&conn, windowed, "", 10).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, *recent);
        assert_ne!(listed[0].id, *ancient);
    }

    // ── #93: global chat scope ──────────────────────────────────────────────

    #[test]
    fn chat_target_derives_scope_and_scope_id_together() {
        let note = ChatTarget::Note("n1".into());
        assert_eq!((note.scope(), note.scope_id()), (CHAT_SCOPE_NOTE, "n1"));
        assert_eq!((ChatTarget::Global.scope(), ChatTarget::Global.scope_id()), (CHAT_SCOPE_GLOBAL, CHAT_GLOBAL_SCOPE_ID));
        // A global target has no anchor; callers needing one must handle None.
        assert_eq!(note.note_id(), Some("n1"));
        assert_eq!(ChatTarget::Global.note_id(), None);
    }

    /// Absent means global; EMPTY is an error. Letting `""` mean global is the
    /// trap humla-cloud#26 avoided server-side — a typo must not silently become
    /// a library-wide conversation.
    #[test]
    fn an_absent_note_id_is_global_but_an_empty_one_is_an_error() {
        assert_eq!(ChatTarget::from_note_id(None).unwrap(), ChatTarget::Global);
        assert_eq!(
            ChatTarget::from_note_id(Some("n1".into())).unwrap(),
            ChatTarget::Note("n1".into())
        );
        for empty in ["", "   ", "\t"] {
            let err = ChatTarget::from_note_id(Some(empty.into())).unwrap_err();
            assert!(err.contains("empty id"), "got: {err}");
        }
    }

    /// `/chat` lists conversations uncapped, so it fetches them a page at a time
    /// (#95). The window must ride on the SAME ordering the unpaged list uses, or
    /// scrolling would repeat and skip rows.
    #[test]
    fn conversation_pages_tile_the_list_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("pages.sqlite")).unwrap();

        // Six global conversations, stamped so the intended order is unambiguous —
        // rows created in one test share a timestamp, which would otherwise leave
        // the tie broken only by id.
        let mut ids = Vec::new();
        for i in 0..6 {
            let c =
                create_conversation(&conn, CHAT_TENANT_PERSONAL, CHAT_SCOPE_GLOBAL, CHAT_GLOBAL_SCOPE_ID, "all")
                    .unwrap();
            conn.execute("UPDATE conversations SET updated_at = ?1 WHERE id = ?2", params![1_000 + i, c.id])
                .unwrap();
            ids.push(c.id);
        }
        ids.reverse(); // most-recently-updated first

        let page = |limit: i64, offset: i64| {
            list_conversations(
                &conn,
                CHAT_TENANT_PERSONAL,
                CHAT_SCOPE_GLOBAL,
                CHAT_GLOBAL_SCOPE_ID,
                Some(Page { limit, offset }),
                ListFilter::All,
            )
            .unwrap()
            .into_iter()
            .map(|c| c.id)
            .collect::<Vec<_>>()
        };

        // Successive pages tile the list exactly: no gap, no repeat.
        assert_eq!([page(4, 0), page(4, 4)].concat(), ids);
        // A page past the end is empty rather than an error — that's how the
        // frontend learns to stop asking.
        assert!(page(4, 8).is_empty());
        // And no page at all still means everything.
        assert_eq!(
            list_conversations(&conn, CHAT_TENANT_PERSONAL, CHAT_SCOPE_GLOBAL, CHAT_GLOBAL_SCOPE_ID, None, ListFilter::All)
                .unwrap()
                .len(),
            6,
        );
    }

    /// The composite key is `(tenant, scope, scope_id)`, so a global thread and a
    /// note thread never see each other even within one tenant — and a note whose
    /// id somehow equalled the sentinel still wouldn't collide, because the scope
    /// differs.
    #[test]
    fn global_and_note_conversations_do_not_leak_into_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("scopes.sqlite")).unwrap();

        let note = ChatTarget::Note("n1".into());
        let global = ChatTarget::Global;
        let mk = |t: &ChatTarget| {
            create_conversation(&conn, CHAT_TENANT_PERSONAL, t.scope(), t.scope_id(), "all")
                .unwrap()
        };
        let note_conv = mk(&note);
        let global_conv = mk(&global);
        assert_ne!(note_conv.id, global_conv.id);

        let listed = |t: &ChatTarget| {
            list_conversations(&conn, CHAT_TENANT_PERSONAL, t.scope(), t.scope_id(), None, ListFilter::All)
                .unwrap()
                .into_iter()
                .map(|c| c.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(listed(&note), vec![note_conv.id.clone()]);
        assert_eq!(listed(&global), vec![global_conv.id.clone()]);

        // …and the same holds per tenant: a workspace's global set is its own.
        let ws_global =
            create_conversation(&conn, "wsA", global.scope(), global.scope_id(), "all").unwrap();
        assert_eq!(listed(&global), vec![global_conv.id.clone()], "personal is unaffected");
        assert_eq!(
            list_conversations(&conn, "wsA", global.scope(), global.scope_id(), None, ListFilter::All).unwrap().len(),
            1
        );
        assert_eq!(
            latest_conversation(&conn, "wsA", global.scope(), global.scope_id())
                .unwrap()
                .unwrap()
                .id,
            ws_global.id
        );
        // A note id colliding with the sentinel still can't reach the global set.
        let collide = ChatTarget::Note(CHAT_GLOBAL_SCOPE_ID.into());
        let collide_conv = mk(&collide);
        assert_eq!(listed(&collide), vec![collide_conv.id]);
        assert_eq!(listed(&global), vec![global_conv.id], "the scope column separates them");
    }

    #[test]
    fn list_notes_filtered_carries_the_summary_for_skimming() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open(&dir.path().join("listsummary.sqlite")).unwrap();
        let n = create_note(&conn, "en", "meeting", "").unwrap();
        update_note(
            &conn,
            &n.id,
            &NotePatch {
                title: Some("Kickoff".into()),
                summary: Some("Launch slipped two weeks.".into()),
                ..Default::default()
            },
        )
        .unwrap();

        let listed = list_notes_filtered(&conn, NoteFilter::default(), "", 10).unwrap();
        assert_eq!(listed[0].summary, "Launch slipped two weeks.");
    }
}
