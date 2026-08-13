//! OpenAI batch STT adapter. Delegates to the shared OpenAI-compat
//! transcriber for the multipart POST + response parsing — Phase 2 made
//! the HTTP plumbing reusable so Groq can plug into the same code path.

use anyhow::Result;
use async_trait::async_trait;
use std::path::Path;

use crate::stt::adapter::{BatchSttAdapter, TranscribeCtx, TranscribeResult};
use crate::stt::openai_compat;

#[derive(Default)]
pub struct OpenAiAdapter;

impl OpenAiAdapter {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl BatchSttAdapter for OpenAiAdapter {
    fn provider_id(&self) -> &'static str {
        "openai"
    }

    fn label(&self) -> &'static str {
        "OpenAI"
    }

    fn supports_language(&self, _lang: &str) -> bool {
        true
    }

    fn supports_word_timestamps(&self, model: &str) -> bool {
        // Same gate as the legacy code: only `whisper-1` returns word-level
        // timestamps when asked for verbose_json. The gpt-4o-transcribe
        // family rejects verbose_json outright.
        model == "whisper-1"
    }

    async fn transcribe(
        &self,
        ctx: TranscribeCtx<'_>,
        audio: &Path,
    ) -> Result<TranscribeResult> {
        let api_key = ctx
            .api_key
            .ok_or_else(|| anyhow::anyhow!("OpenAI adapter requires api_key"))?;
        let base_url = ctx.base_url.unwrap_or("https://api.openai.com/v1");
        let verbose = self.supports_word_timestamps(ctx.model);
        // Note the detection gap: `verbose` is `whisper-1` only, so the
        // gpt-4o-transcribe family never reports a language (issue #167).
        openai_compat::transcribe(
            base_url,
            api_key,
            ctx.model,
            Some(ctx.language),
            ctx.bias_terms,
            ctx.prior_context,
            audio,
            verbose,
            // Per OpenAI docs, gpt-4o-transcribe-diarize doesn't accept prompt.
            Some("gpt-4o-transcribe-diarize"),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_matches_legacy_behavior() {
        let a = OpenAiAdapter::new();
        assert_eq!(a.provider_id(), "openai");
        assert_eq!(a.label(), "OpenAI");
        assert!(a.supports_word_timestamps("whisper-1"));
        assert!(!a.supports_word_timestamps("gpt-4o-transcribe"));
        assert!(!a.supports_word_timestamps("gpt-4o-transcribe-diarize"));
    }
}
