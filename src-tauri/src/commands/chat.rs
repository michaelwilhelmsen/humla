//! Chat commands (issue #46). `chat_send` runs a single-pass, Note-grounded
//! completion: it resolves the configured chat provider, grounds the prompt in
//! the current Note's content (as reference material, never the system prompt),
//! streams the answer to the frontend, and persists the turn. `chat_history`
//! reloads a Note's conversation after restart. The heavy lifting (prompt
//! assembly, budget, streaming orchestration) lives Tauri-free in `crate::chat`.

use super::{DEFAULT_LOCAL_LLM_BASE_URL, DEFAULT_SUMMARY_MODEL};
use crate::chat::{self, ChatCtx, ChatEvent, Citation, ToolScope};
use crate::db::{self, CHAT_SCOPE_NOTE, CHAT_TENANT_PERSONAL};
use crate::embed::{self, EmbeddingAdapter, OLLAMA_EMBED_MODEL, OPENAI_EMBED_MODEL};
use crate::openai;
use crate::AppState;
use parking_lot::Mutex;
use rusqlite::Connection;
use serde::Serialize;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};

/// Derive the embedding config from the resolved chat provider (issue #48) —
/// no user-facing embedding choice. Cloud chat embeds with OpenAI's
/// `text-embedding-3-small`; local chat with `embeddinggemma` via the same
/// Ollama server. Both speak the OpenAI-compatible `/v1/embeddings` shape.
fn resolve_embed(resolved: &ResolvedChat) -> embed::EmbedConfig {
    match resolved.provider.as_str() {
        "ollama" => embed::EmbedConfig {
            provider: "ollama",
            model: OLLAMA_EMBED_MODEL,
            base_url: resolved.base_url.clone(),
            api_key: None,
        },
        _ => embed::EmbedConfig {
            provider: "openai",
            model: OPENAI_EMBED_MODEL,
            base_url: resolved.base_url.clone(),
            api_key: resolved.api_key.clone(),
        },
    }
}

/// Embed a Note's not-yet-embedded chunks under the adapter's model and cache
/// them (issue #48). Content-addressed, so only changed/new text embeds. Best-
/// effort: any failure (model missing, API error) logs and returns — chat still
/// works keyword-only. The DB lock is never held across the embed `.await`.
pub(crate) async fn embed_note(
    db: &Arc<Mutex<Connection>>,
    adapter: &dyn EmbeddingAdapter,
    note_id: &str,
) {
    let need = {
        let conn = db.lock();
        db::note_texts_needing_embedding(&conn, note_id, adapter.model_id()).unwrap_or_default()
    };
    if need.is_empty() {
        return;
    }
    let texts: Vec<String> = need.iter().map(|(_, t)| t.clone()).collect();
    match adapter.embed(&texts).await {
        Ok(vectors) if vectors.len() == need.len() => {
            let conn = db.lock();
            for ((hash, _), vector) in need.iter().zip(vectors.iter()) {
                let _ = db::store_embedding(&conn, hash, adapter.model_id(), vector);
            }
        }
        Ok(vectors) => eprintln!(
            "[chat] embed count mismatch for note {note_id}: {} vectors for {} chunks",
            vectors.len(),
            need.len()
        ),
        Err(e) => eprintln!("[chat] embed_note {note_id} failed (keyword-only): {e}"),
    }
}

/// Resolve the embedding provider and embed a Note off the request path
/// (content-settled checkpoint). Spawned fire-and-forget after a re-chunk so
/// editing never blocks on embedding. No-op if no provider is configured.
pub async fn embed_note_bg(app: AppHandle, note_id: String) {
    let state: State<AppState> = app.state();
    let key = super::read_provider_api_key(&state, "openai").ok().flatten();
    let resolved = {
        let conn = state.db.lock();
        resolve_chat(&conn, key)
    };
    let Ok(resolved) = resolved else { return };
    let adapter = resolve_embed(&resolved).adapter();
    embed_note(&state.db, &adapter, &note_id).await;
}

/// Rebuild one Note's retrieval chunks from its current content. The single
/// place body-HTML→text + `db::reindex_note` are combined, called from every
/// content-settled checkpoint (after summarize, after diarization, on
/// Note-view unmount) and the startup backfill. Best-effort: a failure to
/// index never blocks the user-facing action.
pub(crate) fn reindex_note_content(conn: &rusqlite::Connection, note_id: &str) {
    if let Ok(note) = db::get_note(conn, note_id) {
        let body_text = crate::html_text::html_to_text(&note.body);
        if let Err(e) = db::reindex_note(conn, note_id, &body_text, &note.transcript, &note.summary) {
            eprintln!("[chat] reindex of note {note_id} failed: {e}");
        }
    }
}

