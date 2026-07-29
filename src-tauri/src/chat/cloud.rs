//! Cloud (Teams) chat wiring (issue #50). For a WORKSPACE-tenant conversation
//! the desktop delegates the turn to the deployed humla-cloud `POST /api/chat`
//! endpoint, streams its SSE response, and re-emits it onto the SAME `chat_*`
//! events the local loop uses — so the Chat UI is identical across tenants
//! (story 18). Workspace conversations are server-authoritative; the desktop
//! only displays what the server returns.
//!
//! This module holds the Tauri-free pieces — request-body assembly, SSE frame
//! parsing, the server→`chat_*` event-name mapping, and the reason-code →
//! user-message mapping — so they're unit-testable. The streaming orchestration
//! (reqwest + emit + remote-id persistence) lives in `commands::chat`.

use serde::Serialize;
use serde_json::{json, Value};

/// Build the JSON body for `POST /api/chat`. `remote_conversation_id` is the
/// server's conversation record id (None/empty on the first turn → the server
/// creates one and returns it). The anchor `note_id` is always sent inside
/// `scope` for grounding; `folder_id` rides only when the breadth is folder.
///
/// The breadth is threaded through *verbatim* — no silent clamp (issue #58).
/// It's validated through the shared `super::validate_breadth` (the single owner
/// of the "Unrecognized chat scope" message), so an unknown value errors there;
/// a "folder" breadth without a folder_id is a distinct error (the desktop's
/// `heal_and_read_breadth` heals the folder-less case to "note" upstream, so
/// that arm is a strict guard that shouldn't fire in practice). This is what
/// lets breadth "all" reach the server as `scope.breadth == "all"` instead of
/// degrading to the anchor note.
pub fn build_cloud_request(
    remote_conversation_id: Option<&str>,
    workspace_id: &str,
    message: &str,
    title: Option<&str>,
    breadth: &str,
    note_id: Option<&str>,
    folder_id: Option<&str>,
    // A pinned authorship filter (#103): `(user_id, display_name)`. The id is
    // what the server filters on; the name only reaches the prompt, so the model
    // can say whose notes it was restricted to. Absent = no filter.
    owner: Option<(&str, &str)>,
    // Display names for the ACTIVE BREADTH (#113): the anchor note's title and the
    // folder's name. Prompt text only — never filter inputs, exactly like
    // `owner`'s second element. The server needs them to disclose what the turn is
    // confined to, and naming an id the user has never seen would be worse than
    // silence. Absent just loses the disclosure.
    note_title: Option<&str>,
    folder_name: Option<&str>,
) -> Result<Value, String> {
    // Reject an unknown breadth through the shared validator — one owner of the
    // vocabulary and its error, no duplicated match/message here.
    super::validate_breadth(breadth)?;
    // A library-wide turn (#93) sends NO `note_id` at all: the server 400s a
    // malformed `note_id` under *every* breadth including "all", so a sentinel or
    // an empty string would be rejected rather than ignored (humla-cloud#26).
    // An EMPTY anchor is a caller bug, not "absent" — `ChatTarget::from_note_id`
    // rejects it at the IPC boundary and this is the matching strictness, so the
    // same value can't mean two things at two layers.
    if note_id.is_some_and(|s| s.trim().is_empty()) {
        return Err("An empty note id is not a valid chat anchor.".into());
    }
    let anchor = note_id;
    // The anchor-less rule lives in `chat::check_anchor` — one owner, one message.
    super::check_anchor(breadth, anchor.is_some())?;
    // When present, `note_id` stays in every scope so the server can ground on the
    // anchor Note regardless of breadth (under "all" the server ignores it by
    // design).
    let with_anchor = |mut v: Value| -> Value {
        if let Some(id) = anchor {
            v["note_id"] = json!(id);
        }
        v
    };
    // The pin rides the scope, beside the breadth clamp it behaves like: both are
    // the user's stated intent, and both bind regardless of what the model asks
    // for. The display name travels separately from the id it names — one is a
    // filter input, the other is only ever prompt text.
    let with_owner = |mut v: Value| {
        if let Some((id, name)) = owner.filter(|(id, _)| !id.trim().is_empty()) {
            v["owner"] = json!(id);
            if !name.trim().is_empty() {
                v["owner_name"] = json!(name);
            }
        }
        v
    };
    // Only the name the BREADTH uses is sent. Under "all" neither goes — the anchor
    // rides along to resolve the conversation, so sending its title would invite the
    // server to announce a one-note confinement on a library-wide turn.
    let with_reach_name = |mut v: Value| {
        let name = match breadth {
            "note" => note_title,
            "folder" => folder_name,
            _ => None,
        };
        if let Some(n) = name.map(str::trim).filter(|n| !n.is_empty()) {
            v[if breadth == "note" { "note_title" } else { "folder_name" }] = json!(n);
        }
        v
    };
    let scope = match breadth {
        "note" => with_reach_name(with_owner(with_anchor(json!({ "breadth": "note" })))),
        "all" => with_owner(with_anchor(json!({ "breadth": "all" }))),
        "folder" => match folder_id.filter(|f| !f.is_empty()) {
            Some(f) => with_reach_name(with_owner(with_anchor(
                json!({ "breadth": "folder", "folder_id": f }),
            ))),
            None => {
                return Err(
                    "\"Folder\" scope needs a folder, but this note isn't in one.".into()
                )
            }
        },
        // Every value `validate_breadth` accepts is handled above. A value that
        // passes validation but lands here means the vocabulary grew without
        // this builder keeping up — fail loudly (a distinct message, not the
        // validator's), never `unreachable!()` that could mask the gap.
        other => {
            return Err(format!(
                "chat scope {other:?} is valid but not handled by the cloud request builder"
            ))
        }
    };
    let mut body = json!({
        "workspace_id": workspace_id,
        "message": message,
        "scope": scope,
    });
    if let Some(id) = remote_conversation_id.filter(|s| !s.is_empty()) {
        body["conversation_id"] = json!(id);
    }
    if let Some(t) = title.filter(|s| !s.is_empty()) {
        body["title"] = json!(t);
    }
    Ok(body)
}

