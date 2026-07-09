//! Summary generation. Reads a note's body + transcript, resolves the
//! preset prompt and the target provider (cloud OpenAI or a local
//! OpenAI-compatible server), streams thinking/content deltas to the
//! frontend, and writes the result back. Surfaced to the frontend via
//! `commands::summarize_note` (re-exported from the parent module).
//! `read_provider_api_key` comes from the `api_keys` sibling module via
//! the parent re-export.

use super::read_provider_api_key;
use super::{DEFAULT_LANGUAGE, DEFAULT_LOCAL_LLM_BASE_URL, DEFAULT_SUMMARY_MODEL, DEFAULT_SUMMARY_PROMPT};
use crate::db::{self, Note, NotePatch};
use crate::languages;
use crate::openai;
use crate::presets;
use crate::recording::{StreamDeltaPayload, SummaryPayload, SummaryStatusPayload};
use crate::AppState;
use tauri::{AppHandle, Emitter, Manager, State};

// Resolved provider for a single summary call. Both cloud OpenAI and any
// local OpenAI-compatible server (Ollama, LM Studio, llama-server, vLLM)
// flow through this same shape — the only difference is `base_url`.
struct ResolvedProvider {
    base_url: String,
    api_key: String,
    model: String,
    // Ollama-only knob: enable thinking mode (Qwen 3+ <think>...</think>
    // chain-of-thought). Default off; flipping it on lets users A/B the
    // quality difference in dev. Ignored for cloud + non-Ollama servers.
    think: bool,
}

// Decide whether this note's summary call should hit cloud OpenAI or a
// local OpenAI-compatible server. Note-level override beats the global
// setting; default is openai.
//
// For local: reads `local_llm_base_url` and `local_llm_model` from settings.
// `api_key` is forwarded as-is — local servers typically ignore it but
// Ollama requires a non-empty bearer string, so we send a sentinel.
fn resolve_provider(
    conn: &rusqlite::Connection,
    note: &Note,
    openai_api_key: Option<String>,
) -> anyhow::Result<ResolvedProvider> {
    let note_override = note.summary_provider.trim();
    let provider = if note_override.is_empty() {
        db::get_setting(conn, "summary_provider")
            .ok()
            .flatten()
            .unwrap_or_else(|| "openai".into())
    } else {
        note_override.to_string()
    };
    eprintln!(
        "[llm] resolve_provider: note={} note_override={:?} effective={}",
        note.id, note_override, provider
    );

    match provider.as_str() {
        "local" => {
            let base_url = db::get_setting(conn, "local_llm_base_url")?
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| DEFAULT_LOCAL_LLM_BASE_URL.to_string());
            let model = db::get_setting(conn, "local_llm_model")?
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow::anyhow!(
                    "local LLM model not configured — pick one in Settings"
                ))?;
            let think = db::get_setting(conn, "local_llm_think")?
                .map(|s| s.trim().eq_ignore_ascii_case("true"))
                .unwrap_or(false);
            eprintln!("[llm] resolved local: url={base_url} model={model} think={think}");
            Ok(ResolvedProvider {
                base_url,
                api_key: "humla-local".into(),
                model,
                think,
            })
        }
        _ => {
            let api_key = openai_api_key
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow::anyhow!("OpenAI API key not set"))?;
            let model = db::get_setting(conn, "summary_model")?
                .unwrap_or_else(|| DEFAULT_SUMMARY_MODEL.to_string());
            eprintln!("[llm] resolved openai: model={model}");
            Ok(ResolvedProvider {
                base_url: openai::BASE.into(),
                api_key,
                model,
                think: false,
            })
        }
    }
}

#[tauri::command]
pub async fn summarize_note(app: AppHandle, note_id: String) -> Result<(), String> {
    eprintln!("[llm] summarize_note invoked for note={note_id}");
    // Per-note `summary_status` channel keeps the summary spinner from
    // touching `recording_status`. Summarising note B while note A
    // records used to wipe A's recording state on completion (`note_id:
    // None, Phase::Idle` on the shared channel); the dedicated event
    // makes the two lifecycles independent.
    emit_summary_status(&app, &note_id, true);
    let result = run_summary(app.clone(), note_id.clone()).await;
    emit_summary_status(&app, &note_id, false);
    match &result {
        Ok(()) => eprintln!("[llm] summarize_note succeeded"),
        Err(e) => eprintln!("[llm] summarize_note failed: {e:#}"),
    }
    result.map_err(|e| e.to_string())
}

