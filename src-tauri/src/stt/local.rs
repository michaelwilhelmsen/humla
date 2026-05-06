//! Local Whisper batch STT adapter. Wraps
//! `local_whisper::transcribe_file_with_words`. The adapter holds the
//! shared `WhisperContext`, the model file path, and the GPU flag — all
//! data the legacy call site used to gather inline.

use anyhow::Result;
use async_trait::async_trait;
use std::path::{Path, PathBuf};

use crate::local_whisper::{self, SharedContext};
use crate::stt::adapter::{BatchSttAdapter, TranscribeCtx, TranscribeResult, Word};

pub struct LocalWhisperAdapter {
    shared: SharedContext,
    model_path: PathBuf,
    use_gpu: bool,
    preset: local_whisper::Preset,
}

impl LocalWhisperAdapter {
    pub fn new(
        shared: SharedContext,
        model_path: PathBuf,
        use_gpu: bool,
        preset: local_whisper::Preset,
    ) -> Self {
        Self { shared, model_path, use_gpu, preset }
    }
}

#[async_trait]
impl BatchSttAdapter for LocalWhisperAdapter {
    fn provider_id(&self) -> &'static str {
        "local"
    }

    fn label(&self) -> &'static str {
        "Local Whisper"
    }

    fn supports_language(&self, _lang: &str) -> bool {
        true
    }

    fn supports_word_timestamps(&self, _model: &str) -> bool {
        true
    }

    async fn transcribe(
        &self,
        ctx: TranscribeCtx<'_>,
        audio: &Path,
    ) -> Result<TranscribeResult> {
        let prompt = crate::stt::openai::build_whisper_prompt(ctx.bias_terms, ctx.prior_context);
        let (text, words) = local_whisper::transcribe_file_with_words(
            self.shared.clone(),
            self.model_path.clone(),
            self.use_gpu,
            ctx.language,
            prompt.as_deref(),
            self.preset,
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
    use parking_lot::Mutex;
    use std::sync::Arc;

    #[test]
    fn metadata_matches_legacy_behavior() {
        let shared = Arc::new(Mutex::new(None));
        let a = LocalWhisperAdapter::new(
            shared,
            PathBuf::from("/tmp/nonexistent.bin"),
            true,
            local_whisper::Preset::Quality,
        );
        assert_eq!(a.provider_id(), "local");
        assert_eq!(a.label(), "Local Whisper");
        assert!(a.supports_word_timestamps("any"));
    }
}
