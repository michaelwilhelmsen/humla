//! OpenAI batch STT adapter. Wraps the existing `openai::transcribe_file`
//! function so we don't duplicate the multipart-form logic.

use anyhow::Result;
use async_trait::async_trait;
use std::path::Path;

use crate::openai;
use crate::stt::adapter::{BatchSttAdapter, TranscribeCtx, TranscribeResult, Word};

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
        let (text, words) = openai::transcribe_file(
            api_key,
            ctx.model,
            Some(ctx.language),
            ctx.initial_prompt,
            audio,
        )
        .await?;
        let words = words
            .into_iter()
            .map(|w| Word {
                text: w.text,
                start_ms: w.start_ms,
                end_ms: w.end_ms,
            })
            .collect();
        Ok(TranscribeResult { text, words })
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