/// One-time backfill: index every live Note that has no chunks yet, so notes
/// created before #47 become searchable. Cheap and idempotent — reruns index
/// only the still-missing ones. Runs off-thread at startup.
pub fn backfill_note_chunks(db: &std::sync::Arc<parking_lot::Mutex<rusqlite::Connection>>) {
    let ids = {
        let conn = db.lock();
        db::note_ids_needing_reindex(&conn).unwrap_or_default()
    };
    if ids.is_empty() {
        return;
    }
    eprintln!("[chat] backfilling retrieval chunks for {} note(s)", ids.len());
    for id in ids {
        let conn = db.lock();
        reindex_note_content(&conn, &id);
    }
}

/// One-time embedding backfill (issue #48): after chunks exist, embed every
/// Note that still lacks vectors under the current model, so semantic search
/// covers the whole corpus on day one — not only notes touched since #48.
/// Runs off the request path at startup, best-effort (a missing model / API
/// error just leaves those notes keyword-only). Batched per Note + cached, so
/// it's idempotent and cheap on reruns.
pub async fn embed_backfill(app: AppHandle) {
    let state: State<AppState> = app.state();
    let key = super::read_provider_api_key(&state, "openai").ok().flatten();
    let resolved = {
        let conn = state.db.lock();
        resolve_chat(&conn, key)
    };
    let Ok(resolved) = resolved else { return };
    let adapter = resolve_embed(&resolved).adapter();
    let ids = {
        let conn = state.db.lock();
        db::note_ids_needing_embedding(&conn, adapter.model_id()).unwrap_or_default()
    };
    if ids.is_empty() {
        return;
    }
    eprintln!("[chat] embedding backfill: {} note(s) under {}", ids.len(), adapter.model_id());
    for id in ids {
        embed_note(&state.db, &adapter, &id).await;
    }
    eprintln!("[chat] embedding backfill complete");
}

/// Rebuild a Note's retrieval index on demand — the frontend calls this when
/// the Note view unmounts, so edits made without triggering summarize/diarize
/// still land in search. Re-chunks synchronously, then embeds off the request
/// path (issue #48) so unmount stays snappy.
#[tauri::command]
pub fn chat_reindex_note(app: AppHandle, note_id: String) -> Result<(), String> {
    {
        let state: State<AppState> = app.state();
        let conn = state.db.lock();
        reindex_note_content(&conn, &note_id);
    }
    tauri::async_runtime::spawn(embed_note_bg(app, note_id));
    Ok(())
}

/// Resolve the retrieval breadth the user picked in the Scope popover into a
/// server-enforced `ToolScope`. "folder" resolves to the anchor Note's folder;
/// a folder-less Note falls back to Note breadth (there's no folder to widen
/// to). Anything unrecognised is treated as the safe single-Note breadth.
fn resolve_scope(breadth: &str, note: &db::Note) -> ToolScope {
    match breadth {
        "all" => ToolScope::All,
        "folder" => match note.folder_id.as_deref() {
            Some(folder_id) if !folder_id.is_empty() => ToolScope::Folder(folder_id.to_string()),
            _ => ToolScope::Note(note.id.clone()),
        },
        _ => ToolScope::Note(note.id.clone()),
    }
}

/// Whether a model id is embedding-only (can't do chat completions). Mirrors
/// the frontend `isEmbeddingModel` heuristic — matches embeddinggemma and the
/// common embedding families.
fn is_embedding_model(model: &str) -> bool {
    let m = model.to_lowercase();
    m.contains("embed")
        || m.starts_with("bge-")
        || m.starts_with("all-minilm")
        || m.starts_with("paraphrase-")
}

// Resolved chat provider for a single call. Only "openai" (cloud, shared key)
// and "ollama" (local) are valid — see issue #44.
struct ResolvedChat {
    provider: String,
    base_url: String,
    api_key: Option<String>,
    model: String,
    think: bool,
}

