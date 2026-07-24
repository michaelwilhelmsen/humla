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
    note_id: &str,
    folder_id: Option<&str>,
) -> Result<Value, String> {
    // Reject an unknown breadth through the shared validator — one owner of the
    // vocabulary and its error, no duplicated match/message here.
    super::validate_breadth(breadth)?;
    // `note_id` stays in every scope so the server can ground on the anchor
    // Note regardless of breadth (under "all" the server ignores it by design).
    let scope = match breadth {
        "note" => json!({ "breadth": "note", "note_id": note_id }),
        "all" => json!({ "breadth": "all", "note_id": note_id }),
        "folder" => match folder_id.filter(|f| !f.is_empty()) {
            Some(f) => json!({ "breadth": "folder", "note_id": note_id, "folder_id": f }),
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
    fn request_defaults_to_note_breadth_with_grounding_anchor() {
        let b = build_cloud_request(None, "ws1", "hi", None, "note", "n1", None).unwrap();
        assert_eq!(b["workspace_id"], "ws1");
        assert_eq!(b["message"], "hi");
        assert_eq!(b["scope"], json!({ "breadth": "note", "note_id": "n1" }));
        assert!(b.get("conversation_id").is_none(), "no remote id on the first turn");
        assert!(b.get("title").is_none());
    }

    #[test]
    fn request_carries_remote_id_and_title_when_present() {
        let b =
            build_cloud_request(Some("srv123"), "ws1", "hi", Some("Kickoff"), "note", "n1", None)
                .unwrap();
        assert_eq!(b["conversation_id"], "srv123");
        assert_eq!(b["title"], "Kickoff");
        // Empty strings are dropped, not sent.
        let b2 = build_cloud_request(Some(""), "ws1", "hi", Some(""), "note", "n1", None).unwrap();
        assert!(b2.get("conversation_id").is_none());
        assert!(b2.get("title").is_none());
    }

    #[test]
    fn request_folder_breadth_carries_the_folder_id() {
        let with =
            build_cloud_request(None, "ws1", "hi", None, "folder", "n1", Some("f1")).unwrap();
        assert_eq!(with["scope"], json!({ "breadth": "folder", "note_id": "n1", "folder_id": "f1" }));
    }

    #[test]
    fn request_folder_breadth_without_a_folder_is_an_error_not_a_clamp() {
        // Issue #58: no silent degradation. The desktop heals the folder-less
        // case to "note" upstream, so this strict guard shouldn't fire — but if
        // it's reached it errors loudly rather than quietly narrowing to note.
        let err = build_cloud_request(None, "ws1", "hi", None, "folder", "n1", None).unwrap_err();
        assert!(err.to_lowercase().contains("folder"), "names the missing folder: {err}");
    }

    #[test]
    fn request_unrecognized_breadth_is_an_error() {
        // Issue #58: the former `_ => {}` fall-through silently clamped to note.
        let err = build_cloud_request(None, "ws1", "hi", None, "everything", "n1", None).unwrap_err();
        assert!(err.contains("everything"), "the offending value is surfaced: {err}");
    }

    #[test]
    fn request_all_breadth_keeps_the_grounding_anchor() {
        let b = build_cloud_request(None, "ws1", "hi", None, "all", "n1", None).unwrap();
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
