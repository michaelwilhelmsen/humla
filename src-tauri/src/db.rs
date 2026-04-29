use anyhow::Result;
use rusqlite::{Connection, params};
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
    // Per-note transcription language. Empty string means "fall back to the
    // global language setting" — that's how pre-feature notes are handled
    // without a backfill migration.
    pub language: String,
    // Per-note summary provider override. Empty string means "fall back
    // to the global summary_provider setting" (same convention as `language`).
    // Populated values are "openai" or "local".
    pub summary_provider: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Folder {
    pub id: String,
    pub name: String,
    pub created_at: i64,
    pub updated_at: i64,
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

        CREATE TABLE IF NOT EXISTS settings (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
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
    let _ = conn.execute(
        "ALTER TABLE notes ADD COLUMN language TEXT NOT NULL DEFAULT ''",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE notes ADD COLUMN summary_provider TEXT NOT NULL DEFAULT ''",
        [],
    );
    // Index is created AFTER the ALTERs so it's safe on both fresh DBs and
    // older DBs that needed the column added.
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_notes_folder ON notes(folder_id)",
        [],
    )?;
    Ok(conn)
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

const NOTE_COLS: &str = "id, title, body, transcript, summary, audio_path, summary_preset, folder_id, language, summary_provider, created_at, updated_at";

pub fn list_notes(conn: &Connection) -> Result<Vec<Note>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {NOTE_COLS} FROM notes ORDER BY updated_at DESC"
    ))?;
    let rows = stmt
        .query_map([], map_note)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_note(conn: &Connection, id: &str) -> Result<Note> {
    let n = conn.query_row(
        &format!("SELECT {NOTE_COLS} FROM notes WHERE id = ?1"),
        params![id],
        map_note,
    )?;
    Ok(n)
}

pub fn create_note(conn: &Connection, default_language: &str) -> Result<Note> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_ms();
    conn.execute(
        "INSERT INTO notes (id, title, body, transcript, summary, audio_path, summary_preset, folder_id, language, summary_provider, created_at, updated_at)
         VALUES (?1, '', '', '', '', NULL, 'meeting', NULL, ?2, '', ?3, ?3)",
        params![id, default_language, now],
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

pub fn list_folders(conn: &Connection) -> Result<Vec<Folder>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, created_at, updated_at FROM folders ORDER BY name COLLATE NOCASE",
    )?;
    let rows = stmt
        .query_map([], map_folder)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn create_folder(conn: &Connection, name: &str) -> Result<Folder> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_ms();
    conn.execute(
        "INSERT INTO folders (id, name, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
        params![id, name, now],
    )?;
    conn.query_row(
        "SELECT id, name, created_at, updated_at FROM folders WHERE id = ?1",
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

pub fn delete_folder(conn: &Connection, id: &str) -> Result<()> {
    // Notes in the folder fall back to root (folder_id = NULL), they're not deleted.
    conn.execute("UPDATE notes SET folder_id = NULL WHERE folder_id = ?1", params![id])?;
    conn.execute("DELETE FROM folders WHERE id = ?1", params![id])?;
    Ok(())
}

fn map_folder(row: &rusqlite::Row) -> rusqlite::Result<Folder> {
    Ok(Folder {
        id: row.get(0)?,
        name: row.get(1)?,
        created_at: row.get(2)?,
        updated_at: row.get(3)?,
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
}

pub fn update_note(conn: &Connection, id: &str, patch: &NotePatch) -> Result<()> {
    let now = now_ms();
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
    Ok(())
}

/// Append `text` to the note's transcript, inserting `separator` between
/// the existing text and the new content. Caller decides the separator —
/// " " for same-speaker continuation, "\n" for a speaker switch, "" when
/// the existing transcript is empty.
pub fn append_transcript(conn: &Connection, id: &str, text: &str, separator: &str) -> Result<String> {
    let mut current: String = conn.query_row(
        "SELECT transcript FROM notes WHERE id = ?1",
        params![id],
        |row| row.get(0),
    )?;
    if !current.is_empty() {
        current.push_str(separator);
    }
    current.push_str(text);
    let now = now_ms();
    conn.execute(
        "UPDATE notes SET transcript = ?1, updated_at = ?2 WHERE id = ?3",
        params![current, now, id],
    )?;
    Ok(current)
}

pub fn delete_note(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("DELETE FROM notes WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn get_setting(conn: &Connection, key: &str) -> Result<Option<String>> {
    let v: rusqlite::Result<String> = conn.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        params![key],
        |row| row.get(0),
    );
    match v {
        Ok(s) => Ok(Some(s)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn set_setting(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
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
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}