fn resolve_chat(
    conn: &rusqlite::Connection,
    openai_api_key: Option<String>,
) -> anyhow::Result<ResolvedChat> {
    // Read a setting as a non-empty trimmed value, or None.
    let setting = |key: &str| -> anyhow::Result<Option<String>> {
        Ok(db::get_setting(conn, key)?
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()))
    };

    let provider = setting("chat_provider")?.unwrap_or_else(|| "openai".into());
    let model_setting = setting("chat_model")?;

    match provider.as_str() {
        "ollama" => {
            let base_url =
                setting("local_llm_base_url")?.unwrap_or_else(|| DEFAULT_LOCAL_LLM_BASE_URL.to_string());
            let think = db::get_setting(conn, "local_llm_think")?
                .map(|s| s.trim().eq_ignore_ascii_case("true"))
                .unwrap_or(false);
            let model = model_setting.ok_or_else(|| {
                anyhow::anyhow!("No chat model configured — pick one in Settings → Chat.")
            })?;
            // Defence in depth: an embedding model (e.g. embeddinggemma, pulled
            // for semantic search) can't do chat — Ollama 400s "does not support
            // chat". The pickers now exclude it, but guard a stale setting too.
            if is_embedding_model(&model) {
                anyhow::bail!(
                    "“{model}” is an embedding model and can't chat — pick a chat model in \
                     Settings → Chat."
                );
            }
            Ok(ResolvedChat { provider, base_url, api_key: None, model, think })
        }
        _ => {
            let api_key = openai_api_key.filter(|s| !s.is_empty()).ok_or_else(|| {
                anyhow::anyhow!("OpenAI API key not set — add one in Settings → Chat.")
            })?;
            // A fresh install may have a key but no explicit model yet; fall
            // back to the default chat-class model rather than erroring.
            let model = model_setting.unwrap_or_else(|| DEFAULT_SUMMARY_MODEL.to_string());
            Ok(ResolvedChat {
                provider,
                base_url: openai::BASE.into(),
                api_key: Some(api_key),
                model,
                think: false,
            })
        }
    }
}

