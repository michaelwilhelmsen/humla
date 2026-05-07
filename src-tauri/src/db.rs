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
    // Optional speaker count hint, passed through to the offline diarizer
    // as `OfflineDiarizerConfig.withSpeakers(exactly: N)`. `None` (or 0)
    // means "let VBx auto-detect" — the default for fresh notes. A positive
    // value pins the cluster count, which is the most reliable fix for
    // dominant-speaker conversations where auto-detect collapses to 1.
    pub expected_speakers: Option<i64>,
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

        CREATE TABLE IF NOT EXISTS summary_prompts (
            id          TEXT PRIMARY KEY,
            name        TEXT NOT NULL,
            content     TEXT NOT NULL,
            created_at  INTEGER NOT NULL,
            updated_at  INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_summary_prompts_updated
            ON summary_prompts(updated_at DESC);
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
    let _ = conn.execute(
        "ALTER TABLE notes ADD COLUMN expected_speakers INTEGER",
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

const NOTE_COLS: &str = "id, title, body, transcript, summary, audio_path, summary_preset, folder_id, language, summary_provider, expected_speakers, created_at, updated_at";

pub fn list_notes(conn: &Connection) -> Result<Vec<Note>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {NOTE_COLS} FROM notes ORDER BY updated_at DESC"
    ))?;
    let rows = stmt
        .query_map([], map_note)?
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
) -> Result<Note> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_ms();
    conn.execute(
        "INSERT INTO notes (id, title, body, transcript, summary, audio_path, summary_preset, folder_id, language, summary_provider, expected_speakers, created_at, updated_at)
         VALUES (?1, '', '', '', '', NULL, ?2, NULL, ?3, '', NULL, ?4, ?4)",
        params![id, default_preset, default_language, now],
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
    let mut stmt = conn.prepare_cached(
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

pub fn delete_note(conn: &Connection, id: &str) -> Result<()> {
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
}

pub fn list_summary_prompts(conn: &Connection) -> Result<Vec<SummaryPrompt>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, name, content, created_at, updated_at FROM summary_prompts
         ORDER BY name COLLATE NOCASE",
    )?;
    let rows = stmt
        .query_map([], map_summary_prompt)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn get_summary_prompt(conn: &Connection, id: &str) -> Result<SummaryPrompt> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, name, content, created_at, updated_at FROM summary_prompts WHERE id = ?1",
    )?;
    let p = stmt.query_row(params![id], map_summary_prompt)?;
    Ok(p)
}

pub fn create_summary_prompt(
    conn: &Connection,
    name: &str,
    content: &str,
) -> Result<SummaryPrompt> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = now_ms();
    conn.execute(
        "INSERT INTO summary_prompts (id, name, content, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?4)",
        params![id, name, content, now],
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
    })
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
    let row = create_summary_prompt(conn, "Custom prompt (migrated)", &legacy)?;
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
