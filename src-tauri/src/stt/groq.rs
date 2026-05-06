//! Groq batch STT adapter. Groq hosts whisper-large-v3-turbo at an
//! OpenAI-compatible endpoint; auth + payload + response shape are
//! identical to OpenAI, only the base URL and rate-limit profile differ.
//! Roughly 10x cheaper than OpenAI Whisper at comparable quality.

use anyhow::Result;
use async_trait::async_trait;
use std::path::Path;

use crate::stt::adapter::{BatchSttAdapter, TranscribeCtx, TranscribeResult, Word};
use crate::stt::openai_compat;

const GROQ_BASE: &str = "https://api.groq.com/openai/v1";

#[derive(Default)]
pub struct GroqAdapter;

impl GroqAdapter {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl BatchSttAdapter for GroqAdapter {
    fn provider_id(&self) -> &'static str {
        "groq"
    }

    fn label(&self) -> &'static str {
        "Groq"
    }

    fn supports_language(&self, _lang: &str) -> bool {
        // Groq's whisper-large-v3-turbo supports the full Whisper language
        // set plus auto-detect.
        true
    }

    fn supports_word_timestamps(&self, _model: &str) -> bool {
        // whisper-large-v3-turbo accepts verbose_json and returns
        // word-level timestamps. Model-agnostic in practice; if Groq adds
        // a non-Whisper STT model later, narrow this.
        true
    }

    async fn transcribe(
        &self,
        ctx: TranscribeCtx<'_>,
        audio: &Path,
    ) -> Result<TranscribeResult> {
        let api_key = ctx
            .api_key
            .ok_or_else(|| anyhow::anyhow!("Groq adapter requires api_key"))?;
        let base_url = ctx.base_url.unwrap_or(GROQ_BASE);
        let (text, words) = openai_compat::transcribe(
            base_url,
            api_key,
            ctx.model,
            Some(ctx.language),
            ctx.bias_terms,
            ctx.prior_context,
            audio,
            true, // always request verbose_json — Groq accepts it
            None,
        )
        .await?;
        Ok(TranscribeResult { text, words })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_is_correct() {
        let a = GroqAdapter::new();
        assert_eq!(a.provider_id(), "groq");
        assert_eq!(a.label(), "Groq");
        assert!(a.supports_word_timestamps("whisper-large-v3-turbo"));
    }
}
