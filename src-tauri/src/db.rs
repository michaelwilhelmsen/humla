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
            created_at      INTEGER NOT NULL,
            updated_at      INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_notes_updated ON notes(updated_at DESC);

        CREATE TABLE IF NOT EXISTS settings (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        "#,
    )?;
    // Idempotent migration for existing DBs created before summary_preset existed.
    let _ = conn.execute(
        "ALTER TABLE notes ADD COLUMN summary_preset TEXT NOT NULL DEFAULT 'meeting'",
        [],
    );
    Ok(conn)
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub fn list_notes(conn: &Connection) -> Result<Vec<Note>> {
    let mut stmt = conn.prepare(
        "SELECT id, title, body, transcript, summary, audio_path, summary_preset, created_at, updated_at
         FROM notes ORDER BY updated_at DESC",
    )?;
    let rows = stmt
        .query_map([], map_note)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_note(conn: &Connection, id: &str) -> Result<Note> {
    let n = conn.query_row(
        "SELECT id, title, body, transcript, summary, audio_path, summary_preset, created_at, updated_at
         FROM notes WHERE id = ?1",
        params![id],
        map_note,
    )?;
    Ok(n)
}

pub fn create_note(conn: &Connection) -> Result<Note> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_ms();
    conn.execute(
        "INSERT INTO notes (id, title, body, transcript, summary, audio_path, summary_preset, created_at, updated_at)
         VALUES (?1, '', '', '', '', NULL, 'meeting', ?2, ?2)",
        params![id, now],
    )?;
    get_note(conn, &id)
}

#[derive(Debug, Default, Deserialize)]
pub struct NotePatch {
    pub title: Option<String>,
    pub body: Option<String>,
    pub transcript: Option<String>,
    pub summary: Option<String>,
    pub summary_preset: Option<String>,
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
    Ok(())
}

pub fn append_transcript(conn: &Connection, id: &str, text: &str) -> Result<String> {
    let mut current: String = conn.query_row(
        "SELECT transcript FROM notes WHERE id = ?1",
        params![id],
        |row| row.get(0),
    )?;
    if !current.is_empty() && !current.ends_with(' ') {
        current.push(' ');
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
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}