// ── Event payloads (camelCase to match the app's other Tauri events) ────────
// Slice-3 subset of the #46 wire contract: text-delta, done, error. Each
// carries conversationId; the delta/done ones also carry the assistant
// messageId (the row exists by then).

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChatTextDeltaPayload {
    conversation_id: String,
    message_id: String,
    block_id: String,
    delta: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChatDonePayload {
    conversation_id: String,
    message_id: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChatErrorPayload {
    conversation_id: String,
    message: String,
}

/// Tool activity between steps — drives the "searching your notes…" progress
/// line (issue #47, story 18).
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChatToolActivityPayload {
    conversation_id: String,
    message_id: String,
    name: String,
    is_error: bool,
}

/// Sources gathered so far, for citation chips (story 28).
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChatCitationsPayload {
    conversation_id: String,
    message_id: String,
    citations: Vec<Citation>,
}

/// Synchronous result of `chat_send`: enough for the UI to attach a
/// "context truncated" notice to the turn and to know which conversation it
/// landed in. The streamed answer arrives via events, not here.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSendResult {
    conversation_id: String,
    truncated: bool,
}

#[tauri::command]
pub async fn chat_send(
    app: AppHandle,
    note_id: String,
    message: String,
    scope: Option<String>,
) -> Result<ChatSendResult, String> {
    let state: State<AppState> = app.state();
    // Keychain read out of band — not inside the DB lock. Chat reuses the
    // shared OpenAI key (issue #44).
    let openai_api_key = super::read_provider_api_key(&state, "openai")?;
    let breadth = scope.unwrap_or_else(|| "note".into());

    let (grounding, resolved, conversation_id, tool_scope, workspace) = {
        let conn = state.db.lock();
        let note = db::get_note(&conn, &note_id).map_err(|e| e.to_string())?;
        let workspace = super::cloud::active_workspace(&conn);
        let resolved = resolve_chat(&conn, openai_api_key).map_err(|e| e.to_string())?;
        // Conversation is keyed by the anchor Note; breadth is a live filter
        // within it, not a new conversation (issue #47).
        let conversation =
            db::get_or_create_conversation(&conn, CHAT_TENANT_PERSONAL, CHAT_SCOPE_NOTE, &note_id)
                .map_err(|e| e.to_string())?;
        // Keep the anchor Note searchable — reindex it now so "this Note" and
        // any broader search always find the note the user is looking at, even
        // if a content-settled checkpoint hasn't fired yet.
        let body_text = crate::html_text::html_to_text(&note.body);
        let _ = db::reindex_note(&conn, &note.id, &body_text, &note.transcript, &note.summary);
        let tool_scope = resolve_scope(&breadth, &note);
        let grounding = chat::build_grounding(&body_text, &note.transcript, &note.summary);
        (grounding, resolved, conversation.id, tool_scope, workspace)
    };

    // Embed the anchor Note now (best-effort, cached) so semantic search works
    // for the note the user is chatting about on the very first question. Other
    // notes are embedded at their own checkpoints (issue #48).
    let embed_cfg = resolve_embed(&resolved);
    let embedder = embed_cfg.adapter();
    embed_note(&state.db, &embedder, &note_id).await;

    let adapter = chat::build_chat_adapter(&resolved.provider);
    let conv_for_sink = conversation_id.clone();
    let app_for_sink = app.clone();
    let sink = move |ev: ChatEvent| match ev {
        ChatEvent::TextDelta { message_id, block_id, delta } => {
            let _ = app_for_sink.emit(
                "chat_text_delta",
                ChatTextDeltaPayload {
                    conversation_id: conv_for_sink.clone(),
                    message_id,
                    block_id,
                    delta,
                },
            );
        }
        ChatEvent::ToolActivity { message_id, name, is_error } => {
            let _ = app_for_sink.emit(
                "chat_tool_activity",
                ChatToolActivityPayload {
                    conversation_id: conv_for_sink.clone(),
                    message_id,
                    name,
                    is_error,
                },
            );
        }
        ChatEvent::Citations { message_id, citations } => {
            let _ = app_for_sink.emit(
                "chat_citations",
                ChatCitationsPayload { conversation_id: conv_for_sink.clone(), message_id, citations },
            );
        }
        ChatEvent::Done { message_id } => {
            let _ = app_for_sink.emit(
                "chat_done",
                ChatDonePayload { conversation_id: conv_for_sink.clone(), message_id },
            );
        }
    };

    let ctx = ChatCtx {
        model: &resolved.model,
        api_key: resolved.api_key.as_deref(),
        base_url: &resolved.base_url,
        think: resolved.think,
    };
    let result = chat::run_chat(
        &state.db,
        adapter.as_ref(),
        ctx,
        &conversation_id,
        &grounding.text,
        &tool_scope,
        &workspace,
        Some(&embedder as &dyn EmbeddingAdapter),
        &message,
        sink,
    )
    .await;

    match result {
        Ok(()) => Ok(ChatSendResult { conversation_id, truncated: grounding.truncated }),
        Err(e) => {
            let message = e.to_string();
            let _ = app.emit(
                "chat_error",
                ChatErrorPayload { conversation_id: conversation_id.clone(), message: message.clone() },
            );
            Err(message)
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessageDto {
    id: String,
    role: String,
    seq: i64,
    parts: Vec<chat::Part>,
    created_at: i64,
}

/// Reload a Note's conversation (empty if none exists yet). Drives history
/// restore when the Chat tab opens or after an app restart.
#[tauri::command]
pub fn chat_history(state: State<AppState>, note_id: String) -> Result<Vec<ChatMessageDto>, String> {
    let conn = state.db.lock();
    let Some(conversation) =
        db::get_conversation(&conn, CHAT_TENANT_PERSONAL, CHAT_SCOPE_NOTE, &note_id)
            .map_err(|e| e.to_string())?
    else {
        return Ok(Vec::new());
    };
    let messages = db::list_chat_messages(&conn, &conversation.id).map_err(|e| e.to_string())?;
    Ok(messages
        .into_iter()
        .map(|m| ChatMessageDto {
            id: m.id,
            role: m.role,
            seq: m.seq,
            parts: chat::parse_parts(&m.content),
            created_at: m.created_at,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedding_models_are_recognised() {
        for m in ["embeddinggemma", "embeddinggemma:latest", "nomic-embed-text", "mxbai-embed-large", "bge-m3", "all-minilm", "snowflake-arctic-embed2", "paraphrase-multilingual"] {
            assert!(is_embedding_model(m), "expected embedding: {m}");
        }
        for m in ["gemma4:12b-mlx", "qwen3.5:4b", "llama3.2:3b", "gpt-5.4-mini"] {
            assert!(!is_embedding_model(m), "expected chat-capable: {m}");
        }
    }

    #[test]
    fn resolve_chat_rejects_an_embedding_model_as_the_chat_model() {
        let dir = tempfile::tempdir().unwrap();
        let conn = db::open(&dir.path().join("t.sqlite")).unwrap();
        db::set_setting(&conn, "chat_provider", "ollama").unwrap();
        db::set_setting(&conn, "chat_model", "embeddinggemma:latest").unwrap();
        let res = resolve_chat(&conn, None);
        assert!(res.is_err());
        let err = res.err().unwrap().to_string();
        assert!(err.contains("embedding model"), "clear guidance, not a raw 400: {err}");
    }
}
