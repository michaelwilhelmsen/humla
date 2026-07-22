//! `ChatAdapter` — the provider seam for chat completions. Every provider
//! (cloud OpenAI, local Ollama, and a fake for tests) implements this one
//! trait, so the chat command's dispatch path is provider-agnostic. Mirrors
//! the `stt::BatchSttAdapter` shape.

use anyhow::Result;
use async_trait::async_trait;

/// One turn in the prompt sent to a provider. `role` is "system" / "user" /
/// "assistant"; `text` is the plain-text content (this slice has text-only
/// parts, so a turn flattens to a single string).
#[derive(Debug, Clone)]
pub struct ChatTurn {
    pub role: String,
    pub text: String,
}

impl ChatTurn {
    pub fn new(role: impl Into<String>, text: impl Into<String>) -> Self {
        Self { role: role.into(), text: text.into() }
    }
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

/// Normalized streamed output. Owns its payload so the trait callback needs no
/// lifetime gymnastics. Only `TextDelta` exists in this slice; reasoning / tool
/// variants land with the agentic-retrieval slice (see the wire contract in
/// issue #46).
#[derive(Debug, Clone, PartialEq)]
pub enum ChatStreamEvent {
    TextDelta(String),
}

#[async_trait]
pub trait ChatAdapter: Send + Sync {
    fn provider_id(&self) -> &'static str;

    /// Stream a completion for `messages`, firing `on_event` per normalized
    /// event, and return the full assembled assistant text.
    async fn stream(
        &self,
        ctx: ChatCtx<'_>,
        messages: &[ChatTurn],
        on_event: &mut (dyn FnMut(ChatStreamEvent) + Send),
    ) -> Result<String>;
}
