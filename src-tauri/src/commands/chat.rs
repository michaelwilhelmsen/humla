//! Chat commands (issue #46). `chat_send` runs a single-pass, Note-grounded
//! completion: it resolves the configured chat provider, grounds the prompt in
//! the current Note's content (as reference material, never the system prompt),
//! streams the answer to the frontend, and persists the turn. `chat_history`
//! reloads a Note's conversation after restart. The heavy lifting (prompt
//! assembly, budget, streaming orchestration) lives Tauri-free in `crate::chat`.

use super::{DEFAULT_LOCAL_LLM_BASE_URL, DEFAULT_SUMMARY_MODEL};
use crate::chat::{self, ChatCtx, ChatEvent, Citation, ToolScope};
use crate::db::{self, ChatTarget, CHAT_TENANT_PERSONAL};
use crate::embed::{self, EmbeddingAdapter, OLLAMA_EMBED_MODEL, OPENAI_EMBED_MODEL};
use crate::openai;
use crate::AppState;

/// A user-set display name for chat's first-person referent (#103). Empty/unset
/// falls back to the macOS account name — see [`asker_name`].
pub(crate) const SETTING_DISPLAY_NAME: &str = "user_display_name";
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

/// How many notes a rebuild would repair (#122), so the UI can offer the action only
/// when it does something — and say what it will do.
///
/// A blind "rebuild search index" button is a bad bargain: it is slow, it spends the
/// user's embedding key, and it gives no way to tell whether any of that was needed.
/// With a count, the row can stay quiet on a current library and state the work on a
/// stale one.
#[tauri::command]
pub fn chat_stale_note_count(state: State<AppState>) -> Result<usize, String> {
    let conn = state.db.lock();
    db::notes_with_stale_chunks(&conn).map_err(super::err)
}

