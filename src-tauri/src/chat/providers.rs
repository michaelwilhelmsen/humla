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

/// Local Ollama, via its native `/api/chat`. Tool steps are buffered (spike
/// #45); the final buffered answer is emitted as one text delta.
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
        let native = crate::openai::ollama_native_url(ctx.base_url).ok_or_else(|| {
            anyhow!(
                "Chat on Ollama needs an Ollama server (…:11434). \
                 Check the local server URL in Settings."
            )
        })?;
        let wire_messages = lower_messages_ollama(messages);
        let wire_tools = lower_tools(tools);
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
