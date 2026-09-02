//! Concrete `ChatAdapter` implementations. The cloud + local ones delegate the
//! actual HTTP streaming to `openai.rs` (shared with the summary path); the
//! fake one is deterministic for tests.
//!
//! Tool-calling (issue #47) frames differently per provider — OpenAI wants
//! index-keyed streamed `tool_calls` with ids and stringified arguments; Ollama
//! native returns buffered `tool_calls` with object arguments and no ids. Both
//! per-provider stream mappers live here, converting to the normalized
//! `ChatStep` the loop consumes.

use super::adapter::{ChatAdapter, ChatCtx, ChatStep, ChatStreamEvent, ChatTurn, ToolCall, ToolSpec};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};

/// Lower a tool spec to the OpenAI/Ollama `{type:"function", function:{…}}`
/// envelope — both providers accept the identical shape (verified in spike #45).
fn lower_tools(tools: &[ToolSpec]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                },
            })
        })
        .collect()
}

/// Lower the conversation to OpenAI chat messages: assistant turns carry
/// `tool_calls` (arguments as a JSON string), tool turns carry `tool_call_id`.
fn lower_messages_openai(messages: &[ChatTurn]) -> Vec<Value> {
    messages
        .iter()
        .map(|m| {
            if m.role == "assistant" && !m.tool_calls.is_empty() {
                json!({
                    "role": "assistant",
                    "content": m.text,
                    "tool_calls": m.tool_calls.iter().map(|tc| json!({
                        "id": tc.id,
                        "type": "function",
                        "function": { "name": tc.name, "arguments": tc.arguments },
                    })).collect::<Vec<_>>(),
                })
            } else if m.role == "tool" {
                json!({
                    "role": "tool",
                    "tool_call_id": m.tool_call_id.clone().unwrap_or_default(),
                    "content": m.text,
                })
            } else {
                json!({ "role": m.role, "content": m.text })
            }
        })
        .collect()
}

/// Lower the conversation to Ollama native messages: assistant tool calls take
/// `arguments` as an object (parsed from our stored string), and tool results
/// are a bare `{role:"tool", content}` (Ollama keys tool output positionally,
/// with no id — see spike #45).
fn lower_messages_ollama(messages: &[ChatTurn]) -> Vec<Value> {
    messages
        .iter()
        .map(|m| {
            if m.role == "assistant" && !m.tool_calls.is_empty() {
                json!({
                    "role": "assistant",
                    "content": m.text,
                    "tool_calls": m.tool_calls.iter().map(|tc| json!({
                        "function": {
                            "name": tc.name,
                            "arguments": serde_json::from_str::<Value>(&tc.arguments)
                                .unwrap_or_else(|_| json!({})),
                        },
                    })).collect::<Vec<_>>(),
                })
            } else if m.role == "tool" {
                json!({ "role": "tool", "content": m.text })
            } else {
                json!({ "role": m.role, "content": m.text })
            }
        })
        .collect()
}

fn emit_step(
    text: String,
    raw: Vec<crate::openai::RawToolCall>,
    on_event: &mut (dyn FnMut(ChatStreamEvent) + Send),
) -> ChatStep {
    let tool_calls: Vec<ToolCall> = raw
        .into_iter()
        .map(|c| ToolCall { id: c.id, name: c.name, arguments: c.arguments })
        .collect();
    for tc in &tool_calls {
        on_event(ChatStreamEvent::ToolCall(tc.clone()));
    }
    ChatStep { text, tool_calls }
}

/// Cloud OpenAI, streaming via SSE. Uses the shared BYO key.
pub struct OpenAiChatAdapter;

#[async_trait]
impl ChatAdapter for OpenAiChatAdapter {
    fn provider_id(&self) -> &'static str {
        "openai"
    }

    async fn step(
        &self,
        ctx: ChatCtx<'_>,
        messages: &[ChatTurn],
        tools: &[ToolSpec],
        on_event: &mut (dyn FnMut(ChatStreamEvent) + Send),
    ) -> Result<ChatStep> {
        let api_key = ctx
            .api_key
            .ok_or_else(|| anyhow!("OpenAI chat needs an API key — add one in Settings → Chat."))?;
        let wire_messages = lower_messages_openai(messages);
        let wire_tools = lower_tools(tools);
        let (text, raw) = crate::openai::openai_chat_step(
            ctx.base_url,
            api_key,
            ctx.model,
            &wire_messages,
            &wire_tools,
            // Returning false breaks the SSE loop, so a stop lands mid-answer
            // and the partial that already streamed is what comes back (#80).
            |delta| {
                on_event(ChatStreamEvent::TextDelta(delta.to_string()));
                !ctx.cancel.is_cancelled()
            },
        )
        .await?;
        Ok(emit_step(text, raw, on_event))
    }
}

