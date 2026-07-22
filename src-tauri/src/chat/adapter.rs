//! `ChatAdapter` — the provider seam for chat completions. Every provider
//! (cloud OpenAI, local Ollama, and a fake for tests) implements this one
//! trait, so the chat command's dispatch path is provider-agnostic. Mirrors
//! the `stt::BatchSttAdapter` shape.
//!
//! Issue #47 extends the seam with tool-calling: `stream` now runs ONE step of
//! the agentic loop, returning both the assistant's text and any tool calls it
//! requested. The loop itself lives in `chat::run_chat`; per-provider stream
//! mappers (OpenAI vs Ollama-native frame tool-calls differently) live in the
//! concrete adapters.

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// One turn in the prompt sent to a provider. Beyond plain text, an assistant
/// turn can carry the `tool_calls` it requested, and a `tool` turn carries the
/// result of one call (keyed by `tool_call_id`) — both are re-lowered to the
/// provider each step so the model sees the full tool trail.
#[derive(Debug, Clone, Default)]
pub struct ChatTurn {
    pub role: String,
    pub text: String,
    /// Populated on assistant turns that requested tools.
    pub tool_calls: Vec<ToolCall>,
    /// Set on `tool`-role turns: which call this is the result of.
    pub tool_call_id: Option<String>,
}

impl ChatTurn {
    pub fn new(role: impl Into<String>, text: impl Into<String>) -> Self {
        Self { role: role.into(), text: text.into(), ..Default::default() }
    }

    /// An assistant turn that requested one or more tool calls (text may be
    /// empty — many models emit tool calls with no prose).
    pub fn assistant_tool_calls(text: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self { role: "assistant".into(), text: text.into(), tool_calls, tool_call_id: None }
    }

    /// A tool-result turn feeding one call's output back to the model.
    pub fn tool_result(tool_call_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            role: "tool".into(),
            text: text.into(),
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id.into()),
        }
    }
}

/// A tool call the model requested: a stable `id` (echoed back on the result
/// turn), the tool `name`, and raw JSON `arguments` (kept as a string because
/// providers stream it as text; parsed by the tool layer).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// A tool the model may call, described to the provider. `parameters` is a JSON
/// Schema object; the concrete adapters wrap it in each provider's tool-spec
/// envelope.
#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: serde_json::Value,
}

/// Per-call provider inputs, borrowed. `base_url` is the OpenAI-compat base
/// (cloud OpenAI's, or the local server's); `think` toggles reasoning on
/// providers that support it (Ollama/Qwen).
pub struct ChatCtx<'a> {
    pub model: &'a str,
    pub api_key: Option<&'a str>,
    pub base_url: &'a str,
    pub think: bool,
}

/// Normalized streamed output during one step. `TextDelta` streams the answer
/// token-by-token for live UI; `ToolCall` fires once per fully-assembled call
/// the model requested (providers buffer partial call args internally).
#[derive(Debug, Clone, PartialEq)]
pub enum ChatStreamEvent {
    TextDelta(String),
    ToolCall(ToolCall),
}

/// The result of one completed step: the assistant's full text plus every tool
/// call it requested. An empty `tool_calls` means the model answered and the
/// loop should stop.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChatStep {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
}

#[async_trait]
pub trait ChatAdapter: Send + Sync {
    fn provider_id(&self) -> &'static str;

    /// Run ONE step: stream a completion for `messages`, offering `tools`
    /// (empty = no tools this step, e.g. the forced final wrap-up). Fire
    /// `on_event` per normalized event and return the assembled step.
    async fn step(
        &self,
        ctx: ChatCtx<'_>,
        messages: &[ChatTurn],
        tools: &[ToolSpec],
        on_event: &mut (dyn FnMut(ChatStreamEvent) + Send),
    ) -> Result<ChatStep>;
}