/// Parse one SSE event block into `(event name, data payload)`. Matches the
/// framing hono's `streamSSE` emits — `event: <name>\ndata: <json>` — tolerating
/// an optional single space after each colon and multi-line `data:` folds.
/// Returns None for a block with no `event:` line (e.g. a lone comment/keepalive).
pub fn parse_sse_frame(block: &str) -> Option<(String, String)> {
    let mut event = None;
    let mut data = String::new();
    for line in block.lines() {
        if let Some(v) = line.strip_prefix("event:") {
            event = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("data:") {
            data.push_str(v.strip_prefix(' ').unwrap_or(v));
        }
    }
    event.map(|e| (e, data))
}

/// Byte index + length of the first SSE event terminator (`\n\n` or `\r\n\r\n`)
/// in `buf`, or None if no complete event has arrived yet. Picks whichever
/// terminator appears first, so a proxy that rewrites line endings still frames
/// correctly. Operates on BYTES (not `&str`) so the caller can buffer raw
/// network chunks and decode only *complete* frames — a multibyte codepoint
/// split across two chunks is never lossily decoded mid-character. The caller
/// drains `idx + len` bytes and decodes `buf[..idx]`.
pub fn find_event_end(buf: &[u8]) -> Option<(usize, usize)> {
    let lf = buf.windows(2).position(|w| w == b"\n\n").map(|i| (i, 2));
    let crlf = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| (i, 4));
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

/// Extract a human answer from a non-streamed whole-turn body (the AC2 degraded
/// fallback, when a 2xx response isn't an SSE stream). Tries the obvious text
/// fields, then falls back to the raw body so the UI shows *something* and
/// completes rather than hanging on a stream that never frames.
pub fn whole_turn_answer(body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<Value>(body) {
        for key in ["answer", "message", "text", "content"] {
            if let Some(s) = v.get(key).and_then(|x| x.as_str()) {
                if !s.trim().is_empty() {
                    return s.to_string();
                }
            }
        }
    }
    body.trim().to_string()
}

/// Map a server SSE event name to the Tauri `chat_*` event the frontend already
/// listens on. Unknown names are ignored (None) so a future server event can't
/// crash an older client.
pub fn tauri_event_for(server_event: &str) -> Option<&'static str> {
    match server_event {
        "text_delta" => Some("chat_text_delta"),
        "tool_activity" => Some("chat_tool_activity"),
        "citations" => Some("chat_citations"),
        "done" => Some("chat_done"),
        "error" => Some("chat_error"),
        _ => None,
    }
}