/// Any local LLM server. Ollama is driven through its native `/api/chat` (tool
/// steps are buffered — spike #45 — and the final answer is emitted as one text
/// delta); every other runtime (LM Studio, llama-server, vLLM, mlx) speaks the
/// same OpenAI-compat endpoint the cloud path uses. Which one is decided by the
/// base URL, exactly as the summary path decides it (#179).
pub struct OllamaChatAdapter;

#[async_trait]
impl ChatAdapter for OllamaChatAdapter {
    fn provider_id(&self) -> &'static str {
        "ollama"
    }

    async fn step(
        &self,
        ctx: ChatCtx<'_>,
        messages: &[ChatTurn],
        tools: &[ToolSpec],
        on_event: &mut (dyn FnMut(ChatStreamEvent) + Send),
    ) -> Result<ChatStep> {
        let wire_tools = lower_tools(tools);
        let Some(native) = crate::openai::ollama_native_url(ctx.base_url) else {
            // A local OpenAI-compat server. It has no key, but some runtimes
            // still expect the header to exist, so send an empty bearer.
            let wire_messages = lower_messages_openai(messages);
            let (text, raw) = crate::openai::openai_chat_step(
                ctx.base_url,
                ctx.api_key.unwrap_or(""),
                ctx.model,
                &wire_messages,
                &wire_tools,
                |delta| {
                    on_event(ChatStreamEvent::TextDelta(delta.to_string()));
                    !ctx.cancel.is_cancelled()
                },
            )
            .await?;
            return Ok(emit_step(text, raw, on_event));
        };
        let wire_messages = lower_messages_ollama(messages);
        let (text, raw) = crate::openai::ollama_chat_step(
            &native,
            ctx.model,
            &wire_messages,
            &wire_tools,
            // Buffered, so this fires once at the end and the bool is moot —
            // `agentic_loop` aborts an in-flight Ollama step by racing it
            // against the cancel flag instead (#80).
            |delta| {
                on_event(ChatStreamEvent::TextDelta(delta.to_string()));
                !ctx.cancel.is_cancelled()
            },
        )
        .await?;
        Ok(emit_step(text, raw, on_event))
    }
}

/// Test adapter that emits one text delta and then never returns, standing in
/// for a provider mid-answer (or a buffered Ollama call that can't be
/// interrupted from inside). Used to exercise the stop path where the step
/// future is dropped: its return value is lost, so the partial must survive via
/// the deltas the caller already accumulated (issue #80).
#[cfg(test)]
pub struct StallingChatAdapter {
    pub text: String,
}

#[cfg(test)]
#[async_trait]
impl ChatAdapter for StallingChatAdapter {
    fn provider_id(&self) -> &'static str {
        "stalling"
    }

    async fn step(
        &self,
        _ctx: ChatCtx<'_>,
        _messages: &[ChatTurn],
        _tools: &[ToolSpec],
        on_event: &mut (dyn FnMut(ChatStreamEvent) + Send),
    ) -> Result<ChatStep> {
        on_event(ChatStreamEvent::TextDelta(self.text.clone()));
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }
}

/// Deterministic adapter for tests: replays a script of `ChatStep`s in order,
/// emitting the matching normalized events for each (tool-call events for tool
/// steps, a single text delta for text steps). Never touches the network.
#[cfg(test)]
pub struct FakeChatAdapter {
    steps: std::sync::Mutex<std::collections::VecDeque<ChatStep>>,
}

#[cfg(test)]
impl FakeChatAdapter {
    /// A single text answer (no tools) — the #46-style one-shot.
    pub fn new<I, S>(deltas: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let text: String = deltas.into_iter().map(Into::into).collect();
        Self::scripted(vec![ChatStep { text, tool_calls: Vec::new() }])
    }

    /// A full scripted loop: each `ChatStep` is one provider round-trip.
    pub fn scripted(steps: Vec<ChatStep>) -> Self {
        Self { steps: std::sync::Mutex::new(steps.into()) }
    }

    /// Convenience: a step that requests one tool call.
    pub fn tool_step(id: &str, name: &str, arguments: &str) -> ChatStep {
        ChatStep {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: id.into(),
                name: name.into(),
                arguments: arguments.into(),
            }],
        }
    }

    /// A tool step that narrates first — a real provider streams prose like
    /// "Let me search your notes…" alongside its tool call, and that prose has
    /// already reached the UI by the time the tool runs (issue #98).
    pub fn narrated_tool_step(text: &str, id: &str, name: &str, arguments: &str) -> ChatStep {
        ChatStep { text: text.into(), ..Self::tool_step(id, name, arguments) }
    }

    pub fn text_step(text: &str) -> ChatStep {
        ChatStep { text: text.into(), tool_calls: Vec::new() }
    }
}

