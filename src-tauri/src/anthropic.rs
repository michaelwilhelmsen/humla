//! Anthropic's Messages API (#183) — the summary and chat transport for the
//! `anthropic` provider. Separate from `openai.rs` because the wire shape
//! differs on every axis that matters: a top-level `system` string rather than
//! a system turn, tool specs keyed `input_schema`, tool results carried in a
//! user turn, and an SSE stream of typed content blocks instead of
//! `choices[].delta`.
//!
//! No `thinking` block is sent: current models reject an explicit one, and
//! omitting it leaves each model's own adaptive default deciding.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

pub const BASE: &str = "https://api.anthropic.com/v1";
pub const VERSION: &str = "2023-06-01";

/// Output ceilings. A meeting summary is long-form; one agentic chat step is a
/// short answer or a tool call.
pub const SUMMARY_MAX_TOKENS: u32 = 16_000;
pub const CHAT_MAX_TOKENS: u32 = 8_192;

pub const DEFAULT_MODEL: &str = "claude-sonnet-5";

/// One assembled tool call the model requested.
#[derive(Debug, Clone, PartialEq)]
pub struct RawToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// Normalized item from the stream, handed to the caller as it arrives.
#[derive(Debug, Clone, PartialEq)]
pub enum AnthropicEvent {
    Text(String),
    Thinking(String),
    ToolCall(RawToolCall),
}

/// Everything one completed request produced.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AnthropicStep {
    pub text: String,
    pub thinking: String,
    pub tool_calls: Vec<RawToolCall>,
    pub stop_reason: Option<String>,
}

pub fn headers(key: &str) -> Vec<(&'static str, String)> {
    vec![
        ("x-api-key", key.to_string()),
        ("anthropic-version", VERSION.to_string()),
    ]
}

/// Turn a non-2xx response into something a user can act on. The JSON body
/// carries `error.message`; the status is what says whose problem it is.
pub fn readable_error(status: u16, body: &str) -> String {
    let detail = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(|s| s.to_string()))
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| body.chars().take(300).collect());
    match status {
        401 | 403 => "Anthropic rejected the API key — check it in Settings".to_string(),
        429 => "Anthropic rate limit hit — wait a moment and try again".to_string(),
        529 => "Anthropic is overloaded right now — try again shortly".to_string(),
        s if (500..600).contains(&s) => format!("Anthropic server error (HTTP {s}): {detail}"),
        s => format!("HTTP {s} from Anthropic: {detail}"),
    }
}

/// Byte-level line buffering: decoding each network chunk as it arrives would
/// corrupt a multi-byte character split across two reads.
#[derive(Default)]
struct LineBuf {
    buf: Vec<u8>,
}

impl LineBuf {
    fn extend(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
    }

    fn next_line(&mut self) -> Option<String> {
        let idx = self.buf.iter().position(|&b| b == b'\n')?;
        let raw: Vec<u8> = self.buf.drain(..=idx).collect();
        Some(String::from_utf8_lossy(&raw).into_owned())
    }
}

enum Block {
    Text,
    Thinking,
    ToolUse { id: String, name: String, json: String },
    Other,
}

/// Incremental parser for a Messages-API SSE body. Fed raw bytes, so an event
/// split across chunk boundaries survives.
#[derive(Default)]
pub struct AnthropicSseParser {
    lines: LineBuf,
    blocks: std::collections::BTreeMap<u64, Block>,
    pub stop_reason: Option<String>,
}