/// Rebuild the retrieval index for the WHOLE library (issue #104).
///
/// The only way to repair an existing library after a chunking-shape change. The
/// startup backfill is keyed on sentinels for never-indexed notes, so it cannot see
/// a note whose chunks are present but built the old way — and without this, an
/// archive would stay hard-split until each note happened to be opened, which is
/// precisely the wrong outcome: old meetings are what a "briefing on X" query needs.
///
/// Deliberately user-triggered rather than automatic. Re-chunking changes every
/// chunk's `text_hash`, so the embedding cache misses and the library re-embeds on
/// the user's own API key — cents, but not to be spent unasked.
///
/// Returns the number of notes rebuilt. Embedding is kicked off afterwards and
/// reuses `embed_backfill`, which already finds exactly the chunks missing vectors
/// under the current model, so the two stay one mechanism rather than two.
///
/// `async` + `spawn_blocking` is load-bearing, not decoration: Tauri runs a
/// synchronous command on the main thread, so walking a whole library there would
/// freeze the webview for the duration — the "Rebuilding…" state could never even
/// paint, which is the one piece of UI that makes a slow, key-spending action
/// tolerable. Same shape as the local-Whisper path for the same reason.
#[tauri::command]
pub async fn chat_rebuild_index(app: AppHandle) -> Result<usize, String> {
    // Clone the Arc out and drop the `State` borrow before any await: a
    // `parking_lot` guard held across one would make this future non-Send, and all
    // the locking below happens inside the blocking closure regardless.
    let db = {
        let state: State<AppState> = app.state();
        state.db.clone()
    };
    let rebuilt = tauri::async_runtime::spawn_blocking(move || {
        let ids = {
            let conn = db.lock();
            db::live_note_ids(&conn).map_err(super::err)?
        };
        // One lock acquisition per note, not one around the loop: a large library
        // would otherwise hold the connection long enough to stall every other
        // command that needs the database.
        for id in &ids {
            let conn = db.lock();
            reindex_note_content(&conn, id);
        }
        Ok::<usize, String>(ids.len())
    })
    .await
    .map_err(|e| format!("rebuild task failed: {e}"))??;
    eprintln!("[chat] rebuilt retrieval chunks for {rebuilt} note(s)");
    tauri::async_runtime::spawn(embed_backfill(app));
    Ok(rebuilt)
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

/// The library-wide counterpart: a global conversation is all-notes-only in v1
/// (#82), so "note" and "folder" are meaningless — it has no anchor to mean them
/// by. Heal rather than error, matching the folder-heal above: a corrupt row must
/// not be able to break the user's chat.
///
/// A separate function rather than an `Option<&Note>` arm on the one above,
/// because `None` there would hide a whole policy branch behind a nullable
/// parameter — a reader couldn't tell "library-wide" from "note not loaded".
fn heal_and_read_global_breadth(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    stored: &str,
) -> Result<String, String> {
    if chat::validate_breadth(stored)? != "all" {
        db::set_conversation_breadth(conn, conversation_id, "all").map_err(|e| e.to_string())?;
    }
    Ok("all".into())
}

/// Effective breadth for whichever target a turn is on — the one place the two
/// heal policies are chosen between.
fn effective_breadth(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    stored: &str,
    note: Option<&db::Note>,
) -> Result<String, String> {
    match note {
        Some(note) => heal_and_read_breadth(conn, conversation_id, stored, note),
        None => heal_and_read_global_breadth(conn, conversation_id, stored),
    }
}

/// Load a turn's anchor note, or None for a library-wide turn. There is
/// deliberately no fallback note (#82) — a global turn runs on retrieval alone.
fn anchor_note(
    conn: &rusqlite::Connection,
    target: &ChatTarget,
) -> Result<Option<db::Note>, String> {
    match target.note_id() {
        Some(id) => db::get_note(conn, id).map(Some).map_err(|e| e.to_string()),
        None => Ok(None),
    }
}

/// The reference block for a turn, plus the anchor's plain-text body when there is
/// one (the caller reindexes it). A library-wide turn gets NO grounding at all —
/// `assemble_prompt` skips an empty block, and nothing was truncated because
/// nothing was injected.
fn turn_grounding(note: Option<&db::Note>) -> (chat::Grounding, Option<String>) {
    match note {
        Some(note) => {
            let body_text = crate::html_text::html_to_text(&note.body);
            let grounding =
                chat::build_grounding(&body_text, &note.transcript, &note.summary);
            (grounding, Some(body_text))
        }
        None => (chat::Grounding { text: String::new(), truncated: false }, None),
    }
}

/// Resolve an (already-effective) breadth into a server-enforced `ToolScope`.
/// "folder" resolves to the anchor Note's folder. Unrecognised breadths and a
/// folder breadth without a folder are loud errors (issue #58) rather than a
/// silent clamp to Note — in practice `heal_and_read_breadth` heals the
/// folder-less case upstream, so those arms are belt-and-suspenders.
///
/// `note` is `None` for a library-wide turn (#93), which resolves straight to
/// `All` without reaching for an anchor — the retrieval tools carry it alone.
fn resolve_scope(breadth: &str, note: Option<&db::Note>) -> Result<ToolScope, String> {
    match chat::validate_breadth(breadth)? {
        "all" => Ok(ToolScope::All),
        "folder" => match note.and_then(|n| n.folder_id.as_deref()) {
            Some(folder_id) if !folder_id.is_empty() => Ok(ToolScope::Folder(folder_id.to_string())),
            _ => Err("This note isn't in a folder, so \"Folder\" scope isn't available.".into()),
        },
        // A "note" breadth with no note can only be a corrupt global row.
        // `heal_and_read_global_breadth` rewrites those to "all" before a turn ever
        // reaches here, so this is unreachable in practice — and it errors rather
        // than widening to the whole library, which would silently search
        // everything on the strength of a bad row. Loud, like every other arm.
        _ => match note {
            Some(n) => Ok(ToolScope::Note(n.id.clone())),
            None => Err(
                "This conversation has no anchor note, so only \"All notes\" scope applies.".into(),
            ),
        },
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
    /// Machine reason code (issue #76) so the client can render role-aware copy
    /// for the BYOK error taxonomy (byok_key_invalid / byok_provider_quota /
    /// byok_key_unavailable / chat_not_activated). Empty when unknown.
    #[serde(default)]
    reason: String,
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

// ── Chat sessions (issue #61) ───────────────────────────────────────────────
// Multiple conversations ("sessions") per Note. The active session is the most-
// recently-updated one; an explicit command creates a fresh one. Sessions can be
// deleted and renamed since #109 (`chat_delete_conversation` /
// `chat_rename_conversation`).

/// The longest title we store on a rename (issue #109).
///
/// Pinned to the cap in `chat_sessions.pb.js`'s create + rename routes: if this
/// were larger, a workspace rename would silently come back shortened, and the
/// local row and the server record would disagree about the thread's name. Much
/// longer than [`chat::TITLE_MAX_CHARS`] (40) on purpose — that bounds what we
/// *derive* from a first message, where this bounds what a user may deliberately
/// type.
const TITLE_MAX_STORED_CHARS: usize = 300;

/// One row in the session list. `id` is always the LOCAL conversation id (for a
/// workspace session that's the handle row, its `remote_id` mapping to the
/// server record), so `chat_history` / `chat_send` / breadth all key on the
/// same id across tenants. `title` is resolved (stored title, else a date
/// fallback) so a not-yet-titled session still shows something.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationMeta {
    id: String,
    title: String,
    breadth: String,
    /// The pinned authorship filter's user id, or "" for none (#103). The chip
    /// resolves it to a name against the workspace roster.
    owner_filter: String,
    updated_at: i64,
    message_count: i64,
}

/// `chat_history` result (issue #61): the messages plus the conversation they
/// were resolved to, so the panel learns which session it's on. `conversation_id`
/// is None when the Note has no session yet (opening the tab creates nothing).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatHistoryResult {
    conversation_id: Option<String>,
    messages: Vec<ChatMessageDto>,
}

/// A conversation's display title: its stored title, else a derived date label
/// (an untitled session — e.g. one just created — still reads sensibly).
fn resolved_title(c: &db::Conversation) -> String {
    match c.title.as_deref() {
        Some(t) if !t.trim().is_empty() => t.to_string(),
        _ => chat::derive_title(None, c.created_at),
    }
}

/// Whether a conversation has no stored title yet — the first user message
/// should set it (issue #61).
fn resolved_title_is_unset(c: &db::Conversation) -> bool {
    c.title.as_deref().map_or(true, |t| t.trim().is_empty())
}

/// Build a session-list entry for a local (personal or workspace-handle) row.
fn conversation_meta(
    conn: &rusqlite::Connection,
    c: &db::Conversation,
) -> Result<ConversationMeta, String> {
    let message_count =
        db::conversation_message_count(conn, &c.id).map_err(|e| e.to_string())?;
    Ok(ConversationMeta {
        id: c.id.clone(),
        title: resolved_title(c),
        breadth: c.breadth.clone(),
        owner_filter: c.owner_filter.clone(),
        updated_at: c.updated_at,
        message_count,
    })
}

/// The breadth a new session inherits (issue #61): the target's most-recent
/// session's breadth, or the target's own default for its first-ever session
/// ("note" for a Note, "all" for the library — a global thread has no anchor to
/// narrow to).
fn inherited_breadth(conn: &rusqlite::Connection, tenant: &str, target: &ChatTarget) -> String {
    db::latest_conversation(conn, tenant, target.scope(), target.scope_id())
        .ok()
        .flatten()
        .map(|c| c.breadth)
        .unwrap_or_else(|| chat::default_breadth(target).to_string())
}

/// Resolve the session a command targets WITHOUT creating one (None when the
/// Note has no session yet). `explicit` targets a specific session; otherwise
/// the most-recently-updated one is the active session.
///
/// An explicit id is validated to belong to the expected (tenant, scope,
/// scope_id) — a mismatched id is a clean error, never a silent cross-scope
/// operation (which would, e.g., heal breadth against the wrong Note, or splice
/// a note thread into the library list). A not-found explicit id is treated as
/// "no session" (None) so the caller falls back to the active one.
fn resolve_existing(
    conn: &rusqlite::Connection,
    tenant: &str,
    target: &ChatTarget,
    explicit: Option<&str>,
) -> Result<Option<db::Conversation>, String> {
    if let Some(id) = explicit {
        let Some(c) = db::get_conversation_by_id(conn, id).map_err(|e| e.to_string())? else {
            return Ok(None);
        };
        if c.tenant != tenant || c.scope != target.scope() || c.scope_id != target.scope_id() {
            return Err(match target {
                ChatTarget::Note(_) => "That chat session doesn't belong to this note.".into(),
                ChatTarget::Global => "That chat session isn't a library-wide one.".to_string(),
            });
        }
        return Ok(Some(c));
    }
    db::latest_conversation(conn, tenant, target.scope(), target.scope_id())
        .map_err(|e| e.to_string())
}

/// Whether opening this target's pane RESUMES its most-recent conversation, or
/// starts an unsaved draft (issue #120).
///
/// A library-wide pane drafts. `/chat` is a front door, and asking a new question
/// is overwhelmingly what it is for — reopening a finished thread there hid the
/// prompt cards, named the bar after an old question, and made "ask something"
/// take a second deliberate action. Returning to a specific past thread is the
/// rarer, more intentional act, and the sidebar list already exists for it.
///
/// A Note's pane resumes, deliberately diverging: a note is an anchor, and coming
/// back to it to continue the same line of thinking is a plausible default in a
/// way it isn't on a library-wide route. Recorded here, in one predicate, so the
/// divergence stays a decision rather than becoming an accident — every caller
/// that cares reads it from this function.
fn resumes_on_open(target: &ChatTarget) -> bool {
    match target {
        ChatTarget::Note(_) => true,
        ChatTarget::Global => false,
    }
}

/// The conversation-list filter implied by the resume rule (issue #120).
///
/// A target that drafts can never return to an empty thread, so an empty row in
/// its list is residue — from an older client, or a breadth chosen then abandoned
/// — and hiding it is right. A target that resumes has the opposite relationship
/// to the same row: it IS the draft being resumed, so hiding it would hide what
/// the pane is showing.
fn list_filter_for(target: &ChatTarget) -> db::ListFilter {
    if resumes_on_open(target) {
        db::ListFilter::All
    } else {
        db::ListFilter::WithMessages
    }
}

/// Resolve the session a READ command targets, honouring the resume rule (#120).
///
/// Shared by `chat_history`, `chat_get_breadth` and `chat_get_owner_filter` so all
/// three agree about what a bare request means. An explicit id always wins — a
/// thread picked from the sidebar must open — but a bare request on a drafting
/// target resolves to nothing rather than to whatever was newest.
fn resolve_for_read(
    conn: &rusqlite::Connection,
    tenant: &str,
    target: &ChatTarget,
    explicit: Option<&str>,
) -> Result<Option<db::Conversation>, String> {
    if explicit.is_none() && !resumes_on_open(target) {
        return Ok(None);
    }
    resolve_existing(conn, tenant, target, explicit)
}

/// Resolve the session a SETTINGS write targets, or `None` when there is nothing
/// to write to yet (issue #120).
///
/// A Note still lazily creates its first session, which is what #61 wanted: that
/// pane resumes, so a breadth chosen before the first turn has to be stored
/// somewhere it will be found again. A drafting target returns `None` instead —
/// the pane holds the value, and `DraftSettings` carries it into the turn that
/// makes the row.
fn resolve_for_write(
    conn: &rusqlite::Connection,
    tenant: &str,
    target: &ChatTarget,
    explicit: Option<&str>,
) -> Result<Option<db::Conversation>, String> {
    if explicit.is_none() && !resumes_on_open(target) {
        return Ok(None);
    }
    Ok(Some(resolve_or_create(conn, tenant, target, explicit, DraftSettings::default())?))
}

/// What a draft had chosen before it had a row to store it on (issue #120).
///
/// A library-wide pane holds its breadth and authorship pin locally until the
/// first turn, so those values arrive with the send that materialises the row.
/// This replaces the old lazy-create (#61, #103): a row that existed only to hold
/// a pre-turn setting was invisible in the list yet still resolved to by the next
/// send, so the pane could show "no pin" while the stored row narrowed the turn
/// anyway — exactly the lie #103 set out to prevent. Now the row appears once,
/// already carrying what the chips showed.
#[derive(Debug, Default, Clone)]
pub struct DraftSettings {
    pub breadth: Option<String>,
    pub owner_filter: Option<String>,
}

/// Check a draft's pending settings against the same rules their dedicated
/// setters enforce (issue #120).
///
/// `chat_set_breadth` rejects a value outside the vocabulary and a non-`all`
/// breadth on a library-wide target; `chat_set_owner_filter` rejects a pin in
/// Personal, where every note is the caller's own already. `DraftSettings` writes
/// the same two columns, so it answers to the same rules — a chip that can't
/// produce an illegal value today is not a reason for the command to accept one.
fn validated_draft(
    state: &State<'_, AppState>,
    target: &ChatTarget,
    breadth: Option<String>,
    owner_filter: Option<String>,
) -> Result<DraftSettings, String> {
    if let Some(b) = breadth.as_deref() {
        chat::validate_breadth(b)?;
        if matches!(target, ChatTarget::Global) && b != "all" {
            return Err("A library-wide chat always searches all notes.".into());
        }
    }
    // An empty string is "no pin", not a pin on nobody — normalise it away rather
    // than storing it, so the created row reads the same as one that never had one.
    let owner_filter = owner_filter.filter(|o| !o.trim().is_empty());
    if owner_filter.is_some() {
        let personal = {
            let conn = state.db.lock();
            matches!(ChatContext::load(&conn), ChatContext::Personal)
        };
        if personal {
            return Err(
                "Filtering by author needs a workspace — in Personal every note is yours.".into()
            );
        }
    }
    Ok(DraftSettings { breadth, owner_filter })
}

/// Resolve the active session, creating one when none applies.
///
/// Creation happens either because the target has no session yet, or — on a
/// drafting target with no explicit id — because a bare send always starts its own
/// conversation. That second case is load-bearing: resolving a draft's first turn
/// to "most recent" would file the message into a thread the user believed they
/// had left, which is worse than any list bug.
fn resolve_or_create(
    conn: &rusqlite::Connection,
    tenant: &str,
    target: &ChatTarget,
    explicit: Option<&str>,
    draft: DraftSettings,
) -> Result<db::Conversation, String> {
    if explicit.is_some() || resumes_on_open(target) {
        if let Some(c) = resolve_existing(conn, tenant, target, explicit)? {
            return Ok(c);
        }
    }
    // A draft's own choice wins over inheritance; absent one, "+"-style
    // inheritance is unchanged.
    let breadth = draft.breadth.unwrap_or_else(|| inherited_breadth(conn, tenant, target));
    let conv = db::create_conversation(conn, tenant, target.scope(), target.scope_id(), &breadth)
        .map_err(|e| e.to_string())?;
    match draft.owner_filter.as_deref().map(str::trim) {
        // Blank is "no pin", checked here as well as in `validated_draft`: this is
        // the layer that writes, and it shouldn't depend on a validator having run
        // first. A stored "   " would be a pin on nobody — a third state retrieval
        // would then need an opinion about.
        //
        // Written as a second statement rather than a wider INSERT: the pin is
        // rare, and `create_conversation` is on every path that makes a row.
        Some(owner) if !owner.is_empty() => {
            db::set_conversation_owner_filter(conn, &conv.id, Some(owner))
                .map_err(|e| e.to_string())?;
            db::get_conversation_by_id(conn, &conv.id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "conversation vanished after insert".to_string())
        }
        _ => Ok(conv),
    }
}

/// Resolve the "new session" for a target in the personal scope (issue #61),
/// honoring the no-op guard: if the most-recent session has no messages, reuse
/// it instead of piling up empty sessions; otherwise create a fresh one that
/// inherits breadth from the most-recent session.
fn new_personal_conversation(
    conn: &rusqlite::Connection,
    target: &ChatTarget,
) -> Result<db::Conversation, String> {
    if let Some(latest) =
        db::latest_conversation(conn, CHAT_TENANT_PERSONAL, target.scope(), target.scope_id())
            .map_err(|e| e.to_string())?
    {
        if db::conversation_message_count(conn, &latest.id).map_err(|e| e.to_string())? == 0 {
            return Ok(latest);
        }
    }
    let breadth = inherited_breadth(conn, CHAT_TENANT_PERSONAL, target);
    db::create_conversation(
        conn,
        CHAT_TENANT_PERSONAL,
        target.scope(),
        target.scope_id(),
        &breadth,
    )
    .map_err(|e| e.to_string())
}

/// List a Note's chat sessions, most-recently-updated first (issue #61).
/// Personal reads local SQLite; a workspace reads the server-authoritative list
/// (see [`list_conversations_cloud`]). Opening/listing creates nothing.
#[tauri::command]
pub async fn chat_list_conversations(
    app: AppHandle,
    note_id: Option<String>,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<Vec<ConversationMeta>, String> {
    let target = ChatTarget::from_note_id(note_id)?;
    let page = conversation_page(limit, offset);
    let state: State<AppState> = app.state();
    let ctx = {
        let conn = state.db.lock();
        ChatContext::load(&conn)
    };
    match ctx.workspace() {
        None => {
            let conn = state.db.lock();
            let convs = db::list_conversations(
                &conn,
                CHAT_TENANT_PERSONAL,
                target.scope(),
                target.scope_id(),
                page,
                list_filter_for(&target),
            )
            .map_err(|e| e.to_string())?;
            convs.iter().map(|c| conversation_meta(&conn, c)).collect()
        }
        Some(ws) => {
            let all = list_conversations_cloud(&state, ws, &target).await?;
            Ok(apply_page(all, page))
        }
    }
}

/// The requested window, or None for "everything" (issue #95).
///
/// A caller that omits `limit` gets the whole list — the Note header's history
/// popover has always shown all of a note's conversations and there is no reason
/// to change that. `offset` without `limit` is meaningless and ignored rather
/// than errored: it can only come from our own frontend, and a dropped window is
/// a longer list, never a wrong one.
fn conversation_page(limit: Option<u32>, offset: Option<u32>) -> Option<db::Page> {
    limit.map(|limit| db::Page { limit: limit.into(), offset: offset.unwrap_or(0).into() })
}

/// Apply a window to an already-materialised list.
///
/// The Personal path pages in SQL; a workspace can't yet, because the server's
/// list route takes no paging parameters and hard-caps at 200 rows
/// (`chat_sessions.pb.js`). So the workspace list is fetched whole and windowed
/// here: the UI stays lazy and the DOM stays small, but the request doesn't
/// shrink and a workspace past 200 conversations would silently lose the tail.
/// Real server paging is tracked separately — see humla-cloud#33.
fn apply_page<T>(all: Vec<T>, page: Option<db::Page>) -> Vec<T> {
    let Some(page) = page else { return all };
    all.into_iter().skip(page.offset as usize).take(page.limit as usize).collect()
}

/// Create a fresh chat session for a target (issue #61). Personal creates a local
/// row (with the no-op guard reusing an empty most-recent session); a workspace
/// delegates to the server (see [`new_conversation_cloud`]).
#[tauri::command]
pub async fn chat_new_conversation(
    app: AppHandle,
    note_id: Option<String>,
) -> Result<ConversationMeta, String> {
    let target = ChatTarget::from_note_id(note_id)?;
    let state: State<AppState> = app.state();
    let ctx = {
        let conn = state.db.lock();
        ChatContext::load(&conn)
    };
    match ctx.workspace() {
        None => {
            let conn = state.db.lock();
            let conv = new_personal_conversation(&conn, &target)?;
            conversation_meta(&conn, &conv)
        }
        Some(ws) => new_conversation_cloud(&state, ws, &target).await,
    }
}

/// Delete a conversation and its messages (issue #109).
///
/// The id is explicit and required — there is no "delete the active one" fallback.
/// Every other command here resolves an absent id to the most-recent session,
/// which is a helpful default for reading and an unacceptable one for destroying:
/// a caller that forgot to pass an id would silently take out whatever thread
/// happened to be newest.
///
/// **Both sides or neither.** A workspace conversation is server-authoritative,
/// but the local handle is what `list_conversations_cloud`'s pre-#19 union falls
/// back to — so dropping only the remote record leaves a handle that the very
/// next list puts back on screen. The remote delete therefore goes FIRST and its
/// failure aborts: a local row with a live server record is a stale cache the
/// next list corrects, where a deleted server record with a surviving handle is a
/// row that reappears with no way to remove it.
#[tauri::command]
pub async fn chat_delete_conversation(
    app: AppHandle,
    note_id: Option<String>,
    conversation_id: String,
) -> Result<(), String> {
    let target = ChatTarget::from_note_id(note_id)?;
    let state: State<AppState> = app.state();
    let ctx = {
        let conn = state.db.lock();
        ChatContext::load(&conn)
    };

    // Resolve within the caller's tenant + target first, so a stray id can't
    // reach another note's or another tenant's thread.
    let conv = {
        let conn = state.db.lock();
        resolve_existing(&conn, ctx.tenant(), &target, Some(&conversation_id))?
    };
    let Some(conv) = conv else {
        // Nothing local to remove. Idempotent by intent: the UI drops the row
        // optimistically, so a retry after a partial failure must still succeed.
        return Ok(());
    };

    // No remote id → the thread never reached the server (created, never sent to).
    // The local handle is the whole of it, so dropping it below is complete.
    if let (Some(workspace), Some(remote_id)) = (ctx.workspace(), conv.remote_id.as_deref()) {
        let workspace = workspace.to_string();
        super::cloud::cloud_delete_json(
            &state,
            &format!("/api/humla/chat/conversations/{remote_id}"),
            &[("workspace_id", workspace.as_str())],
        )
        .await
        .map_err(|e| e.message(chat::cloud::cloud_chat_error_message))?;
    }

    let conn = state.db.lock();
    db::delete_conversation(&conn, &conv.id).map_err(|e| e.to_string())
}

/// Rename a conversation (issue #109) — the user's override of the title derived
/// from the first turn.
///
/// Remote first, for the same reason as the delete above: a local title the
/// server never accepted would be silently reverted the next time the workspace
/// list is read, and a rename that looks applied but isn't is worse than one that
/// reports failure. Rejects an all-whitespace title, because an empty title is
/// how both sides spell "never titled" — storing one would re-arm the send path's
/// derivation and let the next turn overwrite the user's choice.
#[tauri::command]
pub async fn chat_rename_conversation(
    app: AppHandle,
    note_id: Option<String>,
    conversation_id: String,
    title: String,
) -> Result<ConversationMeta, String> {
    let target = ChatTarget::from_note_id(note_id)?;
    let state: State<AppState> = app.state();
    let ctx = {
        let conn = state.db.lock();
        ChatContext::load(&conn)
    };

    let trimmed = title.trim();
    if trimmed.is_empty() {
        return Err("A conversation needs a name.".into());
    }
    // Same cap the server applies, so a title written here can't be silently
    // truncated to a different value there. Sliced on a char boundary.
    let capped: String = trimmed.chars().take(TITLE_MAX_STORED_CHARS).collect();

    let conv = {
        let conn = state.db.lock();
        resolve_existing(&conn, ctx.tenant(), &target, Some(&conversation_id))?
    };
    let Some(conv) = conv else {
        return Err("That conversation no longer exists.".into());
    };

    // No remote id yet: the local title rides up with the first turn's persist,
    // which carries the conversation's title alongside its messages.
    if let (Some(workspace), Some(remote_id)) = (ctx.workspace(), conv.remote_id.as_deref()) {
        let body = serde_json::json!({ "workspace_id": workspace, "title": capped });
        super::cloud::cloud_patch_json(
            &state,
            &format!("/api/humla/chat/conversations/{remote_id}"),
            &body,
        )
        .await
        .map_err(|e| e.message(chat::cloud::cloud_chat_error_message))?;
    }

    let conn = state.db.lock();
    db::rename_conversation(&conn, &conv.id, &capped).map_err(|e| e.to_string())?;
    let updated = db::get_conversation_by_id(&conn, &conv.id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "That conversation no longer exists.".to_string())?;
    conversation_meta(&conn, &updated)
}

/// Persist the Scope chip's breadth on a conversation (issue #58) — the single
/// source of truth for retrieval breadth. Validates the value loudly (garbage →
/// Err), resolves the target session (`conversation_id` when the panel has one,
/// else the active/most-recent session, lazily creating the Note's first
/// session so a breadth chosen before the first turn survives — issue #61), and
/// writes the column.
#[tauri::command]
pub fn chat_set_breadth(
    app: AppHandle,
    note_id: Option<String>,
    conversation_id: Option<String>,
    breadth: String,
) -> Result<(), String> {
    chat::validate_breadth(&breadth)?;
    let target = ChatTarget::from_note_id(note_id)?;
    if matches!(target, ChatTarget::Global) && breadth != "all" {
        // #82 fixed the library surface at all-notes-only for v1, so there is no
        // picker to send this — reject rather than store a breadth that
        // `heal_and_read_breadth` would immediately undo.
        return Err("A library-wide chat always searches all notes.".into());
    }
    let state: State<AppState> = app.state();
    let conn = state.db.lock();
    let ctx = ChatContext::load(&conn);
    // A draft has nowhere to store this yet, and must not grow a row to hold it
    // (#120): the pane keeps the value and the first turn carries it in via
    // `DraftSettings`. Writing here would create a row the list hides but the next
    // send resolves to — the stale-setting trap this replaced.
    let Some(conversation) = resolve_for_write(&conn, ctx.tenant(), &target, conversation_id.as_deref())?
    else {
        return Ok(());
    };
    db::set_conversation_breadth(&conn, &conversation.id, &breadth).map_err(|e| e.to_string())
}

/// Pin (or clear) the conversation's authorship filter (#103) — the user whose
/// notes it retrieves from.
///
/// A user id, not a flag, because a workspace's conversation list is visible to
/// every member: a boolean would mean different notes to different readers of the
/// same thread. Storing the person keeps one meaning per conversation.
///
/// `owner` is `None` to clear. The UI only ever offers the caller's own id, and
/// only when the filter is off or already pinned to them — but that gate is not
/// enforced here, matching `chat_set_breadth`, which likewise lets any member
/// change a shared conversation's retrieval settings.
#[tauri::command]
pub fn chat_set_owner_filter(
    app: AppHandle,
    note_id: Option<String>,
    conversation_id: Option<String>,
    owner: Option<String>,
) -> Result<(), String> {
    let target = ChatTarget::from_note_id(note_id)?;
    let owner = owner.map(|o| o.trim().to_string()).filter(|o| !o.is_empty());
    let state: State<AppState> = app.state();
    let conn = state.db.lock();
    let ctx = ChatContext::load(&conn);
    if matches!(ctx, ChatContext::Personal) && owner.is_some() {
        // In Personal every note is the user's own, so a filter could only ever be
        // the identity function or a mistake. The control isn't rendered there;
        // reject rather than store a pin nothing would honour.
        return Err("Filtering by author needs a workspace — in Personal every note is yours.".into());
    }
    // Same as breadth: a draft carries its pin in the pane until the first turn
    // creates the row (#120). Especially important here — an invisible row holding
    // a pin is what let the chip read "off" while the turn narrowed anyway.
    let Some(conversation) = resolve_for_write(&conn, ctx.tenant(), &target, conversation_id.as_deref())?
    else {
        return Ok(());
    };
    db::set_conversation_owner_filter(&conn, &conversation.id, owner.as_deref())
        .map_err(|e| e.to_string())
}

/// Read the persisted breadth for a conversation so the Scope chip initialises
/// from the backend in one round trip (issue #58). Resolves `conversation_id`
/// when the panel has one, else the active/most-recent session; when the Note
/// has no session yet it returns the would-be-inherited default (issue #61) so
/// the chip shows something sane WITHOUT creating a row. NOTE: this read may
/// PERSIST a heal — a stale "folder" breadth (the Note's folder was since
/// removed) is reset to "note" via `heal_and_read_breadth`; the heal-on-read is
/// intentional.
#[tauri::command]
pub fn chat_get_breadth(
    app: AppHandle,
    note_id: Option<String>,
    conversation_id: Option<String>,
) -> Result<String, String> {
    let target = ChatTarget::from_note_id(note_id)?;
    let state: State<AppState> = app.state();
    let conn = state.db.lock();
    let ctx = ChatContext::load(&conn);
    let Some(conversation) =
        resolve_for_read(&conn, ctx.tenant(), &target, conversation_id.as_deref())?
    else {
        // No session yet → report the target's own default WITHOUT creating a row,
        // which is also what `inherited_breadth` would return here (its
        // `latest_conversation` read would be redundant — `resolve_existing` just
        // established there's no session).
        return Ok(chat::default_breadth(&target).into());
    };
    // A library-wide conversation has no anchor to heal against.
    let note = anchor_note(&conn, &target)?;
    effective_breadth(&conn, &conversation.id, &conversation.breadth, note.as_ref())
}

/// Read the persisted authorship pin (#103) so the chip initialises from the
/// backend in one round trip, exactly as the Scope chip does. `""` = off.
///
/// No session yet → off, WITHOUT creating a row: unlike breadth there is no
/// default to inherit, since a pin is only ever something the user set.
#[tauri::command]
pub fn chat_get_owner_filter(
    app: AppHandle,
    note_id: Option<String>,
    conversation_id: Option<String>,
) -> Result<String, String> {
    let target = ChatTarget::from_note_id(note_id)?;
    let state: State<AppState> = app.state();
    let conn = state.db.lock();
    let ctx = ChatContext::load(&conn);
    Ok(
        resolve_for_read(&conn, ctx.tenant(), &target, conversation_id.as_deref())?
            .map(|c| c.owner_filter)
            .unwrap_or_default(),
    )
}

/// Workspace turn-allowance for the composer meter (issue #69). Personal chat is
/// unmetered by design → `None` with no HTTP. In a workspace, GET the chat
/// service's usage endpoint (same base/token/one-shot-401-retry as the other
/// cloud chat calls). A meter must NEVER error the pane, so EVERY outcome that
/// isn't a clean metered reading maps to `None` (the client hides the display):
///
/// | Case                                   | Result         |
/// |----------------------------------------|----------------|
/// | Personal context                       | None (no HTTP) |
/// | Not configured / not signed in         | None           |
/// | Network error                          | None           |
/// | 401 (after the one-shot re-auth retry) | None           |
/// | 404 (route absent on an older server)  | None           |
/// | 400 / 402 / 403 / 5xx                  | None           |
/// | 200 `{ unmetered: true }`              | None           |
/// | 200 malformed body                     | None           |
/// | 200 `{ used_turns, cap_turns, … }`     | Some(UsageDto) |
#[tauri::command]
pub async fn chat_usage(app: AppHandle) -> Result<Option<chat::cloud::UsageDto>, String> {
    let state: State<AppState> = app.state();
    let workspace = {
        let conn = state.db.lock();
        match ChatContext::load(&conn) {
            ChatContext::Personal => return Ok(None),
            ChatContext::Workspace(id) => id,
        }
    };
    Ok(usage_cloud(&state, &workspace).await)
}

/// The name to call the person asking, or `None` when we genuinely don't know
/// one. It is the referent for "I"/"me"/"my" in a turn (#103) — without it, a
/// third of the questions people ask a meeting assistant can't be resolved.
///
/// Two sources, in order:
///
/// 1. The `user_display_name` setting, when the user has set one.
/// 2. The macOS account's full name.
///
/// The OS name matters because it's the ONLY one the local-only majority has:
/// Humla has no local account, and a cloud display name exists only once you've
/// signed in. Falling back to it is what keeps "what did I promise?" answerable
/// for a user who never signs in to anything. A short login name (`msmith`) is
/// deliberately not used — as a referent it's worse than nothing.
///
/// Never fatal: any failure just omits the line from the prompt.
pub(crate) fn asker_name(conn: &rusqlite::Connection) -> Option<String> {
    let configured = crate::db::get_setting(conn, SETTING_DISPLAY_NAME)
        .ok()
        .flatten()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    configured.or_else(os_full_name)
}

/// The macOS account's full name ("Michael Wilhelmsen"), via `id -F`. `None` if
/// the command is unavailable, fails, or returns something empty.
fn os_full_name() -> Option<String> {
    let out = std::process::Command::new("/usr/bin/id").arg("-F").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// How the workspace's retrieval index looks to search — `ready` / `empty` /
/// `quarantined` — so the chat pane can tell "your library is empty" apart from
/// "the index is still building" (issue #102).
///
/// Only the server knows the difference: workspace retrieval runs over the
/// workspace index, and a locally-empty mirror is not evidence of an empty
/// workspace while that index is backfilling. `None` means "no information" and
/// the caller keeps its local guess — Personal (where the local store IS the
/// corpus), an older server without the route, or any failure. Like the turn
/// meter, this is best-effort: it may improve the pane's copy, never break it.
#[tauri::command]
pub async fn chat_index_state(app: AppHandle) -> Result<Option<chat::cloud::IndexState>, String> {
    let state: State<AppState> = app.state();
    let workspace = {
        let conn = state.db.lock();
        match ChatContext::load(&conn) {
            ChatContext::Personal => return Ok(None),
            ChatContext::Workspace(id) => id,
        }
    };
    let body = super::cloud::cloud_get_json(
        &state,
        "/api/chat/index-state",
        &[("workspace_id", workspace.as_str())],
    )
    .await
    .ok();
    Ok(body.as_ref().and_then(chat::cloud::parse_index_state))
}

/// GET the usage endpoint, collapsing every non-metered outcome to `None` so the
/// meter can only ever report or silently hide (issue #69). See [`chat_usage`]
/// for the full mapping. One-shot 401 re-auth mirrors the other cloud calls.
async fn usage_cloud(state: &State<'_, AppState>, workspace: &str) -> Option<chat::cloud::UsageDto> {
    // Every failure — 404 / 402 / 403 / 400 / 5xx / repeated-401 / unreachable —
    // collapses to `None` here, so the meter hides rather than errors.
    let body =
        super::cloud::cloud_get_json(state, "/api/chat/usage", &[("workspace_id", workspace)])
            .await
            .ok()?;
    chat::cloud::parse_usage(&body)
}

// ── Workspace chat key (BYOK, issue #75) ─────────────────────────────────────
// Owner-only set/rotate/remove + member-readable metadata, via the PocketBase
// hook routes under /api/humla/chat/key* — through the same `cloud_*_json`
// helpers (base/token/one-shot-401-retry) as the other cloud chat calls, mapped
// with the key taxonomy `chat_key_error_message`. The key value transits Rust
// memory only: it goes straight into the POST body and is never logged, stored,
// or put in an error string — a failure is rendered from the server's
// `reason`/`error` fields alone, never from the submitted key. Only metadata
// comes back (the server's REST read rule returns null).

/// Read a workspace's chat-key metadata (member-readable). Never returns the key
/// — only `{ configured, last4, set_by, set_at, key_health }`. A 404 (route
/// absent on an older/self-hosted server) reads as "not activated" so settings
/// degrades gracefully; genuine HTTP failures error with a mapped message.
#[tauri::command]
pub async fn chat_key_meta(
    app: AppHandle,
    workspace_id: String,
) -> Result<chat::cloud::ChatKeyMeta, String> {
    let state: State<AppState> = app.state();
    let body = match super::cloud::cloud_get_json(
        &state,
        "/api/humla/chat/key/meta",
        &[("workspace_id", workspace_id.as_str())],
    )
    .await
    {
        Ok(body) => body,
        // Route absent → "not activated" rather than an error.
        Err(e) if e.is_not_found() => return Ok(chat::cloud::ChatKeyMeta::default()),
        Err(e) => return Err(e.message(chat::cloud::chat_key_error_message)),
    };
    Ok(chat::cloud::parse_key_meta(&body))
}

/// POST a key to the set/rotate hook (server test-on-saves it against OpenAI)
/// and map the response. Shared by the manual-entry and from-Keychain commands
/// so the test-on-save path is identical. The `api_key` never leaves this
/// request body — not logged, not stored, not in error strings.
async fn chat_key_set_inner(
    state: &State<'_, AppState>,
    workspace_id: &str,
    api_key: &str,
) -> Result<chat::cloud::ChatKeyMeta, String> {
    let body = serde_json::json!({ "workspace_id": workspace_id, "api_key": api_key });
    let meta = super::cloud::cloud_post_json(state, "/api/humla/chat/key", &body)
        .await
        .map_err(|e| e.message(chat::cloud::chat_key_error_message))?;
    Ok(chat::cloud::parse_key_meta(&meta))
}

/// Owner-only set/rotate of the workspace OpenAI key, from a key the owner typed
/// into the composer. The `api_key` transits Rust memory only.
#[tauri::command]
pub async fn chat_key_set(
    app: AppHandle,
    workspace_id: String,
    api_key: String,
) -> Result<chat::cloud::ChatKeyMeta, String> {
    let state: State<AppState> = app.state();
    chat_key_set_inner(&state, &workspace_id, &api_key).await
}

/// Owner-only set/rotate using the personal OpenAI key already in the macOS
/// Keychain (issue #75). Sharing a personal key with a team is a deliberate act,
/// so the UI gates this behind an explicit button. The key is read in Rust and
/// runs the same test-on-save path as `chat_key_set` — it NEVER enters the
/// webview.
#[tauri::command]
pub async fn chat_key_set_from_keychain(
    app: AppHandle,
    workspace_id: String,
) -> Result<chat::cloud::ChatKeyMeta, String> {
    let state: State<AppState> = app.state();
    let key = super::read_provider_api_key(&state, "openai")?
        .ok_or("No OpenAI key is stored in Settings → Providers.")?;
    chat_key_set_inner(&state, &workspace_id, &key).await
}

/// Owner-only remove of the workspace OpenAI key. Returns the (unconfigured)
/// metadata so the UI updates in place.
#[tauri::command]
pub async fn chat_key_delete(
    app: AppHandle,
    workspace_id: String,
) -> Result<chat::cloud::ChatKeyMeta, String> {
    let state: State<AppState> = app.state();
    let body = super::cloud::cloud_delete_json(
        &state,
        "/api/humla/chat/key",
        &[("workspace_id", workspace_id.as_str())],
    )
    .await
    .map_err(|e| e.message(chat::cloud::chat_key_error_message))?;
    Ok(chat::cloud::parse_key_meta(&body))
}

/// Registers an in-flight turn's stop signal for a pane and clears it on drop,
/// so no early return can leak an entry that would let a later `chat_cancel`
/// abort the *next* turn (issue #80).
struct TurnCancel {
    registry: Arc<Mutex<std::collections::HashMap<String, Arc<chat::CancelFlag>>>>,
    key: String,
    flag: Arc<chat::CancelFlag>,
}

impl TurnCancel {
    fn register(state: &State<'_, AppState>, key: &str) -> Self {
        let flag = Arc::new(chat::CancelFlag::new());
        let registry = Arc::clone(&state.chat_cancels);
        // A fresh send for the same pane replaces any stale entry.
        registry.lock().insert(key.to_string(), Arc::clone(&flag));
        Self { registry, key: key.to_string(), flag }
    }

    fn flag(&self) -> &chat::CancelFlag {
        &self.flag
    }
}

impl Drop for TurnCancel {
    fn drop(&mut self) {
        let mut map = self.registry.lock();
        // Only clear OUR flag: a newer send for this pane may already have
        // replaced it, and removing that one would make its stop button dead.
        let ours = map.get(&self.key).is_some_and(|f| Arc::ptr_eq(f, &self.flag));
        if ours {
            map.remove(&self.key);
        }
    }
}

/// Stop the turn currently streaming in a pane. A no-op when nothing is in
/// flight, so a stray click can't error. The partial answer (if any text
/// arrived) is kept — see `chat::run_chat`.
///
/// Note this is a *client-side* stop for a workspace turn: the stream is
/// dropped, but the server finishes generating and the tokens are really spent,
/// so a metered turn still counts. A server-side cancel is the follow-up.
#[tauri::command]
pub fn chat_cancel(app: AppHandle, note_id: Option<String>) -> Result<(), String> {
    let state: State<AppState> = app.state();
    // Panes are keyed by `scope_id`, which is the note id for a note pane and the
    // global sentinel for the library pane — so the two can't stop each other.
    let target = ChatTarget::from_note_id(note_id)?;
    if let Some(flag) = state.chat_cancels.lock().get(target.scope_id()) {
        flag.cancel();
    }
    Ok(())
}

#[tauri::command]
pub async fn chat_send(
    app: AppHandle,
    note_id: Option<String>,
    conversation_id: Option<String>,
    message: String,
    // See `chat_send_cloud` — display name for a pinned authorship filter.
    // Ignored on the Personal path, which never pins one.
    owner_name: Option<String>,
    // A drafting pane's breadth / authorship pin, chosen before any row existed
    // (#120). Only read when this turn CREATES the conversation; an existing row
    // is already the source of truth for both.
    draft_breadth: Option<String>,
    draft_owner_filter: Option<String>,
) -> Result<ChatSendResult, String> {
    let target = ChatTarget::from_note_id(note_id)?;
    let state: State<AppState> = app.state();
    // Validated on the way in, to the same standard as `chat_set_breadth` and
    // `chat_set_owner_filter`. These two fields reach the same columns those
    // commands guard, so accepting them unchecked here would be a second, unguarded
    // door onto the same state — and the frontend enforcing it is not the same as
    // the backend enforcing it.
    let draft = validated_draft(&state, &target, draft_breadth, draft_owner_filter)?;
    // Chat is pinned to the loaded context (issue #58): a loaded workspace →
    // the Teams (cloud) path; Personal (no workspace) → the on-device path
    // below. There is no user-chosen tenant — it follows the sidebar workspace.
    let in_workspace = {
        let conn = state.db.lock();
        ChatContext::load(&conn).workspace().is_some()
    };
    if in_workspace {
        return chat_send_cloud(app, target, conversation_id, message, owner_name, draft).await;
    }

    // Keychain read out of band — not inside the DB lock. Chat reuses the
    // shared OpenAI key (issue #44).
    let openai_api_key = super::read_provider_api_key(&state, "openai")?;

    let (grounding, resolved, conversation_id, tool_scope, workspace) = {
        let conn = state.db.lock();
        let note = anchor_note(&conn, &target)?;
        // We branched to the Personal path above (no active workspace), so the
        // tenant is Personal and there's no workspace to scope tools to.
        let workspace = String::new();
        let resolved = resolve_chat(&conn, openai_api_key).map_err(|e| e.to_string())?;
        // Resolve the target session (issue #61): an explicit id, else the
        // active/most-recent one, lazily creating the target's first session on
        // the first send. Breadth is a persisted live filter within it.
        let conversation =
            resolve_or_create(&conn, CHAT_TENANT_PERSONAL, &target, conversation_id.as_deref(), draft)?;
        // Set the personal session's title once, from its first user message
        // (issue #61). Guarded on an empty title so later turns never rewrite it.
        if resolved_title_is_unset(&conversation) {
            let title = chat::derive_title(Some(&message), conversation.created_at);
            let _ = db::set_conversation_title(&conn, &conversation.id, &title);
        }
        let (grounding, body_text) = turn_grounding(note.as_ref());
        // Keep the anchor Note searchable — reindex it now so "this Note" and
        // any broader search always find the note the user is looking at, even
        // if a content-settled checkpoint hasn't fired yet. Nothing to do for a
        // library-wide turn: every note is reindexed at its own checkpoints.
        if let (Some(note), Some(body_text)) = (note.as_ref(), body_text.as_deref()) {
            let _ = db::reindex_note(&conn, &note.id, body_text, &note.transcript, &note.summary);
        }
        // Breadth is read from the conversation row (single source of truth),
        // self-healed against the Note's current folder.
        let breadth =
            effective_breadth(&conn, &conversation.id, &conversation.breadth, note.as_ref())?;
        // No authorship pin is read here: `chat_set_owner_filter` refuses one in
        // Personal, where every note is the user's own, so the column is always
        // empty on this path. The workspace turn (`chat_send_cloud`) is where the
        // pin is read and sent. Keep this in step with that rejection — a pin that
        // could be stored but not applied is the silent-no-op shape (#103).
        let tool_scope = resolve_scope(&breadth, note.as_ref())?;
        (grounding, resolved, conversation.id, tool_scope, workspace)
    };

    // Embed the anchor Note now (best-effort, cached) so semantic search works
    // for the note the user is chatting about on the very first question. Other
    // notes are embedded at their own checkpoints (issue #48) — so a library-wide
    // turn has nothing to pre-embed and skips straight to the loop.
    let embed_cfg = resolve_embed(&resolved);
    let embedder = embed_cfg.adapter();
    if let Some(anchor) = target.note_id() {
        embed_note(&state.db, &embedder, anchor).await;
    }

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

    // Register the stop signal for this pane; dropped (and cleared) when the
    // turn finishes, however it finishes (issue #80).
    let turn = TurnCancel::register(&state, target.scope_id());
    let ctx = ChatCtx {
        model: &resolved.model,
        api_key: resolved.api_key.as_deref(),
        base_url: &resolved.base_url,
        think: resolved.think,
        cancel: turn.flag(),
    };
    // Who is asking, for the prompt's first-person referent (#103). Resolved here
    // rather than inside run_chat so the prompt's inputs stay explicit and the
    // loop's tests stay independent of the host machine's account name.
    let asker = {
        let conn = state.db.lock();
        asker_name(&conn)
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
        asker.as_deref(),
        sink,
    )
    .await;

    match result {
        Ok(()) => Ok(ChatSendResult { conversation_id, truncated: grounding.truncated }),
        Err(e) => {
            let message = e.to_string();
            let _ = app.emit(
                "chat_error",
                ChatErrorPayload {
                    conversation_id: conversation_id.clone(),
                    message: message.clone(),
                    reason: String::new(),
                },
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
    target: ChatTarget,
    conversation_id: Option<String>,
    message: String,
    // Display name for a pinned authorship filter, resolved by the caller from
    // the workspace roster. Prompt text only — the id on the conversation row is
    // what actually filters — so it is passed per turn rather than cached in a
    // column that a rename would leave stale.
    owner_name: Option<String>,
    draft: DraftSettings,
) -> Result<ChatSendResult, String> {
    let state: State<AppState> = app.state();
    let (workspace, folder_id, title, conversation, breadth, owner_filter, folder_display) = {
        let conn = state.db.lock();
        let ctx = ChatContext::load(&conn);
        let Some(workspace) = ctx.workspace() else {
            return Err("No workspace selected — switch to a workspace to use Team chat.".into());
        };
        let note = anchor_note(&conn, &target)?;
        // Resolve the target workspace-handle session (issue #61): an explicit
        // id, else the active/most-recent handle, lazily creating one (remote_id
        // filled from the server's response below) on the first turn.
        let conversation =
            resolve_or_create(&conn, workspace, &target, conversation_id.as_deref(), draft)?;
        // Breadth is read from the (workspace) conversation row and self-healed
        // against the Note's folder, exactly like the Personal path (issue #58).
        let breadth =
            effective_breadth(&conn, &conversation.id, &conversation.breadth, note.as_ref())?;
        // The pinned authorship filter (#103), read from the same row as breadth
        // and binding the same way: it's the user's stated intent, so it applies
        // whatever the model asks for.
        let owner_filter = conversation.owner_filter.clone();
        // The folder's display NAME for #113's breadth disclosure — the server has to
        // say `the folder "K2 pilot"`, and the scope carries only an id the user has
        // never seen. Resolved here beside the folder id it names; `None` degrades to
        // no disclosure rather than to an empty quoted string.
        let folder_display = note
            .as_ref()
            .and_then(|n| n.folder_id.clone())
            .and_then(|id| db::folder_name(&conn, &id).ok().flatten());
        (
            workspace.to_string(),
            note.as_ref().and_then(|n| n.folder_id.clone()),
            // The server derives a global conversation's title from the first
            // message, the same as Personal; there's no note title to send.
            note.as_ref().map(|n| n.title.clone()).unwrap_or_default(),
            conversation,
            breadth,
            owner_filter,
            folder_display,
        )
    };

    let body = chat::cloud::build_cloud_request(
        conversation.remote_id.as_deref(),
        &workspace,
        &message,
        Some(&title),
        &breadth,
        target.note_id(),
        folder_id.as_deref(),
        // The name is display-only, resolved by the caller from the workspace
        // roster; the id is what the server filters on.
        (!owner_filter.trim().is_empty())
            .then(|| (owner_filter.as_str(), owner_name.as_deref().unwrap_or(""))),
        // Display names for #113's disclosure. `title` IS the anchor note's title
        // here (the server derives a conversation title from it), so it doubles as
        // the note-breadth name rather than being resolved twice.
        Some(title.as_str()).filter(|t| !t.trim().is_empty()),
        folder_display.as_deref(),
    )?;

    // Stream the turn, retrying once after a 401 (a stale cached token → forget
    // it and re-authenticate from stored credentials, mirroring cloud.rs).
    // Stop signal for this pane (issue #80). A workspace stop is client-side
    // only: it ends the stream so tokens stop appearing, but the server keeps
    // generating and finishes the turn, so a metered turn still counts and the
    // history reload afterwards shows the server's complete answer rather than
    // the partial. A server-side cancel is the follow-up (humla-cloud#26).
    let turn_cancel = TurnCancel::register(&state, target.scope_id());
    let mut attempt = 0u8;
    let outcome = loop {
        let (base, token) = super::cloud::cloud_session(&state).await?;
        let turn =
            stream_cloud_turn(&app, &base, &token, &body, &conversation.id, turn_cancel.flag())
                .await;
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
                    ChatErrorPayload {
                        conversation_id: conversation.id.clone(),
                        message: e.clone(),
                        reason: String::new(),
                    },
                );
                return Err(e);
            }
        }
    };

    if let Some((_, reason, server_msg)) = outcome.preflight {
        let text = chat::cloud::cloud_chat_error_message(&reason, &server_msg);
        let _ = app.emit(
            "chat_error",
            ChatErrorPayload {
                conversation_id: conversation.id.clone(),
                message: text.clone(),
                reason: reason.clone(),
            },
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
    cancel: &chat::CancelFlag,
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
        // Stopped: drop the stream so deltas stop reaching the UI (issue #80).
        // Anything already framed has been emitted; the server finishes its turn
        // regardless, so this is a UI stop, not a billing one.
        if cancel.is_cancelled() {
            break;
        }
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

/// Reload a Note's conversation and report which session it resolved to (issue
/// #61). `conversation_id` targets a specific session; None resolves the
/// active/most-recent one (this is what keeps the single-thread panel working).
/// When the Note has no session yet, returns empty messages + `conversationId:
/// None` WITHOUT creating a row — merely opening the Chat tab must persist
/// nothing. For a workspace tenant (issue #50) the messages are a read-through
/// of the server-authoritative conversation (via the handle's `remote_id`);
/// they never live in the local `messages` table.
#[tauri::command]
pub async fn chat_history(
    app: AppHandle,
    note_id: Option<String>,
    conversation_id: Option<String>,
) -> Result<ChatHistoryResult, String> {
    let target = ChatTarget::from_note_id(note_id)?;
    let state: State<AppState> = app.state();
    // Chat follows the loaded context (issue #58): a loaded workspace reads its
    // server-authoritative conversation; Personal reads the local table.
    let ctx = {
        let conn = state.db.lock();
        ChatContext::load(&conn)
    };

    if let Some(workspace) = ctx.workspace() {
        // Resolve the target workspace handle (explicit id, else most-recent),
        // then read its messages through PocketBase (source of truth for shared
        // conversations). No handle → nothing persisted yet, so empty + None.
        let handle = {
            let conn = state.db.lock();
            resolve_for_read(&conn, workspace, &target, conversation_id.as_deref())?
        };
        let Some(handle) = handle else {
            return Ok(ChatHistoryResult { conversation_id: None, messages: Vec::new() });
        };
        let messages = match handle.remote_id.as_deref() {
            Some(remote_id) => {
                let records = super::cloud::fetch_chat_messages(&state, remote_id).await?;
                records.iter().filter_map(map_remote_message).collect()
            }
            None => Vec::new(),
        };
        return Ok(ChatHistoryResult { conversation_id: Some(handle.id), messages });
    }

    let conn = state.db.lock();
    let Some(conversation) =
        resolve_for_read(&conn, CHAT_TENANT_PERSONAL, &target, conversation_id.as_deref())?
    else {
        return Ok(ChatHistoryResult { conversation_id: None, messages: Vec::new() });
    };
    let messages = db::list_chat_messages(&conn, &conversation.id).map_err(|e| e.to_string())?;
    let messages = messages
        .into_iter()
        .map(|m| ChatMessageDto {
            id: m.id,
            role: m.role,
            seq: m.seq,
            parts: serde_json::to_value(chat::parse_parts(&m.content)).unwrap_or_default(),
            created_at: m.created_at,
        })
        .collect();
    Ok(ChatHistoryResult { conversation_id: Some(conversation.id), messages })
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

// ── Workspace (Teams) session list/create — humla-cloud#19 contract ──────────
//
// Workspace conversations are server-authoritative (issue #50); the desktop
// keeps a local handle row per server conversation, whose `remote_id` maps to
// the server record. The two routes below are PocketBase HOOKS under `/api/humla`
// (NOT the chat service `/api/chat*`, which the prod Caddyfile routes to a
// separate service that has no such route). PocketBase bearer token, JSON
// bodies. Timestamps are PocketBase-native `created` / `updated`, parsed the
// same way as the `chat_messages` read-through (`map_remote_message`).
//
//   GET /api/humla/chat/conversations?workspace_id=<ws>&note_id=<note>
//     Authorization: Bearer <pb_token>
//     → 200 { "conversations": [ {
//           "id":            <server conversation record id>,
//           "title":         <string, may be empty>,
//           "breadth":       "note" | "folder" | "all",
//           "created":       <PB datetime>,
//           "updated":       <PB datetime>,
//           "message_count": <int>,
//           "created_by":    <pb user id>
//         }, … ] }          // ordered most-recently-updated first (-updated)
//     Membership is enforced by the token; a non-member → 403 { reason:"not_a_member", error }.
//
//   POST /api/humla/chat/conversations
//     Authorization: Bearer <pb_token>
//     Content-Type: application/json
//     { "workspace_id": <ws>, "note_id": <note>, "breadth": "note"|"folder"|"all" }
//     → 200 <conversation>          // FLAT object, same shape as a GET list item;
//                                   // there is NO { "conversation": … } wrapper.
//     The server is a DUMB creator: it creates unconditionally and honors the
//     POST `breadth`. The no-op guard is therefore CLIENT-side (see
//     `new_conversation_cloud`): list first, and if the most-recent session has
//     zero messages, reuse it instead of POSTing. Errors mirror `/api/chat`'s
//     preflight shape { reason, error }.
//
// Legacy merge (pre-#19): conversations that predate #19 have NO server-side note
// association, so the per-note GET omits them until one "adopting turn" — the
// persist route stamps `note` one-way from the send scope's note_id, only when
// currently empty. The client therefore UNIONS the server list with local handle
// rows for (tenant=workspace, scope_id=note) whose `remote_id` is absent from the
// server response (dedup by remote_id — `unlisted_legacy_handles`), using the
// local row's breadth/updated_at (title may be empty this slice). Resolving the
// most-recent session needs no extra work: those legacy threads already exist as
// local handle rows, so `latest_conversation` surfaces them — a legacy-only note
// still resolves its thread, `chat_history` reads it through by `remote_id`, and
// the first send continues it (after which the server adopts it into the list).
//
// Identity: metas returned to the frontend use the LOCAL handle id (reconciled by
// `remote_id`), so history/send/breadth all key on the same id across tenants.
//
// 404 fallback: a self-hosted server without these routes degrades to the legacy
// single-conversation read-through (`legacy_workspace_sessions`).

/// Get-or-create the LOCAL handle row for a server conversation (issue #61),
/// matched by `remote_id`. A new handle adopts the server's breadth; an existing
/// one is returned untouched. Keeps server conversations from being duplicated
/// locally as the session list reconciles them.
fn ensure_workspace_handle(
    conn: &rusqlite::Connection,
    workspace: &str,
    target: &ChatTarget,
    remote_id: &str,
    breadth: &str,
    owner_filter: Option<&str>,
) -> Result<db::Conversation, String> {
    // An EXISTING handle is returned untouched — the local row is the source of
    // truth for both breadth and the pin, and this reconciliation runs on every
    // list refresh. Letting the server win here would resurrect a pin the user
    // just cleared: clear it, don't send a turn (so the persist never mirrors the
    // clear upward), reopen the pane, and the stale server value writes itself
    // back over the local "". The filter would start applying again with nobody
    // having touched it — the silent-no-op shape this whole area keeps producing.
    //
    // The cost is that a pin set on another device reaches only a device that has
    // not opened this thread yet (which adopts it below, at handle creation).
    // That is the same guarantee breadth has always had, and the failure it trades
    // for — a pin that doesn't propagate — is visible on the chip, where a pin
    // that undoes itself is not.
    if let Some(c) = db::get_conversation_by_remote_id(conn, workspace, remote_id)
        .map_err(|e| e.to_string())?
    {
        return Ok(c);
    }
    let breadth = chat::validate_breadth(breadth).unwrap_or_else(|_| chat::default_breadth(target));
    let handle =
        db::create_conversation(conn, workspace, target.scope(), target.scope_id(), breadth)
            .map_err(|e| e.to_string())?;
    db::set_conversation_remote_id(conn, &handle.id, remote_id).map_err(|e| e.to_string())?;
    let pin = owner_filter.unwrap_or_default();
    if !pin.is_empty() {
        db::set_conversation_owner_filter(conn, &handle.id, Some(pin))
            .map_err(|e| e.to_string())?;
        return Ok(db::Conversation { owner_filter: pin.to_string(), ..handle });
    }
    Ok(handle)
}

/// Parse a PocketBase timestamp to epoch-ms (0 on failure). Matches the
/// `chat_messages` read-through's handling (`map_remote_message`) so all
/// server-sourced dates use one parser.
fn pb_timestamp_ms(v: Option<&serde_json::Value>) -> i64 {
    v.and_then(|x| x.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.timestamp_millis())
        .unwrap_or(0)
}

/// Map one server conversation JSON to a session-list entry, reconciling it to
/// its local handle so the returned `id` is local. Handles both the GET list
/// items and the FLAT POST-create response (same shape).
fn cloud_conversation_meta(
    state: &State<AppState>,
    workspace: &str,
    target: &ChatTarget,
    item: &serde_json::Value,
) -> Result<ConversationMeta, String> {
    let remote_id = item
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or("Team chat returned a conversation without an id")?;
    let breadth =
        item.get("breadth").and_then(|v| v.as_str()).unwrap_or_else(|| chat::default_breadth(target));
    // `None` is a server that doesn't know the field (pre-#39, or self-hosted and
    // not yet updated) — NOT the same as "off", and the distinction matters at
    // handle creation, where absent must not stamp an empty pin over an inherited
    // one.
    let owner_filter = item.get("owner_filter").and_then(|v| v.as_str());
    let handle = {
        let conn = state.db.lock();
        ensure_workspace_handle(&conn, workspace, target, remote_id, breadth, owner_filter)?
    };
    Ok(ConversationMeta {
        id: handle.id,
        title: item.get("title").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
        breadth: breadth.to_string(),
        // The HANDLE's pin, not the server item's: the local row is what the next
        // turn reads, so reporting the server's would let the chip show a filter
        // the turn isn't applying.
        owner_filter: handle.owner_filter,
        updated_at: pb_timestamp_ms(item.get("updated")),
        message_count: item.get("message_count").and_then(|v| v.as_i64()).unwrap_or(0),
    })
}

/// Local workspace handles the server GET omitted — the legacy-merge core (issue
/// #61). A handle is "unlisted" when it has a `remote_id` (so it maps to a real
/// server conversation) that is NOT among the server's returned ids. Pure so the
/// union/dedup logic is unit-testable without HTTP.
fn unlisted_legacy_handles<'a>(
    server_remote_ids: &std::collections::HashSet<String>,
    local_handles: &'a [db::Conversation],
) -> Vec<&'a db::Conversation> {
    local_handles
        .iter()
        .filter(|h| {
            h.remote_id
                .as_deref()
                .is_some_and(|rid| !server_remote_ids.contains(rid))
        })
        .collect()
}

/// List a workspace Note's sessions from humla-cloud (issue #61). GETs the server
/// list (one-shot 401 re-auth, 404 → legacy fallback), reconciles each to a local
/// handle, then UNIONS in local handles the server omitted (pre-#19 threads not
/// yet adopted — their message count is read through). Most-recently-updated
/// first.
/// Query params for the conversations route. `note_id` is optional (humla-cloud#26)
/// and its ABSENCE is what selects the library-wide list — the server keys that off
/// its own `scope` field, so the param must be omitted rather than sent as a
/// sentinel it would reject as malformed.
fn conversation_route_params<'a>(
    workspace: &'a str,
    target: &'a ChatTarget,
) -> Vec<(&'a str, &'a str)> {
    let mut params = vec![("workspace_id", workspace)];
    if let Some(id) = target.note_id() {
        params.push(("note_id", id));
    }
    params
}

/// Body for creating a conversation server-side. Same rule as the params above,
/// plus the anchor-less-must-be-`all` check in its one owner.
fn new_conversation_body(
    workspace: &str,
    target: &ChatTarget,
    breadth: &str,
) -> Result<serde_json::Value, String> {
    chat::check_anchor(breadth, target.note_id().is_some())?;
    let mut body = serde_json::json!({ "workspace_id": workspace, "breadth": breadth });
    if let Some(id) = target.note_id() {
        body["note_id"] = serde_json::json!(id);
    }
    Ok(body)
}

async fn list_conversations_cloud(
    state: &State<'_, AppState>,
    workspace: &str,
    target: &ChatTarget,
) -> Result<Vec<ConversationMeta>, String> {
    let params = conversation_route_params(workspace, target);
    let val = match super::cloud::cloud_get_json(
        state,
        "/api/humla/chat/conversations",
        &params,
    )
    .await
    {
        Ok(val) => val,
        // Route absent (pre-#19 server) → the single-handle fallback.
        Err(e) if e.is_not_found() => {
            return legacy_workspace_sessions(state, workspace, target).await
        }
        Err(e) => return Err(e.message(chat::cloud::cloud_chat_error_message)),
    };

    let items = val.get("conversations").and_then(|c| c.as_array()).cloned().unwrap_or_default();
    let mut metas: Vec<ConversationMeta> = Vec::with_capacity(items.len());
    let mut server_remote_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let hide_empty = list_filter_for(target) == db::ListFilter::WithMessages;
    for item in &items {
        if let Some(rid) = item.get("id").and_then(|v| v.as_str()) {
            server_remote_ids.insert(rid.to_string());
        }
        // A drafting target can never reopen an empty thread, so an empty row can
        // only be residue — from an older client, or a thread abandoned before its
        // first turn. Emptiness is read from the SERVER's count, deliberately: a
        // workspace's messages live in PocketBase and are never written to the local
        // table, so the local count reads 0 for every workspace thread and would
        // hide the entire list. Still recorded in `server_remote_ids` above, so a
        // hidden row doesn't get resurrected by the legacy union below.
        if hide_empty && item.get("message_count").and_then(|v| v.as_i64()).unwrap_or(0) == 0 {
            continue;
        }
        metas.push(cloud_conversation_meta(state, workspace, target, item)?);
    }

    // Union: local handles the server GET omitted (pre-#19, not yet adopted).
    // Snapshot the fields we need up front so no borrow / db lock crosses the
    // read-through `.await`.
    let legacy: Vec<(String, String, String, i64, String)> = {
        let conn = state.db.lock();
        let local_handles =
            db::list_conversations(
                &conn,
                workspace,
                target.scope(),
                target.scope_id(),
                None,
                // NOT `list_filter_for` — deliberately. This reads LOCAL handles for
                // a workspace, and a workspace's messages live in PocketBase and are
                // never written to the local table, so a `WithMessages` filter here
                // matches nothing and would drop every server-omitted handle from
                // the union. The empty-thread filter for a workspace is applied
                // above, off the SERVER's `message_count`, which is the only count
                // that means anything for these rows.
                db::ListFilter::All,
            )
                .map_err(|e| e.to_string())?;
        unlisted_legacy_handles(&server_remote_ids, &local_handles)
            .into_iter()
            .map(|h| {
                (
                    h.id.clone(),
                    h.remote_id.clone().unwrap_or_default(),
                    h.breadth.clone(),
                    h.updated_at,
                    resolved_title(h),
                )
            })
            .collect()
    };
    for (id, remote_id, breadth, updated_at, title) in legacy {
        // A legacy handle has a remote_id, so it has server messages; read the
        // real count so the no-op guard never reuses a non-empty legacy thread.
        let message_count = super::cloud::fetch_chat_messages(state, &remote_id)
            .await
            .map(|r| r.len() as i64)
            .unwrap_or(0);
        // A legacy handle predates the pin, so it has none.
        metas.push(ConversationMeta {
            id,
            title,
            breadth,
            owner_filter: String::new(),
            updated_at,
            message_count,
        });
    }

    // Server list is already -updated; re-sort so merged legacy entries land in
    // the right place (id break-ties deterministically).
    metas.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then_with(|| b.id.cmp(&a.id)));
    Ok(metas)
}

/// The pre-#19 fallback: the note's single workspace handle (if it exists) as a
/// one-element session list, its message count read through from the server.
async fn legacy_workspace_sessions(
    state: &State<'_, AppState>,
    workspace: &str,
    target: &ChatTarget,
) -> Result<Vec<ConversationMeta>, String> {
    let handle = {
        let conn = state.db.lock();
        db::latest_conversation(&conn, workspace, target.scope(), target.scope_id())
            .map_err(|e| e.to_string())?
    };
    let Some(handle) = handle else { return Ok(Vec::new()) };
    let message_count = match handle.remote_id.as_deref() {
        Some(remote_id) => {
            super::cloud::fetch_chat_messages(state, remote_id).await.map(|r| r.len() as i64).unwrap_or(0)
        }
        None => 0,
    };
    Ok(vec![ConversationMeta {
        title: resolved_title(&handle),
        id: handle.id,
        breadth: handle.breadth,
        owner_filter: handle.owner_filter,
        updated_at: handle.updated_at,
        message_count,
    }])
}

/// Create a workspace session via humla-cloud (issue #61). The server is a dumb
/// creator, so the no-op guard is CLIENT-side: list first (server ∪ legacy) and,
/// if the most-recent session is empty, reuse it instead of POSTing. Otherwise
/// POST — inheriting breadth from the Note's most-recent workspace session, which
/// the server honors — and reconcile the FLAT response to a local handle. A 404
/// (route absent on an older server) is a clean error: a server-authoritative
/// conversation can't be created without the server.
async fn new_conversation_cloud(
    state: &State<'_, AppState>,
    workspace: &str,
    target: &ChatTarget,
) -> Result<ConversationMeta, String> {
    // Client-side no-op guard: reuse the empty most-recent session if there is
    // one (the list already unions server + legacy and is most-recent-first).
    let existing = list_conversations_cloud(state, workspace, target).await?;
    if let Some(most_recent) = existing.into_iter().next() {
        if most_recent.message_count == 0 {
            return Ok(most_recent);
        }
    }

    // Inherit breadth from the target's most-recent session in THIS workspace,
    // just like the personal path (the target's own default when there's none).
    let breadth = {
        let conn = state.db.lock();
        inherited_breadth(&conn, workspace, target)
    };
    let body = new_conversation_body(workspace, target, &breadth)?;
    // The POST response is the conversation object itself — no wrapper.
    let val = match super::cloud::cloud_post_json(state, "/api/humla/chat/conversations", &body)
        .await
    {
        Ok(val) => val,
        Err(e) if e.is_not_found() => {
            return Err("Starting a new Team chat needs a newer server — this workspace's server \
                        doesn't support multiple chat sessions yet."
                .into())
        }
        Err(e) => return Err(e.message(chat::cloud::cloud_chat_error_message)),
    };
    cloud_conversation_meta(state, workspace, target, &val)
}

#[cfg(test)]
mod tests {
    use super::*;
    // Production code derives the scope from a `ChatTarget`; the tests still name
    // it directly to assert the stored value is what we think it is.
    use crate::db::CHAT_SCOPE_NOTE;

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

        assert!(matches!(resolve_scope("all", Some(&no_folder)).unwrap(), ToolScope::All));
        assert!(matches!(resolve_scope("note", Some(&no_folder)).unwrap(), ToolScope::Note(_)));
        match resolve_scope("folder", Some(&with_folder)).unwrap() {
            ToolScope::Folder(id) => assert_eq!(id, folder.id),
            other => panic!("expected Folder scope, got {other:?}"),
        }
        // Unknown breadth is loud (issue #58) — the old `_ => Note` clamp is gone.
        assert!(resolve_scope("everything", Some(&no_folder)).is_err());
        // Folder breadth on a folder-less note errors rather than clamping.
        assert!(resolve_scope("folder", Some(&no_folder)).is_err());
    }

    // ── #93: library-wide (global) scope ────────────────────────────────────

    /// A library-wide turn resolves straight to `All` without an anchor — the
    /// retrieval tools carry it alone (#82: no fallback note is substituted).
    #[test]
    fn resolve_scope_resolves_a_library_wide_turn_without_a_note() {
        assert!(matches!(resolve_scope("all", None).unwrap(), ToolScope::All));
        // Every anchor-requiring breadth is LOUD without an anchor rather than
        // widening to the whole library — a corrupt global row must not silently
        // search everything. `heal_and_read_global_breadth` rewrites such rows to
        // "all" before a turn gets here, so these arms are belt-and-suspenders.
        for breadth in ["note", "folder"] {
            assert!(resolve_scope(breadth, None).is_err(), "{breadth} should not widen silently");
        }
        // Garbage is still loud.
        assert!(resolve_scope("everything", None).is_err());
    }

    /// #93's proof is Rust tests, so the two cloud shapes are asserted directly
    /// rather than left to inspection inside async command bodies. Both encode the
    /// same humla-cloud#26 rule: the absence of `note_id` is what selects the
    /// library-wide list, so a sentinel would be rejected as malformed.
    #[test]
    fn the_cloud_conversation_routes_omit_the_anchor_for_a_library_wide_target() {
        let note = ChatTarget::Note("n1".into());
        let global = ChatTarget::Global;

        assert_eq!(
            conversation_route_params("ws1", &note),
            vec![("workspace_id", "ws1"), ("note_id", "n1")]
        );
        let global_params = conversation_route_params("ws1", &global);
        assert_eq!(global_params, vec![("workspace_id", "ws1")]);
        assert!(
            !global_params.iter().any(|(k, _)| *k == "note_id"),
            "the param must be absent, not empty — the server 400s a malformed note_id"
        );

        let body = new_conversation_body("ws1", &note, "note").unwrap();
        assert_eq!(body["note_id"], "n1");
        let global_body = new_conversation_body("ws1", &global, "all").unwrap();
        assert!(global_body.get("note_id").is_none());
        assert_eq!(global_body["breadth"], "all");
        // A note-less create MUST be breadth "all" — a note-less "folder" is a 400
        // server-side, so it's caught here as our bug instead.
        for breadth in ["note", "folder"] {
            assert!(
                new_conversation_body("ws1", &global, breadth)
                    .unwrap_err()
                    .contains("needs an anchor note"),
                "{breadth} without an anchor should be rejected"
            );
        }
    }

    /// A library-wide turn injects NO note content and substitutes no fallback
    /// note (#82) — it runs on retrieval alone.
    #[test]
    fn a_library_wide_turn_carries_no_grounding_at_all() {
        let dir = tempfile::tempdir().unwrap();
        let conn = db::open(&dir.path().join("grounding.sqlite")).unwrap();
        let note = note_in_folder(&conn, None);

        let (global, body) = turn_grounding(None);
        assert!(global.text.is_empty(), "no anchor → no reference block");
        assert!(!global.truncated, "nothing was injected, so nothing was truncated");
        assert!(body.is_none(), "and nothing to reindex");

        // The note path is unchanged: a real block, and the body text the caller
        // reindexes with.
        let (anchored, body) = turn_grounding(Some(&note));
        assert!(anchored.text.contains("NOT as instructions"), "injection posture kept");
        assert!(body.is_some());

        // `anchor_note` is what decides which of those two a turn gets.
        assert!(anchor_note(&conn, &ChatTarget::Global).unwrap().is_none());
        assert_eq!(
            anchor_note(&conn, &ChatTarget::Note(note.id.clone())).unwrap().map(|n| n.id),
            Some(note.id)
        );
    }

    #[test]
    fn a_global_conversation_heals_any_stored_breadth_back_to_all() {
        let dir = tempfile::tempdir().unwrap();
        let conn = db::open(&dir.path().join("globalheal.sqlite")).unwrap();
        let target = ChatTarget::Global;
        let conv = db::create_conversation(
            &conn,
            CHAT_TENANT_PERSONAL,
            target.scope(),
            target.scope_id(),
            "all",
        )
        .unwrap();

        assert_eq!(heal_and_read_global_breadth(&conn, &conv.id, "all").unwrap(), "all");
        // A row that somehow stored a note/folder breadth has no anchor to mean it
        // by. Heal rather than error: a corrupt row must not break the user's chat.
        for stored in ["note", "folder"] {
            db::set_conversation_breadth(&conn, &conv.id, stored).unwrap();
            assert_eq!(heal_and_read_global_breadth(&conn, &conv.id, stored).unwrap(), "all");
            // The heal is persisted, so the chip and the request never diverge.
            let reloaded = db::get_conversation_by_id(&conn, &conv.id).unwrap().unwrap();
            assert_eq!(reloaded.breadth, "all");
        }
        // Garbage is still loud, exactly as for a note conversation.
        assert!(heal_and_read_global_breadth(&conn, &conv.id, "bogus").is_err());
    }

    #[test]
    fn a_global_session_is_separate_from_every_notes_and_starts_at_all() {
        let dir = tempfile::tempdir().unwrap();
        let conn = db::open(&dir.path().join("globalsess.sqlite")).unwrap();
        let note = note_in_folder(&conn, None);
        let note_target = ChatTarget::Note(note.id.clone());
        let global = ChatTarget::Global;

        // First-ever sessions take their target's own default breadth.
        assert_eq!(inherited_breadth(&conn, CHAT_TENANT_PERSONAL, &global), "all");
        assert_eq!(inherited_breadth(&conn, CHAT_TENANT_PERSONAL, &note_target), "note");

        let g1 = new_personal_conversation(&conn, &global).unwrap();
        assert_eq!(g1.breadth, "all");
        assert_eq!(g1.scope, db::CHAT_SCOPE_GLOBAL);
        // The no-op guard applies to the library pane too: an empty most-recent
        // session is reused rather than piling up.
        assert_eq!(new_personal_conversation(&conn, &global).unwrap().id, g1.id);

        // A note's session is a different row and doesn't appear in the library
        // list (nor the reverse).
        let n1 = new_personal_conversation(&conn, &note_target).unwrap();
        assert_ne!(n1.id, g1.id);
        let listed = |t: &ChatTarget| {
            db::list_conversations(&conn, CHAT_TENANT_PERSONAL, t.scope(), t.scope_id(), None, db::ListFilter::All)
                .unwrap()
                .into_iter()
                .map(|c| c.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(listed(&global), vec![g1.id.clone()]);
        assert_eq!(listed(&note_target), vec![n1.id.clone()]);

        // An explicit id from the other scope is a clean error, never a silent
        // cross-scope read.
        let err = resolve_existing(&conn, CHAT_TENANT_PERSONAL, &global, Some(&n1.id)).unwrap_err();
        assert!(err.contains("library-wide"), "got: {err}");
        assert!(resolve_existing(&conn, CHAT_TENANT_PERSONAL, &note_target, Some(&g1.id))
            .unwrap_err()
            .contains("this note"));
    }

    #[test]
    fn heal_and_read_breadth_self_heals_a_stale_folder_breadth_to_note() {
        let dir = tempfile::tempdir().unwrap();
        let conn = db::open(&dir.path().join("heal.sqlite")).unwrap();
        let note = note_in_folder(&conn, None); // no folder
        let conv =
            db::create_conversation(&conn, CHAT_TENANT_PERSONAL, CHAT_SCOPE_NOTE, &note.id, "note")
                .unwrap();
        // Simulate a breadth stored while the note still had a folder, then lost it.
        db::set_conversation_breadth(&conn, &conv.id, "folder").unwrap();

        let effective = heal_and_read_breadth(&conn, &conv.id, "folder", &note).unwrap();
        assert_eq!(effective, "note", "stale folder breadth heals to note");
        // The heal is persisted, so the chip and the request never diverge.
        let reloaded =
            db::latest_conversation(&conn, CHAT_TENANT_PERSONAL, CHAT_SCOPE_NOTE, &note.id)
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
            db::create_conversation(&conn, CHAT_TENANT_PERSONAL, CHAT_SCOPE_NOTE, &note.id, "note")
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

    /// A user turn (for tests that need a session to have messages).
    fn add_user_message(conn: &rusqlite::Connection, conversation_id: &str, text: &str) {
        db::insert_chat_message(conn, conversation_id, "user", &chat::text_parts_json("b", text))
            .unwrap();
    }

    #[test]
    fn first_session_defaults_breadth_to_note() {
        let dir = tempfile::tempdir().unwrap();
        let conn = db::open(&dir.path().join("first.sqlite")).unwrap();
        // A Note with no sessions → the first new session defaults to "note".
        let conv = new_personal_conversation(&conn, &ChatTarget::Note("n1".into())).unwrap();
        assert_eq!(conv.breadth, "note");
        assert_eq!(db::conversation_message_count(&conn, &conv.id).unwrap(), 0);
    }

    #[test]
    fn new_session_inherits_breadth_from_the_most_recent() {
        let dir = tempfile::tempdir().unwrap();
        let conn = db::open(&dir.path().join("inherit.sqlite")).unwrap();
        let first =
            db::create_conversation(&conn, CHAT_TENANT_PERSONAL, CHAT_SCOPE_NOTE, "n1", "note")
                .unwrap();
        db::set_conversation_breadth(&conn, &first.id, "all").unwrap();
        // The most-recent session has messages, so the guard doesn't reuse it.
        add_user_message(&conn, &first.id, "hi");

        let second = new_personal_conversation(&conn, &ChatTarget::Note("n1".into())).unwrap();
        assert_ne!(second.id, first.id, "a genuinely new session was created");
        assert_eq!(second.breadth, "all", "breadth is inherited from the most recent session");
    }

    #[test]
    fn new_chat_no_ops_when_the_most_recent_session_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let conn = db::open(&dir.path().join("noop.sqlite")).unwrap();
        let first =
            db::create_conversation(&conn, CHAT_TENANT_PERSONAL, CHAT_SCOPE_NOTE, "n1", "note")
                .unwrap();
        // No messages on the most-recent session → "new chat" returns it as-is.
        let again = new_personal_conversation(&conn, &ChatTarget::Note("n1".into())).unwrap();
        assert_eq!(again.id, first.id, "an empty most-recent session is reused, not duplicated");
        assert_eq!(
            db::list_conversations(&conn, CHAT_TENANT_PERSONAL, CHAT_SCOPE_NOTE, "n1", None, db::ListFilter::All).unwrap().len(),
            1,
            "no empty duplicate session was created"
        );
    }

    #[test]
    fn inherited_breadth_resolves_per_tenant_for_a_workspace() {
        // The workspace create path inherits breadth the same way as personal,
        // scoped to the workspace tenant (issue #61 fix 2). "note" only when the
        // Note has no prior session in that tenant.
        let dir = tempfile::tempdir().unwrap();
        let conn = db::open(&dir.path().join("wsbreadth.sqlite")).unwrap();
        assert_eq!(inherited_breadth(&conn, "wsA", &ChatTarget::Note("n1".into())), "note", "first-ever session defaults to note");
        let conv =
            db::create_conversation(&conn, "wsA", CHAT_SCOPE_NOTE, "n1", "note").unwrap();
        db::set_conversation_breadth(&conn, &conv.id, "all").unwrap();
        assert_eq!(inherited_breadth(&conn, "wsA", &ChatTarget::Note("n1".into())), "all", "inherits the workspace session's breadth");
        // A different tenant's breadth doesn't leak across.
        assert_eq!(inherited_breadth(&conn, CHAT_TENANT_PERSONAL, &ChatTarget::Note("n1".into())), "note");
    }

    /// A library-wide pane opens on an unsaved draft, a Note's pane resumes that
    /// Note's most-recent thread (#120, the divergence chosen deliberately).
    ///
    /// `resolve_for_read` is what the three no-id read commands share, so this one
    /// test covers history, breadth and the authorship pin at once.
    #[test]
    fn a_library_wide_pane_opens_on_a_draft_while_a_note_resumes_its_thread() {
        let dir = tempfile::tempdir().unwrap();
        let conn = db::open(&dir.path().join("draft.sqlite")).unwrap();

        let global = ChatTarget::Global;
        let note = ChatTarget::Note("n1".into());
        let g = db::create_conversation(
            &conn,
            CHAT_TENANT_PERSONAL,
            global.scope(),
            global.scope_id(),
            "all",
        )
        .unwrap();
        db::insert_chat_message(&conn, &g.id, "user", "[]").unwrap();
        let n = db::create_conversation(
            &conn,
            CHAT_TENANT_PERSONAL,
            note.scope(),
            note.scope_id(),
            "note",
        )
        .unwrap();
        db::insert_chat_message(&conn, &n.id, "user", "[]").unwrap();

        // No id + library-wide → nothing resolved, so the pane starts empty even
        // though a finished thread exists. This is the bug #120 was filed for.
        assert!(
            resolve_for_read(&conn, CHAT_TENANT_PERSONAL, &global, None).unwrap().is_none(),
            "a library-wide pane must not reopen the last thread"
        );
        // No id + a Note → still resumes, unchanged.
        assert_eq!(
            resolve_for_read(&conn, CHAT_TENANT_PERSONAL, &note, None).unwrap().map(|c| c.id),
            Some(n.id),
            "a Note's pane continues its own thread"
        );
        // An EXPLICIT id is honoured for both — picking a thread from the sidebar
        // must still open it, or the list would be unusable.
        assert_eq!(
            resolve_for_read(&conn, CHAT_TENANT_PERSONAL, &global, Some(&g.id))
                .unwrap()
                .map(|c| c.id),
            Some(g.id.clone()),
            "an explicitly chosen library-wide thread still opens"
        );
    }

    /// A library-wide draft's first turn starts a NEW conversation instead of
    /// appending to the last one (#120).
    ///
    /// The sharpest failure this prevents: with the pane showing an empty draft, a
    /// send that resolved to "most recent" would file the message into a finished
    /// thread the user believed they had left.
    #[test]
    fn a_library_wide_first_turn_never_appends_to_the_previous_thread() {
        let dir = tempfile::tempdir().unwrap();
        let conn = db::open(&dir.path().join("draftsend.sqlite")).unwrap();
        let global = ChatTarget::Global;

        let previous = db::create_conversation(
            &conn,
            CHAT_TENANT_PERSONAL,
            global.scope(),
            global.scope_id(),
            "all",
        )
        .unwrap();
        db::insert_chat_message(&conn, &previous.id, "user", "[]").unwrap();

        let fresh =
            resolve_or_create(&conn, CHAT_TENANT_PERSONAL, &global, None, DraftSettings::default())
                .unwrap();
        assert_ne!(fresh.id, previous.id, "a draft's turn opens its own conversation");

        // A Note's bare send still continues its thread — the resume rule is the
        // one thing deciding both, so they can't drift apart.
        let note = ChatTarget::Note("n1".into());
        let existing =
            db::create_conversation(&conn, CHAT_TENANT_PERSONAL, note.scope(), note.scope_id(), "note")
                .unwrap();
        db::insert_chat_message(&conn, &existing.id, "user", "[]").unwrap();
        let resolved =
            resolve_or_create(&conn, CHAT_TENANT_PERSONAL, &note, None, DraftSettings::default())
                .unwrap();
        assert_eq!(resolved.id, existing.id, "a Note's bare send continues its thread");
    }

    /// An empty pin string is normalised to "no pin" rather than stored (#120).
    ///
    /// The wire carries `""` for "off" in places, and a row storing `""` must read
    /// identically to one that never had a pin — otherwise "pinned to nobody"
    /// becomes a state that retrieval has to have an opinion about.
    #[test]
    fn a_drafts_blank_pin_is_no_pin_at_all() {
        let dir = tempfile::tempdir().unwrap();
        let conn = db::open(&dir.path().join("blankpin.sqlite")).unwrap();
        let created = resolve_or_create(
            &conn,
            "wsA",
            &ChatTarget::Global,
            None,
            DraftSettings { breadth: None, owner_filter: Some("   ".into()) },
        )
        .unwrap();
        assert_eq!(created.owner_filter, "", "whitespace is not a pin");
    }

    /// Settings chosen on a draft — before any row exists — are applied to the
    /// conversation its first turn creates (#120, preserving #61 and #103).
    ///
    /// This is what replaces the old lazy-create: the row appears once, already
    /// carrying what the chips showed, so there is never a moment where a stored
    /// row and the visible chip disagree.
    #[test]
    fn a_drafts_breadth_and_pin_reach_the_conversation_its_turn_creates() {
        let dir = tempfile::tempdir().unwrap();
        let conn = db::open(&dir.path().join("draftsettings.sqlite")).unwrap();
        let global = ChatTarget::Global;

        let created = resolve_or_create(
            &conn,
            "wsA",
            &global,
            None,
            DraftSettings { breadth: Some("all".into()), owner_filter: Some("u7".into()) },
        )
        .unwrap();
        assert_eq!(created.breadth, "all");
        assert_eq!(
            created.owner_filter, "u7",
            "a pin set before the first turn binds that turn — the #103 guarantee"
        );

        // Absent settings fall back to inheritance, so "+" keeps behaving as it did.
        let plain =
            resolve_or_create(&conn, "wsB", &global, None, DraftSettings::default()).unwrap();
        assert_eq!(plain.breadth, "all");
        assert_eq!(plain.owner_filter, "", "no pin unless one was chosen");
    }

    #[test]
    fn resolve_existing_rejects_a_cross_scope_conversation_id() {
        // An explicit id from another Note/tenant must be a clean error, never a
        // silent cross-scope operation (issue #61 fix 3).
        let dir = tempfile::tempdir().unwrap();
        let conn = db::open(&dir.path().join("xscope.sqlite")).unwrap();
        let n1 = db::create_conversation(&conn, CHAT_TENANT_PERSONAL, CHAT_SCOPE_NOTE, "n1", "note").unwrap();

        // Matching (tenant, note) resolves.
        let ok = resolve_existing(&conn, CHAT_TENANT_PERSONAL, &ChatTarget::Note("n1".into()), Some(&n1.id)).unwrap();
        assert_eq!(ok.unwrap().id, n1.id);
        // Wrong note → error.
        assert!(resolve_existing(&conn, CHAT_TENANT_PERSONAL, &ChatTarget::Note("n2".into()), Some(&n1.id)).is_err());
        // Wrong tenant → error.
        assert!(resolve_existing(&conn, "wsA", &ChatTarget::Note("n1".into()), Some(&n1.id)).is_err());
        // An unknown id is not an error — falls back to the active session (None).
        assert!(resolve_existing(&conn, CHAT_TENANT_PERSONAL, &ChatTarget::Note("n1".into()), Some("missing")).unwrap().is_none());
    }

    #[test]
    fn unlisted_legacy_handles_unions_only_the_server_omitted_remote_ids() {
        // Legacy-merge core (issue #61 fix 5): surface local handles whose
        // remote_id the server GET omitted, deduped by remote_id; skip handles
        // already in the server list and handles with no remote_id (never sent).
        fn handle(id: &str, remote: Option<&str>) -> db::Conversation {
            db::Conversation {
                id: id.to_string(),
                scope: CHAT_SCOPE_NOTE.to_string(),
                scope_id: "n1".to_string(),
                tenant: "wsA".to_string(),
                remote_id: remote.map(String::from),
                breadth: "note".to_string(),
                owner_filter: String::new(),
                title: None,
                created_at: 0,
                updated_at: 0,
            }
        }
        let local = vec![
            handle("h_srv", Some("srv1")),  // already in server list → skip
            handle("h_legacy", Some("srv2")), // server omitted → surface
            handle("h_unsent", None),        // never sent (no remote_id) → skip
        ];
        let server_ids: std::collections::HashSet<String> = ["srv1".to_string()].into_iter().collect();
        let unlisted = unlisted_legacy_handles(&server_ids, &local);
        assert_eq!(unlisted.len(), 1, "only the server-omitted, remote-id'd handle");
        assert_eq!(unlisted[0].id, "h_legacy");

        // Empty server list → every remote-id'd handle is legacy; the unsent one
        // is still skipped.
        let none: std::collections::HashSet<String> = std::collections::HashSet::new();
        let unlisted = unlisted_legacy_handles(&none, &local);
        let ids: Vec<&str> = unlisted.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(ids, vec!["h_srv", "h_legacy"]);
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
            Some("roundtrip-anchor"),
            None,
            None,
            None,
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