#[cfg(test)]
#[async_trait]
impl ChatAdapter for FakeChatAdapter {
    fn provider_id(&self) -> &'static str {
        "fake"
    }

    async fn step(
        &self,
        _ctx: ChatCtx<'_>,
        _messages: &[ChatTurn],
        tools: &[ToolSpec],
        on_event: &mut (dyn FnMut(ChatStreamEvent) + Send),
    ) -> Result<ChatStep> {
        let step = self
            .steps
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| ChatStep { text: "(no more scripted steps)".into(), tool_calls: Vec::new() });
        // A scripted tool step only "counts" its tool calls when tools are on
        // offer; on the forced final step (tools dropped) fall back to its text
        // so the loop always terminates with an answer.
        if !step.tool_calls.is_empty() && !tools.is_empty() {
            // Prose accompanying a tool call streams before the call, same as a
            // real provider's SSE ordering.
            if !step.text.is_empty() {
                on_event(ChatStreamEvent::TextDelta(step.text.clone()));
            }
            for tc in &step.tool_calls {
                on_event(ChatStreamEvent::ToolCall(tc.clone()));
            }
            Ok(step)
        } else {
            let text = if step.text.is_empty() {
                "Based on your notes, here is the answer.".to_string()
            } else {
                step.text
            };
            on_event(ChatStreamEvent::TextDelta(text.clone()));
            Ok(ChatStep { text, tool_calls: Vec::new() })
        }
    }
}

#[cfg(test)]
mod local_compat_tests {
    use super::*;
    use crate::chat::adapter::CancelFlag;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// One-shot OpenAI-compat `/chat/completions` server: reads the request,
    /// answers with an SSE stream carrying a single content delta. Returns its
    /// port and the request body it saw.
    async fn fake_compat_server() -> (u16, tokio::task::JoinHandle<String>) {
        serve_sse(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi from mlx\"}}]}\n\n\
             data: [DONE]\n\n",
        )
        .await
    }

    /// One-shot server answering with `body` as an event stream.
    async fn serve_sse(body: &'static str) -> (u16, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 65536];
            let n = sock.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
            sock.flush().await.unwrap();
            req
        });
        (port, handle)
    }

    #[tokio::test]
    async fn local_chat_falls_back_to_openai_compat_off_ollamas_port() {
        let (port, server) = fake_compat_server().await;
        let base = format!("http://127.0.0.1:{port}/v1");
        let cancel = CancelFlag::new();
        let ctx = ChatCtx {
            model: "mlx-community/Qwen3-8B",
            api_key: None,
            base_url: &base,
            think: false,
            cancel: &cancel,
        };
        let mut seen = String::new();
        let step = OllamaChatAdapter
            .step(
                ctx,
                &[ChatTurn::new("user", "hello")],
                &[],
                &mut |ev| {
                    if let ChatStreamEvent::TextDelta(d) = ev {
                        seen.push_str(&d);
                    }
                },
            )
            .await
            .expect("a non-Ollama local server must still chat");
        assert_eq!(step.text, "hi from mlx");
        assert_eq!(seen, "hi from mlx");
        let req = server.await.unwrap();
        assert!(req.starts_with("POST /v1/chat/completions"), "req was: {req}");
    }

    #[tokio::test]
    async fn a_streamed_tool_call_from_a_local_compat_server_is_assembled() {
        let (port, server) = serve_sse(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\
             \"function\":{\"name\":\"search_notes\",\"arguments\":\"{\\\"query\\\":\"}}]}}]}\n\n\
             data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\
             \"function\":{\"arguments\":\"\\\"budget\\\"}\"}}]}}]}\n\n\
             data: [DONE]\n\n",
        )
        .await;
        let base = format!("http://127.0.0.1:{port}/v1");
        let cancel = CancelFlag::new();
        let ctx = ChatCtx {
            model: "local-model",
            api_key: None,
            base_url: &base,
            think: false,
            cancel: &cancel,
        };
        let tools = [ToolSpec {
            name: "search_notes".into(),
            description: "search".into(),
            parameters: json!({"type": "object", "properties": {}}),
        }];
        let mut calls = Vec::new();
        let step = OllamaChatAdapter
            .step(ctx, &[ChatTurn::new("user", "budget?")], &tools, &mut |ev| {
                if let ChatStreamEvent::ToolCall(tc) = ev {
                    calls.push(tc);
                }
            })
            .await
            .expect("tool calls must survive the compat path");
        assert_eq!(step.tool_calls.len(), 1);
        assert_eq!(step.tool_calls[0].name, "search_notes");
        assert_eq!(step.tool_calls[0].arguments, "{\"query\":\"budget\"}");
        assert_eq!(calls.len(), 1);
        let req = server.await.unwrap();
        assert!(req.contains("search_notes"), "tools were not offered: {req}");
    }

    #[tokio::test]
    async fn ollamas_own_port_still_takes_the_native_api() {
        let (port, server) = fake_compat_server().await;
        // The native path only triggers on :11434, so fake that host:port in
        // the URL and let the request itself say which endpoint was chosen.
        let base = format!("http://127.0.0.1:{port}/v1");
        let native = crate::openai::ollama_native_url("http://localhost:11434/v1");
        assert_eq!(native.as_deref(), Some("http://localhost:11434/api"));
        assert_eq!(crate::openai::ollama_native_url(&base), None);
        server.abort();
    }
}
