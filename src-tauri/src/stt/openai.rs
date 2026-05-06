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
        let prompt = build_whisper_prompt(ctx.bias_terms, ctx.prior_context);
        let (text, words) = openai::transcribe_file(
            api_key,
            ctx.model,
            Some(ctx.language),
            prompt.as_deref(),
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

/// Glue bias terms + trailing transcript context into Whisper's
/// `initial_prompt` slot. Vocabulary terms come first (Whisper biases
/// toward early prompt tokens), then trail context. Returns None when
/// neither is present so the API call omits the field.
///
/// Lives here in Phase 2 Task 3; Task 4 moves it to `openai_compat`
/// once that module exists.
pub(crate) fn build_whisper_prompt(
    bias_terms: &[&str],
    prior_context: Option<&str>,
) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if !bias_terms.is_empty() {
        parts.push(bias_terms.join(", "));
    }
    if let Some(ctx) = prior_context {
        let trimmed = ctx.trim();
        if !trimmed.is_empty() {
            parts.push(trimmed.to_string());
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(". "))
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