/// Turn a cloud preflight error (`{reason, error}`) into a clear user-facing
/// message. Subscription/quota blocks name the upgrade path (stories 21/22):
/// a lapsed subscription points at resubscribing, an exhausted quota at the
/// chat-capacity add-on. Everything else passes the server's own message
/// through, falling back to a generic line when it's empty.
pub fn cloud_chat_error_message(reason: &str, server_message: &str) -> String {
    match reason {
        "subscription_lapsed" => {
            "This workspace needs an active subscription to use Team chat — ask the workspace \
             owner to resubscribe. (Personal chat stays free.)"
                .to_string()
        }
        "quota_exhausted" => {
            "This workspace has used its Team chat allowance for the billing period. The \
             chat-capacity add-on raises the limit."
                .to_string()
        }
        "not_a_member" => "You're not a member of this workspace.".to_string(),
        // BYOK-first reasons (issue #76). Distinct from the managed add-on's
        // `quota_exhausted` upsell above. The client renders role-aware copy for
        // these when it has the reason code; these strings are the fallback and
        // the send-rejection text.
        "chat_not_activated" => {
            "Workspace chat isn't activated yet — the owner can turn it on in workspace settings."
                .to_string()
        }
        "byok_key_invalid" => {
            "This workspace's OpenAI key was rejected — the owner can re-enter it in workspace \
             settings."
                .to_string()
        }
        "byok_provider_quota" => {
            "This workspace's OpenAI account is out of quota — the owner can check it in workspace \
             settings."
                .to_string()
        }
        "byok_key_unavailable" => {
            "Workspace chat is temporarily unavailable — try again shortly.".to_string()
        }
        "chat_disabled" | "chat_unavailable" | "index_unavailable" => {
            "Team chat isn't available on this server.".to_string()
        }
        _ => {
            let m = server_message.trim();
            if m.is_empty() {
                "Team chat failed — please try again.".to_string()
            } else {
                m.to_string()
            }
        }
    }
}

/// The workspace turn allowance shown in the composer (issue #69). Serialized
/// camelCase for the frontend (`{ used, cap, periodEnd }`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageDto {
    pub used: i64,
    pub cap: i64,
    pub period_end: i64,
}

/// Map a 200 usage body to a display DTO, or None when there's nothing to show.
/// The meter is best-effort — it must never surface an error — so anything that
/// isn't a clean metered reading maps to None:
///   - `{ unmetered: true }`            → None (no allowance to display)
///   - error shapes `{ reason, … }`     → None (no used/cap fields)
///   - missing `used_turns`/`cap_turns` → None
///   - `{ unmetered:false, used_turns, cap_turns[, period_end] }` → Some
pub fn parse_usage(body: &Value) -> Option<UsageDto> {
    if body.get("unmetered").and_then(|v| v.as_bool()) == Some(true) {
        return None;
    }
    let used = body.get("used_turns").and_then(|v| v.as_i64())?;
    let cap = body.get("cap_turns").and_then(|v| v.as_i64())?;
    let period_end = body.get("period_end").and_then(|v| v.as_i64()).unwrap_or(0);
    Some(UsageDto { used, cap, period_end })
}

/// How a workspace's retrieval index looks to search, as the server reports it
/// (issue #102). The client needs this to keep the chat pane honest: an empty
/// local mirror is not evidence of an empty workspace when the server index is
/// still backfilling, and "No notes yet" is then a false claim about someone's
/// library.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum IndexState {
    /// Searchable — an empty result really does mean "nothing matched".
    Ready,
    /// Nothing to match yet: never indexed, or still backfilling.
    Empty,
    /// Chunks withheld inside the indexer's deactivation grace window.
    Quarantined,
}

/// Map a 200 index-state body to the enum, or None when there's nothing usable.
///
/// Best-effort like [`parse_usage`]: this only ever *improves* the pane's copy,
/// so an unknown state (a newer server growing a fourth value) or a malformed
/// body reads as "no information" and the caller falls back to its local guess.
/// Never an error — a hint must not be able to break the composer.
pub fn parse_index_state(body: &Value) -> Option<IndexState> {
    match body.get("index_state").and_then(|v| v.as_str())? {
        "ready" => Some(IndexState::Ready),
        "empty" => Some(IndexState::Empty),
        "quarantined" => Some(IndexState::Quarantined),
        _ => None,
    }
}

