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

/// Lower a tool spec to Anthropic's envelope: no `type: "function"` wrapper,
/// and the schema is keyed `input_schema`.
fn lower_tools_anthropic(tools: &[ToolSpec]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.parameters,
            })
        })
        .collect()
}

/// Lower the conversation to Anthropic's Messages shape: the system prompt
/// leaves the turn list entirely, tool calls become `tool_use` blocks on the
/// assistant turn, and their results become `tool_result` blocks in a **user**
/// turn. Roles must alternate, so adjacent same-role turns are merged — which
/// is also what puts a run of tool results into one user message. A turn with
/// no blocks at all is dropped, since an empty content array is rejected; the
/// caller refuses a request that leaves nothing behind.
fn lower_messages_anthropic(messages: &[ChatTurn]) -> (String, Vec<Value>) {
    let mut system: Vec<&str> = Vec::new();
    let mut turns: Vec<(&'static str, Vec<Value>)> = Vec::new();

    for m in messages {
        if m.role == "system" {
            if !m.text.trim().is_empty() {
                system.push(&m.text);
            }
            continue;
        }
        let (role, blocks): (&'static str, Vec<Value>) = if m.role == "tool" {
            (
                "user",
                vec![json!({
                    "type": "tool_result",
                    "tool_use_id": m.tool_call_id.clone().unwrap_or_default(),
                    "content": m.text,
                })],
            )
        } else if m.role == "assistant" {
            let mut blocks = Vec::new();
            if !m.text.is_empty() {
                blocks.push(json!({ "type": "text", "text": m.text }));
            }
            for tc in &m.tool_calls {
                blocks.push(json!({
                    "type": "tool_use",
                    "id": tc.id,
                    "name": tc.name,
                    "input": serde_json::from_str::<Value>(&tc.arguments).unwrap_or_else(|_| json!({})),
                }));
            }
            ("assistant", blocks)
        } else {
            let mut blocks = Vec::new();
            if !m.text.is_empty() {
                blocks.push(json!({ "type": "text", "text": m.text }));
            }
            ("user", blocks)
        };
        if blocks.is_empty() {
            continue;
        }
        match turns.last_mut() {
            Some((last_role, last_blocks)) if *last_role == role => last_blocks.extend(blocks),
            _ => turns.push((role, blocks)),
        }
    }

    let wire = turns
        .into_iter()
        .map(|(role, blocks)| json!({ "role": role, "content": blocks }))
        .collect();
    (system.join("\n\n"), wire)
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

/// Anthropic's Messages API. The base URL is a constructor argument so tests
/// can point one at a loopback server; production always uses the cloud base.
pub struct AnthropicChatAdapter {
    base_url: String,
}

impl AnthropicChatAdapter {
    pub fn new() -> Self {
        Self { base_url: crate::anthropic::BASE.to_string() }
    }

    pub fn with_base(base_url: impl Into<String>) -> Self {
        Self { base_url: base_url.into() }
    }
}

impl Default for AnthropicChatAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ChatAdapter for AnthropicChatAdapter {
    fn provider_id(&self) -> &'static str {
        "anthropic"
    }

    async fn step(
        &self,
        ctx: ChatCtx<'_>,
        messages: &[ChatTurn],
        tools: &[ToolSpec],
        on_event: &mut (dyn FnMut(ChatStreamEvent) + Send),
    ) -> Result<ChatStep> {
        let api_key = ctx.api_key.ok_or_else(|| {
            anyhow!("Anthropic chat needs an API key — add one in Settings → Chat.")
        })?;
        let (system, wire_messages) = lower_messages_anthropic(messages);
        if wire_messages.is_empty() {
            return Err(anyhow!("Nothing to send — the conversation has no content."));
        }
        let wire_tools = lower_tools_anthropic(tools);
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let step = crate::anthropic::messages_stream(
            &self.base_url,
            api_key,
            ctx.model,
            &system,
            &wire_messages,
            &wire_tools,
            crate::anthropic::CHAT_MAX_TOKENS,
            |ev| {
                match ev {
                    crate::anthropic::AnthropicEvent::Text(t) => {
                        on_event(ChatStreamEvent::TextDelta(t))
                    }
                    crate::anthropic::AnthropicEvent::ToolCall(c) => {
                        let tc = ToolCall { id: c.id, name: c.name, arguments: c.arguments };
                        tool_calls.push(tc.clone());
                        on_event(ChatStreamEvent::ToolCall(tc));
                    }
                    crate::anthropic::AnthropicEvent::Thinking(_) => {}
                }
                !ctx.cancel.is_cancelled()
            },
        )
        .await?;
        Ok(ChatStep { text: step.text, tool_calls })
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
    use crate::chat::test_server::serve_sse;

    /// The single content delta most of these tests want back.
    const ONE_DELTA: &str = "data: {\"choices\":[{\"delta\":{\"content\":\"hi from mlx\"}}]}\n\n\
                             data: [DONE]\n\n";

    #[tokio::test]
    async fn local_chat_falls_back_to_openai_compat_off_ollamas_port() {
        let (port, server) = serve_sse(vec![ONE_DELTA.into()]).await;
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
        let req = server.await.unwrap().remove(0);
        assert!(req.starts_with("POST /v1/chat/completions"), "req was: {req}");
    }

    #[tokio::test]
    async fn a_streamed_tool_call_from_a_local_compat_server_is_assembled() {
        let (port, server) = serve_sse(vec![
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\
             \"function\":{\"name\":\"search_notes\",\"arguments\":\"{\\\"query\\\":\"}}]}}]}\n\n\
             data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\
             \"function\":{\"arguments\":\"\\\"budget\\\"}\"}}]}}]}\n\n\
             data: [DONE]\n\n"
                .into(),
        ])
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
        let req = server.await.unwrap().remove(0);
        assert!(req.contains("search_notes"), "tools were not offered: {req}");
    }

    #[test]
    fn ollamas_own_port_still_takes_the_native_api() {
        let native = crate::openai::ollama_native_url("http://localhost:11434/v1");
        assert_eq!(native.as_deref(), Some("http://localhost:11434/api"));
        assert_eq!(crate::openai::ollama_native_url("http://127.0.0.1:8000/v1"), None);
    }
}

#[cfg(test)]
mod anthropic_tests {
    use super::*;
    use crate::chat::adapter::CancelFlag;
    use crate::chat::test_server::serve_sse;

    fn spec() -> ToolSpec {
        ToolSpec {
            name: "search_notes",
            description: "Search the user's notes",
            parameters: json!({"type": "object", "properties": {"query": {"type": "string"}}}),
        }
    }

    #[tokio::test]
    async fn an_empty_conversation_is_refused_before_the_request() {
        let cancel = CancelFlag::new();
        let ctx = ChatCtx {
            model: "claude-sonnet-5",
            api_key: Some("sk-ant-test"),
            base_url: "",
            think: false,
            cancel: &cancel,
        };
        let err = AnthropicChatAdapter::with_base("http://127.0.0.1:1/v1")
            .step(ctx, &[ChatTurn::new("user", "")], &[], &mut |_| {})
            .await
            .expect_err("an empty turn list must not reach the wire")
            .to_string();
        assert!(err.contains("Nothing to send"), "{err}");
    }

    #[test]
    fn a_tool_spec_lowers_to_input_schema_with_no_function_wrapper() {
        let wire = lower_tools_anthropic(&[spec()]);
        assert_eq!(wire[0]["name"], "search_notes");
        assert_eq!(wire[0]["description"], "Search the user's notes");
        assert_eq!(wire[0]["input_schema"], spec().parameters);
        assert!(wire[0].get("type").is_none(), "{}", wire[0]);
        assert!(wire[0].get("parameters").is_none(), "{}", wire[0]);
    }

    #[test]
    fn system_turns_leave_the_message_list() {
        let (system, wire) = lower_messages_anthropic(&[
            ChatTurn::new("system", "You are Humla."),
            ChatTurn::new("system", "Cite your sources."),
            ChatTurn::new("user", "what did we agree?"),
        ]);
        assert_eq!(system, "You are Humla.\n\nCite your sources.");
        assert_eq!(wire.len(), 1);
        assert_eq!(wire[0]["role"], "user");
    }

    #[test]
    fn an_assistant_tool_turn_becomes_text_and_tool_use_blocks() {
        let (_, wire) = lower_messages_anthropic(&[ChatTurn::assistant_tool_calls(
            "Let me look.",
            vec![ToolCall {
                id: "toolu_1".into(),
                name: "search_notes".into(),
                arguments: "{\"query\":\"budget\"}".into(),
            }],
        )]);
        let blocks = wire[0]["content"].as_array().unwrap();
        assert_eq!(blocks[0], json!({"type": "text", "text": "Let me look."}));
        assert_eq!(blocks[1]["type"], "tool_use");
        assert_eq!(blocks[1]["id"], "toolu_1");
        assert_eq!(blocks[1]["input"], json!({"query": "budget"}));
    }

    #[test]
    fn unparseable_tool_arguments_lower_to_an_empty_object() {
        let (_, wire) = lower_messages_anthropic(&[ChatTurn::assistant_tool_calls(
            "",
            vec![ToolCall { id: "t".into(), name: "n".into(), arguments: "not json".into() }],
        )]);
        let blocks = wire[0]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 1, "an empty text block must not be sent");
        assert_eq!(blocks[0]["input"], json!({}));
    }

    #[test]
    fn tool_results_group_into_one_user_turn_and_roles_alternate() {
        let (_, wire) = lower_messages_anthropic(&[
            ChatTurn::new("user", "what did we agree?"),
            ChatTurn::assistant_tool_calls(
                "",
                vec![
                    ToolCall { id: "a".into(), name: "search_notes".into(), arguments: "{}".into() },
                    ToolCall { id: "b".into(), name: "get_note".into(), arguments: "{}".into() },
                ],
            ),
            ChatTurn::tool_result("a", "hit one"),
            ChatTurn::tool_result("b", "hit two"),
            ChatTurn::new("assistant", "Here is the answer."),
        ]);
        let roles: Vec<&str> = wire.iter().map(|m| m["role"].as_str().unwrap()).collect();
        assert_eq!(roles, vec!["user", "assistant", "user", "assistant"]);
        let results = wire[2]["content"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["type"], "tool_result");
        assert_eq!(results[0]["tool_use_id"], "a");
        assert_eq!(results[1]["tool_use_id"], "b");
    }

    #[tokio::test]
    async fn a_step_streams_text_and_offers_tools_on_the_messages_endpoint() {
        let body = "event: content_block_delta\n\
                    data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Budsjettet \"}}\n\n\
                    data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ble godkjent.\"}}\n\n\
                    data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n\
                    data: {\"type\":\"message_stop\"}\n\n";
        let (port, server) = serve_sse(vec![body.into()]).await;
        let cancel = CancelFlag::new();
        let ctx = ChatCtx {
            model: "claude-sonnet-5",
            api_key: Some("sk-ant-test"),
            base_url: "",
            think: false,
            cancel: &cancel,
        };
        let mut seen = String::new();
        let step = AnthropicChatAdapter::with_base(format!("http://127.0.0.1:{port}/v1"))
            .step(ctx, &[ChatTurn::new("user", "budsjett?")], &[spec()], &mut |ev| {
                if let ChatStreamEvent::TextDelta(d) = ev {
                    seen.push_str(&d);
                }
            })
            .await
            .expect("the step must complete");
        assert_eq!(step.text, "Budsjettet ble godkjent.");
        assert_eq!(seen, step.text);

        let req = server.await.unwrap().remove(0);
        assert!(req.starts_with("POST /v1/messages"), "{req}");
        assert!(req.contains("x-api-key: sk-ant-test"), "{req}");
        assert!(req.contains("anthropic-version: 2023-06-01"), "{req}");
        assert!(req.contains("\"input_schema\""), "{req}");
        assert!(!req.contains("\"temperature\""), "{req}");
        assert!(!req.contains("\"thinking\""), "{req}");
    }

    #[tokio::test]
    async fn a_streamed_tool_call_is_assembled_and_emitted() {
        let body = "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_9\",\"name\":\"search_notes\",\"input\":{}}}\n\n\
                    data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"query\\\":\\\"budget\\\"}\"}}\n\n\
                    data: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
                    data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n";
        let (port, server) = serve_sse(vec![body.into()]).await;
        let cancel = CancelFlag::new();
        let ctx = ChatCtx {
            model: "claude-sonnet-5",
            api_key: Some("sk-ant-test"),
            base_url: "",
            think: false,
            cancel: &cancel,
        };
        let mut calls = Vec::new();
        let step = AnthropicChatAdapter::with_base(format!("http://127.0.0.1:{port}/v1"))
            .step(ctx, &[ChatTurn::new("user", "budget?")], &[spec()], &mut |ev| {
                if let ChatStreamEvent::ToolCall(tc) = ev {
                    calls.push(tc);
                }
            })
            .await
            .unwrap();
        assert_eq!(step.tool_calls.len(), 1);
        assert_eq!(step.tool_calls[0].arguments, "{\"query\":\"budget\"}");
        assert_eq!(calls, step.tool_calls);
        let _ = server.await.unwrap();
    }
}