/// Resolve the summary prompt for a note. Three cases, in priority order:
///   1. `custom:<id>` — a user-defined prompt row. Look it up; if the row
///      was deleted out from under us, fall back to the built-in default
///      prompt so the summary still runs.
///   2. `"custom"` — the legacy single-prompt sentinel from before the
///      summary_prompts table. The legacy `summary_prompt` setting is no
///      longer read here (migrate_summary_prompts seeds a prompt row for
///      upgraders, rewriting these notes to `custom:<id>`); any note that
///      still carries the bare sentinel falls back to the built-in default
///      prompt.
///   3. Built-in preset value ("meeting", "lecture", etc.) — language-aware
///      via presets::prompt.
fn resolve_prompt(conn: &rusqlite::Connection, note: &Note, language: &str) -> String {
    if let Some(id) = note.summary_preset.strip_prefix("custom:") {
        match db::get_summary_prompt(conn, id) {
            Ok(p) => p.content,
            Err(_) => DEFAULT_SUMMARY_PROMPT.to_string(),
        }
    } else if note.summary_preset == "custom" {
        DEFAULT_SUMMARY_PROMPT.to_string()
    } else {
        presets::prompt(&note.summary_preset, language)
    }
}

async fn run_summary(app: AppHandle, note_id: String) -> anyhow::Result<()> {
    let state: State<AppState> = app.state();
    // Read the API key out of band — keychain lookup shouldn't sit
    // inside the DB lock that resolve_provider takes.
    let openai_api_key = read_provider_api_key(&state, "openai")
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let (provider, language, note) = {
        let conn = state.db.lock();
        let n = db::get_note(&conn, &note_id)?;
        let p_resolved = resolve_provider(&conn, &n, openai_api_key)?;
        let global_lang = db::get_setting(&conn, "language")?
            .unwrap_or_else(|| DEFAULT_LANGUAGE.to_string());
        // Same fallback rule as transcription: note language wins, empty
        // means "follow the global default".
        let lang = if n.language.trim().is_empty() {
            global_lang
        } else {
            n.language.clone()
        };
        (p_resolved, lang, n)
    };
    if note.transcript.trim().is_empty() && note.body.trim().is_empty() {
        return Ok(());
    }
    let prompt = {
        let conn = state.db.lock();
        resolve_prompt(&conn, &note, &language)
    };
    let body_text = crate::html_text::html_to_text(&note.body);
    // Always send both labels even when one side is empty. Sending only
    // [Transkripsjon] while the system prompt references [Notater] sends
    // thinking models down a rabbit hole second-guessing whether the notes
    // are missing or hidden — a real failure mode we observed in dev where
    // Qwen 3.5 spent thousands of reasoning tokens on it. Explicit "(ingen)"
    // tells the model the field is genuinely absent.
    let notes_block = if body_text.trim().is_empty() {
        "[Notater]\n(ingen)".to_string()
    } else {
        format!("[Notater]\n{body_text}")
    };
    let transcript_block = if note.transcript.trim().is_empty() {
        "[Transkripsjon]\n(ingen)".to_string()
    } else {
        format!("[Transkripsjon]\n{}", note.transcript)
    };
    let user_message = format!("{notes_block}\n\n{transcript_block}");
    // Hard language directive in case the prompt was authored in a different
    // language than the user has now chosen.
    let full_prompt = format!("{prompt}\n\n{}", language_directive(&language));
    // Stream thinking + content deltas to the frontend so the user sees
    // the model working in real time. Especially valuable when think=true
    // on Qwen 3.5+ — without live feedback users wait minutes wondering if
    // it's stuck.
    let app_for_stream = app.clone();
    let note_for_stream = note_id.clone();
    let summary = openai::summarize_with_base(
        &provider.base_url,
        &provider.api_key,
        &provider.model,
        provider.think,
        &full_prompt,
        &user_message,
        move |chunk| match chunk {
            openai::StreamChunk::Thinking(t) => {
                let _ = app_for_stream.emit(
                    "summary_thinking_delta",
                    StreamDeltaPayload {
                        note_id: note_for_stream.clone(),
                        delta: t.to_string(),
                    },
                );
            }
            openai::StreamChunk::Content(c) => {
                let _ = app_for_stream.emit(
                    "summary_content_delta",
                    StreamDeltaPayload {
                        note_id: note_for_stream.clone(),
                        delta: c.to_string(),
                    },
                );
            }
        },
    )
    .await?;
    let state: State<AppState> = app.state();
    {
        let conn = state.db.lock();
        db::update_note(&conn, &note_id, &NotePatch {
            summary: Some(summary.clone()),
            ..Default::default()
        })?;
    }
    state.sync.note_upserted(&note_id); // AI summary written → enqueue a push
    let _ = app.emit("summary_ready", SummaryPayload { note_id, summary });
    Ok(())
}

