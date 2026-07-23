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

/// Whether a Note currently sits in a (non-empty) folder.
fn note_has_folder(note: &db::Note) -> bool {
    note.folder_id.as_deref().is_some_and(|f| !f.is_empty())
}

/// Read the breadth a turn runs at, self-healing a stale value as a side effect
/// (issue #58). The name says `heal_and_read` because this MUTATES: a stored
/// "folder" breadth on a Note that no longer has a folder is reset to "note"
/// (persisted here) so the stored value, the request scope, and the Scope chip
/// never diverge. The folder-edge policy for this issue is auto-heal, not a
/// request-time error, so removing a Note's folder never breaks chat behind the
/// user's back. Errors loudly on an unrecognised stored value rather than
/// clamping — a corrupt row is a bug, not a note-scope turn.
fn heal_and_read_breadth(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    stored: &str,
    note: &db::Note,
) -> Result<String, String> {
    let breadth = chat::validate_breadth(stored)?;
    if breadth == "folder" && !note_has_folder(note) {
        db::set_conversation_breadth(conn, conversation_id, "note").map_err(|e| e.to_string())?;
        return Ok("note".into());
    }
    Ok(breadth.to_string())
}

/// Resolve an (already-effective) breadth into a server-enforced `ToolScope`.
/// "folder" resolves to the anchor Note's folder. Unrecognised breadths and a
/// folder breadth without a folder are loud errors (issue #58) rather than a
/// silent clamp to Note — in practice `heal_and_read_breadth` heals the
/// folder-less case upstream, so those arms are belt-and-suspenders.
fn resolve_scope(breadth: &str, note: &db::Note) -> Result<ToolScope, String> {
    match chat::validate_breadth(breadth)? {
        "all" => Ok(ToolScope::All),
        "folder" => match note.folder_id.as_deref() {
            Some(folder_id) if !folder_id.is_empty() => Ok(ToolScope::Folder(folder_id.to_string())),
            _ => Err("This note isn't in a folder, so \"Folder\" scope isn't available.".into()),
        },
        _ => Ok(ToolScope::Note(note.id.clone())),
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

/// The loaded chat context (issue #58): Personal (on-device) or one specific
/// workspace (Teams/cloud). This type is the SINGLE owner of the "active
/// workspace, else Personal" rule — every chat command derives its context via
/// `ChatContext::load` instead of re-checking `active_workspace` emptiness
/// itself. There is no user-chosen tenant; it follows the sidebar workspace.
enum ChatContext {
    Personal,
    Workspace(String),
}

impl ChatContext {
    /// Derive the context from the active workspace setting — the one place the
    /// rule lives.
    fn load(conn: &rusqlite::Connection) -> Self {
        let workspace = super::cloud::active_workspace(conn);
        if workspace.is_empty() {
            ChatContext::Personal
        } else {
            ChatContext::Workspace(workspace)
        }
    }

    /// The `conversations.tenant` string for this context.
    fn tenant(&self) -> &str {
        match self {
            ChatContext::Personal => CHAT_TENANT_PERSONAL,
            ChatContext::Workspace(id) => id,
        }
    }

    /// The workspace id when this is a workspace context, else None. Presence of
    /// a workspace is what routes a turn to the cloud (Teams) path.
    fn workspace(&self) -> Option<&str> {
        match self {
            ChatContext::Personal => None,
            ChatContext::Workspace(id) => Some(id),
        }
    }
}

/// Persist the Scope chip's breadth on the current context's conversation
/// (issue #58) — the single source of truth for retrieval breadth. Validates
/// the value loudly (garbage → Err), then get-or-creates the conversation for
/// the loaded context (Personal or the active workspace) and writes the column.
#[tauri::command]
pub fn chat_set_breadth(
    app: AppHandle,
    note_id: String,
    breadth: String,
) -> Result<(), String> {
    chat::validate_breadth(&breadth)?;
    let state: State<AppState> = app.state();
    let conn = state.db.lock();
    let ctx = ChatContext::load(&conn);
    let conversation =
        db::get_or_create_conversation(&conn, ctx.tenant(), CHAT_SCOPE_NOTE, &note_id)
            .map_err(|e| e.to_string())?;
    db::set_conversation_breadth(&conn, &conversation.id, &breadth).map_err(|e| e.to_string())
}

/// Read the persisted breadth for the current context's conversation so the
/// Scope chip initialises from the backend in one round trip (issue #58).
/// Returns the safe "note" default when no conversation exists yet. NOTE: this
/// read may PERSIST a heal — a stale "folder" breadth (the Note's folder was
/// since removed) is reset to "note" via `heal_and_read_breadth` so the chip
/// shows the corrected value; the heal-on-read is intentional.
#[tauri::command]
pub fn chat_get_breadth(app: AppHandle, note_id: String) -> Result<String, String> {
    let state: State<AppState> = app.state();
    let conn = state.db.lock();
    let ctx = ChatContext::load(&conn);
    let Some(conversation) =
        db::get_conversation(&conn, ctx.tenant(), CHAT_SCOPE_NOTE, &note_id)
            .map_err(|e| e.to_string())?
    else {
        return Ok("note".into());
    };
    let note = db::get_note(&conn, &note_id).map_err(|e| e.to_string())?;
    heal_and_read_breadth(&conn, &conversation.id, &conversation.breadth, &note)
}

#[tauri::command]
pub async fn chat_send(
    app: AppHandle,
    note_id: String,
    message: String,
) -> Result<ChatSendResult, String> {
    let state: State<AppState> = app.state();
    // Chat is pinned to the loaded context (issue #58): a loaded workspace →
    // the Teams (cloud) path; Personal (no workspace) → the on-device path
    // below. There is no user-chosen tenant — it follows the sidebar workspace.
    let in_workspace = {
        let conn = state.db.lock();
        ChatContext::load(&conn).workspace().is_some()
    };
    if in_workspace {
        return chat_send_cloud(app, note_id, message).await;
    }

    // Keychain read out of band — not inside the DB lock. Chat reuses the
    // shared OpenAI key (issue #44).
    let openai_api_key = super::read_provider_api_key(&state, "openai")?;

    let (grounding, resolved, conversation_id, tool_scope, workspace) = {
        let conn = state.db.lock();
        let note = db::get_note(&conn, &note_id).map_err(|e| e.to_string())?;
        // We branched to the Personal path above (no active workspace), so the
        // tenant is Personal and there's no workspace to scope tools to.
        let workspace = String::new();
        let resolved = resolve_chat(&conn, openai_api_key).map_err(|e| e.to_string())?;
        // Conversation is keyed by the anchor Note; breadth is a persisted live
        // filter within it, not a new conversation (issues #47/#58).
        let conversation =
            db::get_or_create_conversation(&conn, CHAT_TENANT_PERSONAL, CHAT_SCOPE_NOTE, &note_id)
                .map_err(|e| e.to_string())?;
        // Keep the anchor Note searchable — reindex it now so "this Note" and
        // any broader search always find the note the user is looking at, even
        // if a content-settled checkpoint hasn't fired yet.
        let body_text = crate::html_text::html_to_text(&note.body);
        let _ = db::reindex_note(&conn, &note.id, &body_text, &note.transcript, &note.summary);
        // Breadth is read from the conversation row (single source of truth),
        // self-healed against the Note's current folder.
        let breadth =
            heal_and_read_breadth(&conn, &conversation.id, &conversation.breadth, &note)?;
        let tool_scope = resolve_scope(&breadth, &note)?;
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

/// Result of one streamed workspace turn against the cloud endpoint.
struct CloudTurn {
    /// A non-2xx preflight response `(status, reason, error)` — the turn never
    /// streamed (blocked or malformed). None once the SSE stream opened.
    preflight: Option<(u16, String, String)>,
    /// The server's conversation record id, learned from the streamed events —
    /// persisted as the local conversation's `remote_id` so later turns resume.
    server_conversation_id: Option<String>,
}

/// Workspace (Teams) chat turn (issue #50): delegate to the deployed humla-cloud
/// `POST /api/chat`, stream the SSE response, and re-emit it on the SAME `chat_*`
/// events the local loop uses (story 18). The conversation is server-
/// authoritative — its messages live in the cloud (read-through via
/// `chat_history`); locally we keep only a handle row whose `remote_id` maps to
/// the server conversation. `"workspace"` always resolves to the *active*
/// workspace, so a turn can never reach a different tenant (story 5/19).
async fn chat_send_cloud(
    app: AppHandle,
    note_id: String,
    message: String,
) -> Result<ChatSendResult, String> {
    let state: State<AppState> = app.state();
    let (workspace, folder_id, title, conversation, breadth) = {
        let conn = state.db.lock();
        let ctx = ChatContext::load(&conn);
        let Some(workspace) = ctx.workspace() else {
            return Err("No workspace selected — switch to a workspace to use Team chat.".into());
        };
        let note = db::get_note(&conn, &note_id).map_err(|e| e.to_string())?;
        let conversation =
            db::get_or_create_conversation(&conn, workspace, CHAT_SCOPE_NOTE, &note_id)
                .map_err(|e| e.to_string())?;
        // Breadth is read from the (workspace) conversation row and self-healed
        // against the Note's folder, exactly like the Personal path (issue #58).
        let breadth =
            heal_and_read_breadth(&conn, &conversation.id, &conversation.breadth, &note)?;
        (workspace.to_string(), note.folder_id.clone(), note.title.clone(), conversation, breadth)
    };

    let body = chat::cloud::build_cloud_request(
        conversation.remote_id.as_deref(),
        &workspace,
        &message,
        Some(&title),
        &breadth,
        &note_id,
        folder_id.as_deref(),
    )?;

    // Stream the turn, retrying once after a 401 (a stale cached token → forget
    // it and re-authenticate from stored credentials, mirroring cloud.rs).
    let mut attempt = 0u8;
    let outcome = loop {
        let (base, token) = super::cloud::cloud_session(&state).await?;
        let turn = stream_cloud_turn(&app, &base, &token, &body, &conversation.id).await;
        match turn {
            Ok(t) => {
                if matches!(&t.preflight, Some((401, _, _))) && attempt == 0 {
                    super::cloud::forget_session();
                    attempt += 1;
                    continue;
                }
                break t;
            }
            Err(e) => {
                let _ = app.emit(
                    "chat_error",
                    ChatErrorPayload { conversation_id: conversation.id.clone(), message: e.clone() },
                );
                return Err(e);
            }
        }
    };

    if let Some((_, reason, server_msg)) = outcome.preflight {
        let text = chat::cloud::cloud_chat_error_message(&reason, &server_msg);
        let _ = app.emit(
            "chat_error",
            ChatErrorPayload { conversation_id: conversation.id.clone(), message: text.clone() },
        );
        return Err(text);
    }

    // Remember the server conversation id for resume + read-through history.
    if let Some(server_id) = outcome.server_conversation_id {
        if conversation.remote_id.as_deref() != Some(server_id.as_str()) {
            let conn = state.db.lock();
            let _ = db::set_conversation_remote_id(&conn, &conversation.id, &server_id);
        }
    }

    // Truncation is a server concern for workspace turns; the client just relays.
    Ok(ChatSendResult { conversation_id: conversation.id, truncated: false })
}

/// POST the turn and pump the SSE response, re-emitting each event on the
/// matching `chat_*` Tauri event. Server event payloads already carry the
/// frontend-shaped camelCase fields; we only re-stamp `conversationId` to the
/// LOCAL conversation id (so the UI keys turns the same across tenants) and
/// capture the server's id to persist. A non-2xx response is returned as a
/// structured preflight error instead of a stream.
async fn stream_cloud_turn(
    app: &AppHandle,
    base: &str,
    token: &str,
    body: &serde_json::Value,
    local_conversation_id: &str,
) -> Result<CloudTurn, String> {
    use futures_util::StreamExt;
    let resp = reqwest::Client::new()
        .post(format!("{}/api/chat", base.trim_end_matches('/')))
        .bearer_auth(token)
        .json(body)
        .send()
        .await
        .map_err(|e| format!("Couldn't reach Team chat: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        let val: serde_json::Value = resp.json().await.unwrap_or_default();
        let reason = val.get("reason").and_then(|r| r.as_str()).unwrap_or_default().to_string();
        let server_msg = val.get("error").and_then(|m| m.as_str()).unwrap_or_default().to_string();
        return Ok(CloudTurn {
            preflight: Some((status.as_u16(), reason, server_msg)),
            server_conversation_id: None,
        });
    }

    // Degraded fallback (AC2): if a 2xx response isn't an SSE stream, treat it as
    // a non-streamed whole-turn — emit the answer as one chunk + done, so the UI
    // completes rather than hanging on a stream that never frames.
    let is_sse = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|ct| ct.contains("text/event-stream"))
        .unwrap_or(false);
    if !is_sse {
        let body = resp.text().await.unwrap_or_default();
        let answer = chat::cloud::whole_turn_answer(&body);
        let message_id = uuid::Uuid::new_v4().to_string();
        let _ = app.emit(
            "chat_text_delta",
            serde_json::json!({
                "conversationId": local_conversation_id,
                "messageId": message_id,
                "blockId": uuid::Uuid::new_v4().to_string(),
                "delta": answer,
            }),
        );
        let _ = app.emit(
            "chat_done",
            serde_json::json!({ "conversationId": local_conversation_id, "messageId": message_id }),
        );
        return Ok(CloudTurn { preflight: None, server_conversation_id: None });
    }

    // Buffer raw bytes and frame on the byte terminator, decoding only COMPLETE
    // frames — so a multibyte codepoint split across two network chunks (e.g. a
    // Norwegian å in a streamed delta) is never mangled by a mid-character decode.
    let mut byte_stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    let mut server_conversation_id: Option<String> = None;
    while let Some(chunk) = byte_stream.next().await {
        let bytes = chunk.map_err(|e| format!("Team chat stream error: {e}"))?;
        buf.extend_from_slice(&bytes);
        while let Some((idx, delim)) = chat::cloud::find_event_end(&buf) {
            let frame: Vec<u8> = buf.drain(..idx + delim).collect();
            let text = String::from_utf8_lossy(&frame[..idx]);
            let Some((event, data)) = chat::cloud::parse_sse_frame(&text) else { continue };
            let Ok(mut payload) = serde_json::from_str::<serde_json::Value>(&data) else { continue };
            if server_conversation_id.is_none() {
                if let Some(sid) = payload.get("conversationId").and_then(|c| c.as_str()) {
                    if !sid.is_empty() {
                        server_conversation_id = Some(sid.to_string());
                    }
                }
            }
            if let Some(obj) = payload.as_object_mut() {
                obj.insert("conversationId".into(), serde_json::json!(local_conversation_id));
            }
            if let Some(tauri_event) = chat::cloud::tauri_event_for(&event) {
                let _ = app.emit(tauri_event, payload);
            }
        }
    }
    Ok(CloudTurn { preflight: None, server_conversation_id })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessageDto {
    id: String,
    role: String,
    seq: i64,
    /// Raw opencode-v2 parts JSON. Passed through verbatim: the local path
    /// serialises its `Vec<chat::Part>` into this value; the workspace read-
    /// through passes PocketBase's stored parts straight through (already the
    /// frontend-shaped camelCase), so tool/citation parts render either way.
    parts: serde_json::Value,
    created_at: i64,
}

/// Reload a Note's conversation (empty if none exists yet). Drives history
/// restore when the Chat tab opens or after an app restart. For a workspace
/// tenant (issue #50) this is a read-through of the server-authoritative
/// conversation — messages live in the cloud, not the local `messages` table.
#[tauri::command]
pub async fn chat_history(
    app: AppHandle,
    note_id: String,
) -> Result<Vec<ChatMessageDto>, String> {
    let state: State<AppState> = app.state();
    // Chat follows the loaded context (issue #58): a loaded workspace reads its
    // server-authoritative conversation; Personal reads the local table.
    let ctx = {
        let conn = state.db.lock();
        ChatContext::load(&conn)
    };
    if let Some(workspace) = ctx.workspace() {
        // Resolve the workspace conversation's server id, then read its messages
        // through PocketBase (the source of truth for shared conversations).
        let remote_id = {
            let conn = state.db.lock();
            db::get_conversation(&conn, workspace, CHAT_SCOPE_NOTE, &note_id)
                .map_err(|e| e.to_string())?
                .and_then(|c| c.remote_id)
        };
        let Some(remote_id) = remote_id else { return Ok(Vec::new()) };
        let records = super::cloud::fetch_chat_messages(&state, &remote_id).await?;
        return Ok(records.iter().filter_map(map_remote_message).collect());
    }

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
            parts: serde_json::to_value(chat::parse_parts(&m.content)).unwrap_or_default(),
            created_at: m.created_at,
        })
        .collect())
}

/// Map one PocketBase `chat_messages` record (read-through) to the UI DTO. Its
/// `parts` are already the frontend-shaped opencode-v2 JSON, so they pass
/// through untouched; `created` (RFC3339 autodate) becomes epoch-ms.
fn map_remote_message(rec: &serde_json::Value) -> Option<ChatMessageDto> {
    let id = rec.get("id")?.as_str()?.to_string();
    let role = rec.get("role").and_then(|r| r.as_str()).unwrap_or("assistant").to_string();
    let seq = rec.get("seq").and_then(|s| s.as_i64()).unwrap_or(0);
    let parts = rec.get("parts").cloned().unwrap_or_else(|| serde_json::json!([]));
    let created_at = rec
        .get("created")
        .and_then(|c| c.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.timestamp_millis())
        .unwrap_or(0);
    Some(ChatMessageDto { id, role, seq, parts, created_at })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Insert a Note with an explicit folder assignment, for scope tests.
    fn note_in_folder(conn: &rusqlite::Connection, folder: Option<&str>) -> db::Note {
        let note = db::create_note(conn, "en", "meeting", "").unwrap();
        if let Some(f) = folder {
            db::move_note(conn, &note.id, Some(f)).unwrap();
        }
        db::get_note(conn, &note.id).unwrap()
    }

    #[test]
    fn resolve_scope_maps_valid_breadths_and_errors_on_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let conn = db::open(&dir.path().join("scope.sqlite")).unwrap();
        let folder = db::create_folder(&conn, "Team", "").unwrap();
        let with_folder = note_in_folder(&conn, Some(&folder.id));
        let no_folder = note_in_folder(&conn, None);

        assert!(matches!(resolve_scope("all", &no_folder).unwrap(), ToolScope::All));
        assert!(matches!(resolve_scope("note", &no_folder).unwrap(), ToolScope::Note(_)));
        match resolve_scope("folder", &with_folder).unwrap() {
            ToolScope::Folder(id) => assert_eq!(id, folder.id),
            other => panic!("expected Folder scope, got {other:?}"),
        }
        // Unknown breadth is loud (issue #58) — the old `_ => Note` clamp is gone.
        assert!(resolve_scope("everything", &no_folder).is_err());
        // Folder breadth on a folder-less note errors rather than clamping.
        assert!(resolve_scope("folder", &no_folder).is_err());
    }

    #[test]
    fn heal_and_read_breadth_self_heals_a_stale_folder_breadth_to_note() {
        let dir = tempfile::tempdir().unwrap();
        let conn = db::open(&dir.path().join("heal.sqlite")).unwrap();
        let note = note_in_folder(&conn, None); // no folder
        let conv =
            db::get_or_create_conversation(&conn, CHAT_TENANT_PERSONAL, CHAT_SCOPE_NOTE, &note.id)
                .unwrap();
        // Simulate a breadth stored while the note still had a folder, then lost it.
        db::set_conversation_breadth(&conn, &conv.id, "folder").unwrap();

        let effective = heal_and_read_breadth(&conn, &conv.id, "folder", &note).unwrap();
        assert_eq!(effective, "note", "stale folder breadth heals to note");
        // The heal is persisted, so the chip and the request never diverge.
        let reloaded =
            db::get_conversation(&conn, CHAT_TENANT_PERSONAL, CHAT_SCOPE_NOTE, &note.id)
                .unwrap()
                .unwrap();
        assert_eq!(reloaded.breadth, "note");
    }

    #[test]
    fn heal_and_read_breadth_keeps_a_valid_breadth_and_errors_on_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let conn = db::open(&dir.path().join("eff.sqlite")).unwrap();
        let folder = db::create_folder(&conn, "Team", "").unwrap();
        let note = note_in_folder(&conn, Some(&folder.id));
        let conv =
            db::get_or_create_conversation(&conn, CHAT_TENANT_PERSONAL, CHAT_SCOPE_NOTE, &note.id)
                .unwrap();
        assert_eq!(
            heal_and_read_breadth(&conn, &conv.id, "folder", &note).unwrap(),
            "folder",
            "folder breadth survives when the folder is present"
        );
        assert_eq!(
            heal_and_read_breadth(&conn, &conv.id, "all", &note).unwrap(),
            "all"
        );
        // A corrupt stored value errors, never silently clamps to note.
        assert!(heal_and_read_breadth(&conn, &conv.id, "bogus", &note).is_err());
    }

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

    /// Cross-repo round-trip of a real workspace `chat_send` against a live
    /// humla-cloud stack (issue #50, AC6), matching the env-gated shape of the
    /// cloud-sync roundtrips: skipped unless the HUMLA_TEST_* vars point at a
    /// booted server (PocketBase + chat-service + an LLM key). It authenticates,
    /// POSTs `/api/chat`, and pumps the SSE stream through the SAME pure helpers
    /// `stream_cloud_turn` uses — asserting the turn reaches a terminal `done`
    /// and carries a conversation id. Plain `cargo test` (no env) skips it.
    #[tokio::test]
    async fn cloud_chat_roundtrip() {
        use futures_util::StreamExt;
        let (Ok(pb), Ok(chat), Ok(email), Ok(pass), Ok(ws)) = (
            std::env::var("HUMLA_TEST_PB_URL"),
            std::env::var("HUMLA_TEST_CHAT_URL"),
            std::env::var("HUMLA_TEST_EMAIL"),
            std::env::var("HUMLA_TEST_PASSWORD"),
            std::env::var("HUMLA_TEST_WORKSPACE"),
        ) else {
            eprintln!("cloud_chat_roundtrip: skipped (set HUMLA_TEST_PB_URL/CHAT_URL/EMAIL/PASSWORD/WORKSPACE)");
            return;
        };
        let client = reqwest::Client::new();

        // 1. Authenticate against PocketBase for a real user token.
        let auth: serde_json::Value = client
            .post(format!("{pb}/api/collections/users/auth-with-password"))
            .json(&serde_json::json!({ "identity": email, "password": pass }))
            .send()
            .await
            .expect("auth send")
            .json()
            .await
            .expect("auth json");
        let token = auth.get("token").and_then(|t| t.as_str()).expect("token");

        // 2. POST a workspace turn, built by the same request assembler the app uses.
        let body = chat::cloud::build_cloud_request(
            None,
            &ws,
            "In one sentence, what is this workspace about?",
            Some("roundtrip test"),
            "all",
            "roundtrip-anchor",
            None,
        )
        .expect("all-breadth request builds");
        let resp = client
            .post(format!("{}/api/chat", chat.trim_end_matches('/')))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .expect("chat send");
        assert!(resp.status().is_success(), "chat endpoint returned {}", resp.status());

        // 3. Pump the SSE stream with the production helpers; assert we reach `done`.
        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        let mut conversation_id: Option<String> = None;
        let mut saw_done = false;
        while let Some(chunk) = stream.next().await {
            buf.extend_from_slice(&chunk.expect("chunk"));
            while let Some((idx, delim)) = chat::cloud::find_event_end(&buf) {
                let frame: Vec<u8> = buf.drain(..idx + delim).collect();
                let text = String::from_utf8_lossy(&frame[..idx]);
                let Some((event, data)) = chat::cloud::parse_sse_frame(&text) else { continue };
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) {
                    if let Some(id) = v.get("conversationId").and_then(|c| c.as_str()) {
                        conversation_id.get_or_insert_with(|| id.to_string());
                    }
                }
                if event == "done" {
                    saw_done = true;
                }
                assert_ne!(event, "error", "server streamed an error: {data}");
            }
        }
        assert!(saw_done, "the turn never reached a terminal `done` event");
        assert!(conversation_id.is_some(), "no conversation id was streamed");
    }
}
