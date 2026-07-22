//! Concrete `ChatAdapter` implementations. The cloud + local ones delegate the
//! actual HTTP streaming to `openai.rs` (shared with the summary path); the
//! fake one is deterministic for tests.

use super::adapter::{ChatAdapter, ChatCtx, ChatStreamEvent, ChatTurn};
use anyhow::{anyhow, Result};
use async_trait::async_trait;

fn as_pairs(messages: &[ChatTurn]) -> Vec<(&str, &str)> {
    messages.iter().map(|m| (m.role.as_str(), m.text.as_str())).collect()
}

/// Cloud OpenAI, streaming via SSE. Uses the shared BYO key.
pub struct OpenAiChatAdapter;

#[async_trait]
impl ChatAdapter for OpenAiChatAdapter {
    fn provider_id(&self) -> &'static str {
        "openai"
    }

    async fn stream(
        &self,
        ctx: ChatCtx<'_>,
        messages: &[ChatTurn],
        on_event: &mut (dyn FnMut(ChatStreamEvent) + Send),
    ) -> Result<String> {
        let api_key = ctx
            .api_key
            .ok_or_else(|| anyhow!("OpenAI chat needs an API key — add one in Settings → Chat."))?;
        let pairs = as_pairs(messages);
        crate::openai::openai_chat_stream(ctx.base_url, api_key, ctx.model, &pairs, |delta| {
            on_event(ChatStreamEvent::TextDelta(delta.to_string()));
        })
        .await
    }
}

/// Local Ollama, streaming via its native `/api/chat`. Reuses the summary
/// path's adaptive context-window sizing and Qwen sampling tuning.
pub struct OllamaChatAdapter;

#[async_trait]
impl ChatAdapter for OllamaChatAdapter {
    fn provider_id(&self) -> &'static str {
        "ollama"
    }

    async fn stream(
        &self,
        ctx: ChatCtx<'_>,
        messages: &[ChatTurn],
        on_event: &mut (dyn FnMut(ChatStreamEvent) + Send),
    ) -> Result<String> {
        let native = crate::openai::ollama_native_url(ctx.base_url).ok_or_else(|| {
            anyhow!(
                "Chat on Ollama needs an Ollama server (…:11434). \
                 Check the local server URL in Settings."
            )
        })?;
        let pairs = as_pairs(messages);
        crate::openai::ollama_chat_stream(&native, ctx.model, ctx.think, &pairs, |chunk| {
            // Only the answer is surfaced in this slice; reasoning deltas are
            // dropped (no reasoning UI for chat yet).
            if let crate::openai::StreamChunk::Content(c) = chunk {
                on_event(ChatStreamEvent::TextDelta(c.to_string()));
            }
        })
        .await
    }
}

/// Deterministic adapter for tests: emits each canned delta in order and
/// returns their concatenation. Never touches the network.
#[cfg(test)]
pub struct FakeChatAdapter {
    deltas: Vec<String>,
}

#[cfg(test)]
impl FakeChatAdapter {
    pub fn new<I, S>(deltas: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self { deltas: deltas.into_iter().map(Into::into).collect() }
    }
}

#[cfg(test)]
#[async_trait]
impl ChatAdapter for FakeChatAdapter {
    fn provider_id(&self) -> &'static str {
        "fake"
    }

    async fn stream(
        &self,
        _ctx: ChatCtx<'_>,
        _messages: &[ChatTurn],
        on_event: &mut (dyn FnMut(ChatStreamEvent) + Send),
    ) -> Result<String> {
        let mut full = String::new();
        for d in &self.deltas {
            full.push_str(d);
            on_event(ChatStreamEvent::TextDelta(d.clone()));
        }
        Ok(full)
    }
}