impl AnthropicSseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one network chunk; returns the events it completed. An `error`
    /// event in the stream is returned as `Err` with readable text.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<AnthropicEvent>> {
        self.lines.extend(chunk);
        let mut out = Vec::new();
        while let Some(line) = self.lines.next_line() {
            let line = line.trim();
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let Ok(frame) = serde_json::from_str::<Value>(data.trim()) else {
                continue;
            };
            self.handle(&frame, &mut out)?;
        }
        Ok(out)
    }

    fn handle(&mut self, frame: &Value, out: &mut Vec<AnthropicEvent>) -> Result<()> {
        let index = frame["index"].as_u64().unwrap_or(0);
        match frame["type"].as_str().unwrap_or_default() {
            "content_block_start" => {
                let cb = &frame["content_block"];
                let block = match cb["type"].as_str().unwrap_or_default() {
                    "text" => Block::Text,
                    "thinking" => Block::Thinking,
                    "tool_use" => Block::ToolUse {
                        id: cb["id"].as_str().unwrap_or_default().to_string(),
                        name: cb["name"].as_str().unwrap_or_default().to_string(),
                        json: String::new(),
                    },
                    _ => Block::Other,
                };
                if let Block::Text = block {
                    if let Some(t) = cb["text"].as_str().filter(|t| !t.is_empty()) {
                        out.push(AnthropicEvent::Text(t.to_string()));
                    }
                }
                self.blocks.insert(index, block);
            }
            "content_block_delta" => {
                let delta = &frame["delta"];
                match delta["type"].as_str().unwrap_or_default() {
                    "text_delta" => {
                        if let Some(t) = delta["text"].as_str().filter(|t| !t.is_empty()) {
                            out.push(AnthropicEvent::Text(t.to_string()));
                        }
                    }
                    "thinking_delta" => {
                        if let Some(t) = delta["thinking"].as_str().filter(|t| !t.is_empty()) {
                            out.push(AnthropicEvent::Thinking(t.to_string()));
                        }
                    }
                    "input_json_delta" => {
                        if let Some(Block::ToolUse { json, .. }) = self.blocks.get_mut(&index) {
                            json.push_str(delta["partial_json"].as_str().unwrap_or_default());
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                // Every kind is dropped here, so the map never outlives the
                // blocks it tracks.
                let finished = self.blocks.remove(&index);
                if let Some(Block::ToolUse { id, name, json }) = finished {
                    let arguments = if json.trim().is_empty() { "{}".to_string() } else { json };
                    out.push(AnthropicEvent::ToolCall(RawToolCall { id, name, arguments }));
                }
            }
            "message_delta" => {
                if let Some(r) = frame["delta"]["stop_reason"].as_str() {
                    self.stop_reason = Some(r.to_string());
                }
            }
            "error" => {
                let message = frame["error"]["message"].as_str().unwrap_or("stream error");
                let kind = frame["error"]["type"].as_str().unwrap_or("error");
                return Err(anyhow!("Anthropic stream {kind}: {message}"));
            }
            _ => {}
        }
        Ok(())
    }
}

/// POST one streamed `/v1/messages` request. `on_event` returning false
/// cancels the turn, leaving whatever already streamed as the result.
///
/// Neither sampling knobs nor a `thinking` block are sent: current models
/// reject both.
#[allow(clippy::too_many_arguments)]
pub async fn messages_stream<F>(
    base_url: &str,
    api_key: &str,
    model: &str,
    system: &str,
    messages: &[Value],
    tools: &[Value],
    max_tokens: u32,
    mut on_event: F,
) -> Result<AnthropicStep>
where
    F: FnMut(AnthropicEvent) -> bool + Send,
{
    let mut body = json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": messages,
        "stream": true,
    });
    if !system.trim().is_empty() {
        body["system"] = json!(system);
    }
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }

    let http = crate::openai::client();
    let url = format!("{}/messages", base_url.trim_end_matches('/'));
    let started = std::time::Instant::now();
    let make = || {
        let mut req = http.post(&url).json(&body);
        for (name, value) in headers(api_key) {
            req = req.header(name, value);
        }
        req
    };
    let r = match crate::openai::send_with_retries(make, started).await {
        Ok(resp) => resp,
        Err(e) if e.is_timeout() => {
            return Err(anyhow!(
                "Anthropic timed out after {}s — try again.",
                started.elapsed().as_secs()
            ))
        }
        Err(e) => return Err(anyhow!("Could not reach Anthropic: {}", crate::openai::error_chain(&e))),
    };

    let status = r.status();
    if !status.is_success() {
        let body = r.text().await.unwrap_or_default();
        return Err(anyhow!("{}", readable_error(status.as_u16(), &body)));
    }

    use futures_util::StreamExt;
    let mut byte_stream = r.bytes_stream();
    let mut parser = AnthropicSseParser::new();
    let mut step = AnthropicStep::default();
    'outer: while let Some(chunk_res) = byte_stream.next().await {
        let bytes = chunk_res.map_err(|e| {
            anyhow!(
                "Connection to Anthropic dropped mid-response after {} chars: {}",
                step.text.len(),
                crate::openai::error_chain(&e),
            )
        })?;
        for ev in parser.push(&bytes)? {
            match &ev {
                AnthropicEvent::Text(t) => step.text.push_str(t),
                AnthropicEvent::Thinking(t) => step.thinking.push_str(t),
                AnthropicEvent::ToolCall(c) => step.tool_calls.push(c.clone()),
            }
            if !on_event(ev) {
                break 'outer;
            }
        }
    }
    step.stop_reason = parser.stop_reason.clone();
    Ok(step)
}

pub async fn list_models(api_key: &str) -> Result<Vec<String>> {
    let mut req = crate::openai::client().get(format!("{BASE}/models?limit=1000"));
    for (name, value) in headers(api_key) {
        req = req.header(name, value);
    }
    let r = req.send().await.map_err(|e| anyhow!("network: {e}"))?;
    let status = r.status();
    let body = r.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow!("{}", readable_error(status.as_u16(), &body)));
    }
    let parsed: Value = serde_json::from_str(&body)?;
    Ok(parsed["data"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|m| m["id"].as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default())
}