fn emit_summary_status(app: &AppHandle, note_id: &str, active: bool) {
    let _ = app.emit("summary_status", SummaryStatusPayload {
        note_id: note_id.to_string(),
        active,
    });
}

// Hard directive appended to the summary system prompt. Enforces output
// language regardless of which language the user wrote their prompt in.
fn language_directive(lang: &str) -> String {
    // Native imperatives for the common Nordic codes — the model picks up
    // the target language faster from a directive written in that language
    // than from "Write in Norwegian." in English. Everything else falls back
    // to a generated "IMPORTANT: Write the entire response in {Name}." using
    // the English language name from the shared lookup.
    match lang {
        "no" => "VIKTIG: Skriv hele svaret på norsk.".to_string(),
        "sv" => "VIKTIGT: Skriv hela svaret på svenska.".to_string(),
        "da" => "VIGTIGT: Skriv hele svaret på dansk.".to_string(),
        "auto" => "Respond in the same language as the user's notes.".to_string(),
        other => format!(
            "IMPORTANT: Write the entire response in {}.",
            languages::english_name(other)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_conn() -> (tempfile::TempDir, rusqlite::Connection) {
        let dir = tempfile::tempdir().unwrap();
        let conn = db::open(&dir.path().join("summary-test.sqlite")).unwrap();
        (dir, conn)
    }

    #[test]
    fn custom_id_resolves_to_the_stored_prompt_row() {
        let (_dir, conn) = temp_conn();
        let row = db::create_summary_prompt(&conn, "Mine", "MY CUSTOM PROMPT", "").unwrap();
        let note = db::create_note(&conn, "no", &format!("custom:{}", row.id), "").unwrap();
        assert_eq!(resolve_prompt(&conn, &note, "no"), "MY CUSTOM PROMPT");
    }

    #[test]
    fn deleted_custom_row_falls_back_to_the_builtin_default() {
        // A note pointing at a custom prompt row that has since been
        // deleted must still summarise — using the built-in default, not
        // the retired `summary_prompt` setting.
        let (_dir, conn) = temp_conn();
        let note = db::create_note(&conn, "no", "custom:does-not-exist", "").unwrap();
        assert_eq!(resolve_prompt(&conn, &note, "no"), DEFAULT_SUMMARY_PROMPT);
    }

    #[test]
    fn legacy_custom_sentinel_falls_back_to_the_builtin_default() {
        // Pre-table `"custom"` sentinel that never got migrated. No longer
        // reads the legacy `summary_prompt` setting.
        let (_dir, conn) = temp_conn();
        let note = db::create_note(&conn, "no", "custom", "").unwrap();
        assert_eq!(resolve_prompt(&conn, &note, "no"), DEFAULT_SUMMARY_PROMPT);
    }

    #[test]
    fn builtin_preset_uses_the_language_aware_preset_prompt() {
        let (_dir, conn) = temp_conn();
        let note = db::create_note(&conn, "no", "meeting", "").unwrap();
        assert_eq!(
            resolve_prompt(&conn, &note, "no"),
            presets::prompt("meeting", "no")
        );
    }
}