// ── Workspace chat key (BYOK, issue #75) ─────────────────────────────────────

/// Metadata about a workspace's OpenAI key, shown in workspace settings. The
/// key value itself is NEVER returned by the server (REST read rule is null) or
/// stored client-side — only this metadata. Serialized camelCase for the
/// frontend (`{ configured, last4, setBy, setAt, keyHealth }`).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatKeyMeta {
    pub configured: bool,
    /// Last 4 chars of the stored key (display only), when configured.
    pub last4: Option<String>,
    /// User id who set/rotated the key (resolved to a name in the UI).
    pub set_by: Option<String>,
    /// When it was set (server timestamp — string or number, passed through).
    pub set_at: Option<String>,
    /// "ok" | "failing" | … — drives the degradation warning.
    pub key_health: Option<String>,
}

fn str_field(body: &Value, key: &str) -> Option<String> {
    body.get(key).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_string)
}

/// A scalar (string or number) field as a string — set_at may be an RFC3339
/// string or an epoch number depending on the server; the UI formats it best-
/// effort, so pass whichever through as text.
fn scalar_field(body: &Value, key: &str) -> Option<String> {
    match body.get(key) {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

/// Map a chat-key set/meta/delete 200 body to the metadata DTO. `configured`
/// defaults false, so both `{configured:false,…}` and a malformed body read as
/// "not activated" — never an error (the caller surfaces genuine HTTP failures
/// separately).
pub fn parse_key_meta(body: &Value) -> ChatKeyMeta {
    ChatKeyMeta {
        configured: body.get("configured").and_then(|v| v.as_bool()).unwrap_or(false),
        last4: str_field(body, "last4"),
        set_by: str_field(body, "set_by"),
        set_at: scalar_field(body, "set_at"),
        key_health: str_field(body, "key_health"),
    }
}

/// Turn a chat-key preflight error (`{reason, error}`) into a short, sentence-
/// case user message (issue #75). Distinct from `cloud_chat_error_message` (turn
/// errors). NEVER includes the submitted key — it maps by reason code, and the
/// fallback only passes the server's own `error` text (which never carries the
/// key) or a generic line.
pub fn chat_key_error_message(reason: &str, server_message: &str) -> String {
    match reason {
        "byok_key_invalid" => "OpenAI rejected this key.".to_string(),
        "byok_validation_unreachable" => {
            "Couldn't reach OpenAI to validate — try again.".to_string()
        }
        "byok_validation_failed" => "Couldn't validate the key — try again.".to_string(),
        "key_encryption_unconfigured" => {
            "Server missing its key-encryption secret — contact support.".to_string()
        }
        "forbidden" => "Only the workspace owner can manage the chat key.".to_string(),
        "not_a_member" => "You're not a member of this workspace.".to_string(),
        "unauthorized" => "Please sign in again.".to_string(),
        "bad_request" => "That doesn't look like a valid key.".to_string(),
        _ => {
            let m = server_message.trim();
            if m.is_empty() {
                "Couldn't save the key — please try again.".to_string()
            } else {
                m.to_string()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_usage_maps_metered_some_and_everything_else_none() {
        // Metered → Some with the counts.
        assert_eq!(
            parse_usage(&json!({
                "unmetered": false, "used_turns": 2, "cap_turns": 3, "period_end": 1893456000
            })),
            Some(UsageDto { used: 2, cap: 3, period_end: 1893456000 })
        );
        // period_end optional → defaults to 0, still metered.
        assert_eq!(
            parse_usage(&json!({ "unmetered": false, "used_turns": 0, "cap_turns": 5 })),
            Some(UsageDto { used: 0, cap: 5, period_end: 0 })
        );
        // Unmetered → nothing to show.
        assert_eq!(parse_usage(&json!({ "unmetered": true })), None);
        // Error shapes carry no used/cap → None.
        assert_eq!(parse_usage(&json!({ "reason": "not_a_member", "error": "x" })), None);
        assert_eq!(parse_usage(&json!({ "reason": "subscription_lapsed" })), None);
        // Partial / malformed → None.
        assert_eq!(parse_usage(&json!({ "used_turns": 2 })), None);
        assert_eq!(parse_usage(&json!({})), None);
    }

    #[test]
    fn parse_index_state_maps_the_three_states_and_nothing_else() {
        for (raw, want) in [
            ("ready", IndexState::Ready),
            ("empty", IndexState::Empty),
            ("quarantined", IndexState::Quarantined),
        ] {
            assert_eq!(parse_index_state(&json!({ "index_state": raw })), Some(want));
        }
        // An unknown state — a newer server growing a fourth value — reads as "no
        // information" so the pane falls back to its local guess rather than
        // guessing wrong about a state it doesn't understand.
        assert_eq!(parse_index_state(&json!({ "index_state": "rebuilding" })), None);
        // Error shapes and malformed bodies carry no state.
        assert_eq!(parse_index_state(&json!({ "reason": "not_a_member" })), None);
        assert_eq!(parse_index_state(&json!({ "index_state": 3 })), None);
        assert_eq!(parse_index_state(&json!({})), None);
    }

    #[test]
    fn parse_key_meta_reads_configured_and_unconfigured_shapes() {
        // Configured (set success / meta) → all fields.
        let m = parse_key_meta(&json!({
            "configured": true, "last4": "n3Kq", "set_by": "u1",
            "set_at": "2026-07-24T10:00:00Z", "key_health": "ok"
        }));
        assert_eq!(
            m,
            ChatKeyMeta {
                configured: true,
                last4: Some("n3Kq".into()),
                set_by: Some("u1".into()),
                set_at: Some("2026-07-24T10:00:00Z".into()),
                key_health: Some("ok".into()),
            }
        );
        // Degraded health surfaces so the UI can warn.
        assert_eq!(
            parse_key_meta(&json!({ "configured": true, "last4": "ab12", "key_health": "failing" }))
                .key_health,
            Some("failing".into())
        );
        // Unconfigured / delete response → not activated, no error.
        assert_eq!(parse_key_meta(&json!({ "configured": false })), ChatKeyMeta::default());
        // set_at may be numeric (epoch) — passed through as text.
        assert_eq!(
            parse_key_meta(&json!({ "configured": true, "set_at": 1893456000 })).set_at,
            Some("1893456000".into())
        );
        // Malformed → not activated.
        assert_eq!(parse_key_meta(&json!({})), ChatKeyMeta::default());
    }

    #[test]
    fn chat_key_error_message_maps_reasons_without_leaking_the_key() {
        assert_eq!(chat_key_error_message("byok_key_invalid", ""), "OpenAI rejected this key.");
        assert!(chat_key_error_message("byok_validation_unreachable", "").contains("reach OpenAI"));
        assert!(chat_key_error_message("key_encryption_unconfigured", "").contains("contact support"));
        assert!(chat_key_error_message("forbidden", "").to_lowercase().contains("owner"));
        // Unknown reason falls back to the server's own message (never the key).
        assert_eq!(chat_key_error_message("weird", "server said no"), "server said no");
        assert!(!chat_key_error_message("weird", "   ").is_empty());
    }

    #[test]
    fn request_defaults_to_note_breadth_with_grounding_anchor() {
        let b = build_cloud_request(None, "ws1", "hi", None, "note", Some("n1"), None, None, None, None).unwrap();
        assert_eq!(b["workspace_id"], "ws1");
        assert_eq!(b["message"], "hi");
        assert_eq!(b["scope"], json!({ "breadth": "note", "note_id": "n1" }));
        assert!(b.get("conversation_id").is_none(), "no remote id on the first turn");
        assert!(b.get("title").is_none());
    }

    /// #93: a library-wide turn OMITS `note_id` entirely. The server 400s a
    /// malformed `note_id` under *every* breadth including "all" (humla-cloud#26),
    /// so a sentinel or an empty string would be rejected rather than ignored —
    /// the key has to be absent.
    #[test]
    fn a_library_wide_request_sends_no_anchor_at_all() {
        let b = build_cloud_request(None, "ws1", "hi", None, "all", None, None, None, None, None).unwrap();
        assert_eq!(b["scope"], json!({ "breadth": "all" }));
        assert!(
            b["scope"].get("note_id").is_none(),
            "an absent anchor must not become a key at all"
        );
        // An EMPTY anchor is a caller bug, not "absent" — the same strictness
        // `ChatTarget::from_note_id` applies at the IPC boundary, so `""` can't
        // mean "a note" at one layer and "the library" at another.
        let err = build_cloud_request(None, "ws1", "hi", None, "all", Some(""), None, None, None, None).unwrap_err();
        assert!(err.contains("empty note id"), "got: {err}");
    }

    /// A note-less create must be breadth "all" — a note-less "folder" is a 400
    /// server-side, so catch it here as our bug rather than as a failed turn.
    #[test]
    fn a_request_without_an_anchor_must_be_all_breadth() {
        for breadth in ["note", "folder"] {
            let err = build_cloud_request(None, "ws1", "hi", None, breadth, None, Some("f1"), None, None, None)
                .unwrap_err();
            assert!(err.contains("needs an anchor note"), "{breadth}: {err}");
        }
        // With an anchor, both still build as before.
        assert!(build_cloud_request(None, "ws1", "hi", None, "note", Some("n1"), None, None, None, None).is_ok());
        assert!(
            build_cloud_request(None, "ws1", "hi", None, "folder", Some("n1"), Some("f1"), None, None, None).is_ok()
        );
    }

    #[test]
    fn request_carries_remote_id_and_title_when_present() {
        let b =
            build_cloud_request(Some("srv123"), "ws1", "hi", Some("Kickoff"), "note", Some("n1"), None, None, None, None)
                .unwrap();
        assert_eq!(b["conversation_id"], "srv123");
        assert_eq!(b["title"], "Kickoff");
        // Empty strings are dropped, not sent.
        let b2 = build_cloud_request(Some(""), "ws1", "hi", Some(""), "note", Some("n1"), None, None, None, None).unwrap();
        assert!(b2.get("conversation_id").is_none());
        assert!(b2.get("title").is_none());
    }

    #[test]
    fn request_folder_breadth_carries_the_folder_id() {
        let with =
            build_cloud_request(None, "ws1", "hi", None, "folder", Some("n1"), Some("f1"), None, None, None).unwrap();
        assert_eq!(with["scope"], json!({ "breadth": "folder", "note_id": "n1", "folder_id": "f1" }));
    }

    #[test]
    fn request_folder_breadth_without_a_folder_is_an_error_not_a_clamp() {
        // Issue #58: no silent degradation. The desktop heals the folder-less
        // case to "note" upstream, so this strict guard shouldn't fire — but if
        // it's reached it errors loudly rather than quietly narrowing to note.
        let err = build_cloud_request(None, "ws1", "hi", None, "folder", Some("n1"), None, None, None, None).unwrap_err();
        assert!(err.to_lowercase().contains("folder"), "names the missing folder: {err}");
    }

    #[test]
    fn request_unrecognized_breadth_is_an_error() {
        // Issue #58: the former `_ => {}` fall-through silently clamped to note.
        let err = build_cloud_request(None, "ws1", "hi", None, "everything", Some("n1"), None, None, None, None).unwrap_err();
        assert!(err.contains("everything"), "the offending value is surfaced: {err}");
    }

    #[test]
    fn request_all_breadth_keeps_the_grounding_anchor() {
        let b = build_cloud_request(None, "ws1", "hi", None, "all", Some("n1"), None, None, None, None).unwrap();
        assert_eq!(b["scope"], json!({ "breadth": "all", "note_id": "n1" }));
    }

    /// #113's display names ride the scope so the server can DISCLOSE what the turn
    /// is confined to. Prompt text only, never filter inputs — the same split
    /// `owner`/`owner_name` already makes.
    #[test]
    fn request_sends_only_the_display_name_its_breadth_actually_uses() {
        let note = build_cloud_request(
            None, "ws1", "hi", None, "note", Some("n1"), None, None, Some("Kickoff"), Some("K2 pilot"),
        )
        .unwrap();
        assert_eq!(note["scope"], json!({ "breadth": "note", "note_id": "n1", "note_title": "Kickoff" }));

        let folder = build_cloud_request(
            None, "ws1", "hi", None, "folder", Some("n1"), Some("f1"), None, Some("Kickoff"), Some("K2 pilot"),
        )
        .unwrap();
        assert_eq!(
            folder["scope"],
            json!({ "breadth": "folder", "note_id": "n1", "folder_id": "f1", "folder_name": "K2 pilot" })
        );

        // Under `all`, NEITHER name is sent. The anchor rides along to resolve the
        // conversation, so sending its title would invite the server to announce a
        // one-note confinement on a library-wide turn — a filter that isn't running,
        // which is the same lie as hiding one that is.
        let all = build_cloud_request(
            None, "ws1", "hi", None, "all", Some("n1"), None, None, Some("Kickoff"), Some("K2 pilot"),
        )
        .unwrap();
        assert_eq!(all["scope"], json!({ "breadth": "all", "note_id": "n1" }));

        // A blank name is omitted rather than sent empty: the server would otherwise
        // have to decide whether `""` means "no narrowing" or "a folder with no name".
        let blank = build_cloud_request(
            None, "ws1", "hi", None, "folder", Some("n1"), Some("f1"), None, None, Some("   "),
        )
        .unwrap();
        assert_eq!(blank["scope"], json!({ "breadth": "folder", "note_id": "n1", "folder_id": "f1" }));
    }

    /// The authorship pin (#103) rides the scope under every breadth — it says
    /// WHOSE notes are in reach, which composes with WHAT is, rather than being a
    /// fourth breadth.
    #[test]
    fn request_carries_the_authorship_pin_under_every_breadth() {
        let owner = Some(("u-anna", "Anna"));
        let note = build_cloud_request(None, "ws1", "hi", None, "note", Some("n1"), None, owner, None, None).unwrap();
        assert_eq!(note["scope"], json!({ "breadth": "note", "note_id": "n1", "owner": "u-anna", "owner_name": "Anna" }));
        let all = build_cloud_request(None, "ws1", "hi", None, "all", None, None, owner, None, None).unwrap();
        assert_eq!(all["scope"], json!({ "breadth": "all", "owner": "u-anna", "owner_name": "Anna" }));
        let folder =
            build_cloud_request(None, "ws1", "hi", None, "folder", Some("n1"), Some("f1"), owner, None, None).unwrap();
        assert_eq!(
            folder["scope"],
            json!({ "breadth": "folder", "note_id": "n1", "folder_id": "f1", "owner": "u-anna", "owner_name": "Anna" }),
        );
    }

    /// The ID filters; the NAME is only the prompt's disclosure wording. So an
    /// unresolvable person (a removed member, a roster that hasn't loaded) must
    /// still pin — losing the name, never the filter.
    #[test]
    fn a_pin_without_a_resolvable_name_still_filters() {
        let b = build_cloud_request(None, "ws1", "hi", None, "all", None, None, Some(("u-anna", "")), None, None).unwrap();
        assert_eq!(b["scope"], json!({ "breadth": "all", "owner": "u-anna" }));
    }

    /// The inverse: a name with no id behind it is not a pin. Sending `owner_name`
    /// alone would make the server disclose a filter that isn't running.
    #[test]
    fn a_blank_owner_id_is_no_pin_at_all() {
        for owner in [Some(("", "Anna")), Some(("   ", "Anna")), None] {
            let b = build_cloud_request(None, "ws1", "hi", None, "all", None, None, owner, None, None).unwrap();
            assert_eq!(b["scope"], json!({ "breadth": "all" }), "{owner:?}");
        }
    }

    #[test]
    fn parses_sse_event_and_data() {
        let (ev, data) = parse_sse_frame("event: text_delta\ndata: {\"delta\":\"hi\"}").unwrap();
        assert_eq!(ev, "text_delta");
        assert_eq!(data, "{\"delta\":\"hi\"}");
    }

    #[test]
    fn sse_block_without_event_is_ignored() {
        assert!(parse_sse_frame(": keepalive comment").is_none());
        assert!(parse_sse_frame("data: {\"x\":1}").is_none());
    }

    #[test]
    fn error_frame_relays_the_top_level_reason_to_the_client() {
        // The server's mid-turn SSE error frame carries a top-level `reason`
        // (byok_key_invalid / byok_provider_quota). Framing → "error" → chat_error,
        // and JSON parse keeps `reason` so the client's chat_error payload (which
        // the stream pump re-emits verbatim, only re-stamping conversationId) can
        // drive the role-aware BYOK error copy (#76).
        let (event, data) = parse_sse_frame(
            "event: error\ndata: {\"reason\":\"byok_key_invalid\",\"message\":\"nope\"}",
        )
        .unwrap();
        assert_eq!(event, "error");
        assert_eq!(tauri_event_for(&event), Some("chat_error"));
        let v: Value = serde_json::from_str(&data).unwrap();
        assert_eq!(v.get("reason").and_then(|r| r.as_str()), Some("byok_key_invalid"));
    }

    #[test]
    fn finds_the_first_event_terminator() {
        assert_eq!(find_event_end(b"event: a\ndata: 1\n\nrest"), Some((16, 2)));
        // CRLF framing.
        assert_eq!(find_event_end(b"event: a\r\ndata: 1\r\n\r\nrest"), Some((17, 4)));
        // Incomplete — no blank line yet.
        assert_eq!(find_event_end(b"event: a\ndata: 1\n"), None);
    }

    #[test]
    fn only_complete_frames_are_decoded_so_split_utf8_survives() {
        // A Norwegian "å" (0xC3 0xA5) split across two network chunks. Framing on
        // bytes means we only decode once the whole frame (and codepoint) arrived.
        let mut buf: Vec<u8> = Vec::new();
        buf.extend_from_slice("event: text_delta\ndata: {\"delta\":\"m\u{00e5}".as_bytes());
        let split = buf.len() - 1; // land mid-codepoint
        let (head, tail) = buf.split_at(split);
        assert!(find_event_end(head).is_none(), "no complete frame yet");
        let mut acc = head.to_vec();
        acc.extend_from_slice(tail);
        acc.extend_from_slice("te\"}\n\n".as_bytes());
        let (idx, delim) = find_event_end(&acc).expect("frame complete");
        let text = String::from_utf8_lossy(&acc[..idx]);
        let (_, data) = parse_sse_frame(&text).unwrap();
        assert!(data.contains("m\u{00e5}te"), "å survived the chunk split: {data}");
        assert_eq!(delim, 2);
    }

    #[test]
    fn whole_turn_answer_prefers_a_text_field_then_falls_back_to_raw() {
        assert_eq!(whole_turn_answer(r#"{"answer":"hi there"}"#), "hi there");
        assert_eq!(whole_turn_answer(r#"{"message":"yo"}"#), "yo");
        assert_eq!(whole_turn_answer("plain text body"), "plain text body");
    }

    #[test]
    fn maps_server_events_to_chat_events() {
        assert_eq!(tauri_event_for("text_delta"), Some("chat_text_delta"));
        assert_eq!(tauri_event_for("tool_activity"), Some("chat_tool_activity"));
        assert_eq!(tauri_event_for("citations"), Some("chat_citations"));
        assert_eq!(tauri_event_for("done"), Some("chat_done"));
        assert_eq!(tauri_event_for("error"), Some("chat_error"));
        assert_eq!(tauri_event_for("PB_CONNECT"), None);
    }

    #[test]
    fn error_messages_name_the_upgrade_path() {
        assert!(cloud_chat_error_message("subscription_lapsed", "x").contains("subscription"));
        let q = cloud_chat_error_message("quota_exhausted", "x");
        assert!(q.contains("add-on"), "quota block names the chat add-on: {q}");
        assert_eq!(cloud_chat_error_message("weird_reason", "raw server text"), "raw server text");
        assert!(!cloud_chat_error_message("weird_reason", "  ").is_empty());
    }

    #[test]
    fn byok_reasons_map_to_distinct_messages_not_the_addon_upsell() {
        let na = cloud_chat_error_message("chat_not_activated", "");
        let invalid = cloud_chat_error_message("byok_key_invalid", "");
        let quota = cloud_chat_error_message("byok_provider_quota", "");
        let unavail = cloud_chat_error_message("byok_key_unavailable", "");
        // Each is distinct and non-generic.
        for m in [&na, &invalid, &quota, &unavail] {
            assert_ne!(*m, "Team chat failed — please try again.");
        }
        assert!(na.to_lowercase().contains("activated"));
        assert!(invalid.to_lowercase().contains("rejected"));
        assert!(quota.to_lowercase().contains("quota"));
        assert!(unavail.to_lowercase().contains("try again"));
        // None of them reuse the managed add-on's upsell copy.
        for m in [&na, &invalid, &quota, &unavail] {
            assert!(!m.contains("add-on"), "BYOK reason must not name the managed add-on: {m}");
        }
    }
}