/// One-shot summary through the Messages API, streaming deltas to `on_chunk`.
pub async fn summarize<F>(
    api_key: &str,
    model: &str,
    system_prompt: &str,
    user_message: &str,
    mut on_chunk: F,
) -> Result<String>
where
    F: FnMut(crate::openai::StreamChunk) + Send,
{
    let messages = vec![json!({ "role": "user", "content": user_message })];
    let started = std::time::Instant::now();
    let step = messages_stream(
        BASE,
        api_key,
        model,
        system_prompt,
        &messages,
        &[],
        SUMMARY_MAX_TOKENS,
        |ev| {
            match ev {
                AnthropicEvent::Text(t) => on_chunk(crate::openai::StreamChunk::Content(&t)),
                AnthropicEvent::Thinking(t) => on_chunk(crate::openai::StreamChunk::Thinking(&t)),
                AnthropicEvent::ToolCall(_) => {}
            }
            true
        },
    )
    .await?;
    eprintln!(
        "[llm] anthropic summary done in {:?}, {} chars",
        started.elapsed(),
        step.text.len()
    );
    if step.text.trim().is_empty() {
        return Err(anyhow!("{model} returned an empty response"));
    }
    Ok(step.text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(parser: &mut AnthropicSseParser, chunks: &[&str]) -> Result<Vec<AnthropicEvent>> {
        let mut all = Vec::new();
        for c in chunks {
            all.extend(parser.push(c.as_bytes())?);
        }
        Ok(all)
    }

    #[test]
    fn text_deltas_arrive_in_order() {
        let mut p = AnthropicSseParser::new();
        let events = drain(
            &mut p,
            &[
                "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
                "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hei \"}}\n\n",
                "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"der\"}}\n\n",
                "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
            ],
        )
        .unwrap();
        assert_eq!(
            events,
            vec![
                AnthropicEvent::Text("Hei ".into()),
                AnthropicEvent::Text("der".into())
            ]
        );
        assert_eq!(p.stop_reason.as_deref(), Some("end_turn"));
    }

    #[test]
    fn a_tool_use_block_assembles_across_chunk_boundaries() {
        let mut p = AnthropicSseParser::new();
        // The second chunk splits an event mid-line.
        let events = drain(
            &mut p,
            &[
                "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"search_notes\",\"input\":{}}}\n\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"query\\\":\"}}\n\ndata: {\"type\":\"content_bl",
                "ock_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"budget\\\"}\"}}\n\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            ],
        )
        .unwrap();
        assert_eq!(
            events,
            vec![AnthropicEvent::ToolCall(RawToolCall {
                id: "toolu_1".into(),
                name: "search_notes".into(),
                arguments: "{\"query\":\"budget\"}".into(),
            })]
        );
    }

    #[test]
    fn a_tool_use_with_no_deltas_is_an_empty_object() {
        let mut p = AnthropicSseParser::new();
        let events = drain(
            &mut p,
            &[
                "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_2\",\"name\":\"list_notes\",\"input\":{}}}\n\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            ],
        )
        .unwrap();
        assert_eq!(events[0], AnthropicEvent::ToolCall(RawToolCall {
            id: "toolu_2".into(),
            name: "list_notes".into(),
            arguments: "{}".into(),
        }));
    }

    #[test]
    fn thinking_deltas_are_surfaced() {
        let mut p = AnthropicSseParser::new();
        let events = drain(
            &mut p,
            &["data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"hm\"}}\n\n"],
        )
        .unwrap();
        assert_eq!(events, vec![AnthropicEvent::Thinking("hm".into())]);
    }

    #[test]
    fn a_mid_stream_error_event_fails_the_turn() {
        let mut p = AnthropicSseParser::new();
        let err = drain(
            &mut p,
            &["data: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n"],
        )
        .expect_err("an error event must not be swallowed")
        .to_string();
        assert!(err.contains("overloaded_error"), "{err}");
        assert!(err.contains("Overloaded"), "{err}");
    }

    #[test]
    fn readable_errors_name_the_cause() {
        let body = "{\"type\":\"error\",\"error\":{\"type\":\"api_error\",\"message\":\"boom\"}}";
        assert!(readable_error(401, body).contains("rejected the API key"));
        assert!(readable_error(429, body).contains("rate limit"));
        assert!(readable_error(529, body).contains("overloaded"));
        let five = readable_error(500, body);
        assert!(five.contains("server error"), "{five}");
        assert!(five.contains("boom"), "{five}");
        let other = readable_error(418, body);
        assert!(other.contains("HTTP 418"), "{other}");
        assert!(other.contains("boom"), "{other}");
    }

    #[test]
    fn an_unparseable_body_falls_back_to_a_snippet() {
        let msg = readable_error(400, "not json at all");
        assert!(msg.contains("not json at all"), "{msg}");
    }
}
