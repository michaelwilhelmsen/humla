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

use serde_json::{json, Value};

/// Build the JSON body for `POST /api/chat`. `remote_conversation_id` is the
/// server's conversation record id (None/empty on the first turn → the server
/// creates one and returns it). The anchor `note_id` is always sent inside
/// `scope` for grounding; `folder_id` rides only when the breadth is folder.
/// Mirrors the desktop breadth clamp: a folder-less note under "folder" breadth
/// falls back to "note".
pub fn build_cloud_request(
    remote_conversation_id: Option<&str>,
    workspace_id: &str,
    message: &str,
    title: Option<&str>,
    breadth: &str,
    note_id: &str,
    folder_id: Option<&str>,
) -> Value {
    // Default to the safe single-Note breadth; widen only on an explicit,
    // well-formed request. `note_id` stays in every scope so the server can
    // ground on the anchor Note regardless of breadth.
    let mut scope = json!({ "breadth": "note", "note_id": note_id });
    match breadth {
        "all" => scope = json!({ "breadth": "all", "note_id": note_id }),
        "folder" => {
            if let Some(f) = folder_id.filter(|f| !f.is_empty()) {
                scope = json!({ "breadth": "folder", "note_id": note_id, "folder_id": f });
            }
        }
        _ => {}
    }
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
    body
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_defaults_to_note_breadth_with_grounding_anchor() {
        let b = build_cloud_request(None, "ws1", "hi", None, "note", "n1", None);
        assert_eq!(b["workspace_id"], "ws1");
        assert_eq!(b["message"], "hi");
        assert_eq!(b["scope"], json!({ "breadth": "note", "note_id": "n1" }));
        assert!(b.get("conversation_id").is_none(), "no remote id on the first turn");
        assert!(b.get("title").is_none());
    }

    #[test]
    fn request_carries_remote_id_and_title_when_present() {
        let b = build_cloud_request(Some("srv123"), "ws1", "hi", Some("Kickoff"), "note", "n1", None);
        assert_eq!(b["conversation_id"], "srv123");
        assert_eq!(b["title"], "Kickoff");
        // Empty strings are dropped, not sent.
        let b2 = build_cloud_request(Some(""), "ws1", "hi", Some(""), "note", "n1", None);
        assert!(b2.get("conversation_id").is_none());
        assert!(b2.get("title").is_none());
    }

    #[test]
    fn request_folder_breadth_needs_a_folder_else_falls_back_to_note() {
        let with = build_cloud_request(None, "ws1", "hi", None, "folder", "n1", Some("f1"));
        assert_eq!(with["scope"], json!({ "breadth": "folder", "note_id": "n1", "folder_id": "f1" }));
        // A folder-less note under folder breadth clamps to note (no folder to widen to).
        let without = build_cloud_request(None, "ws1", "hi", None, "folder", "n1", None);
        assert_eq!(without["scope"], json!({ "breadth": "note", "note_id": "n1" }));
    }

    #[test]
    fn request_all_breadth_keeps_the_grounding_anchor() {
        let b = build_cloud_request(None, "ws1", "hi", None, "all", "n1", None);
        assert_eq!(b["scope"], json!({ "breadth": "all", "note_id": "n1" }));
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
}
