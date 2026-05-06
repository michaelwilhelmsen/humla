//! `BatchSttAdapter` trait and supporting types. Every provider —
//! cloud (OpenAI, Deepgram, …) or in-process (local Whisper) — implements
//! this trait. The recording pipeline calls `adapter.transcribe(ctx, path)`
//! and doesn't care which implementation it got.

use anyhow::Result;
use async_trait::async_trait;
use std::path::Path;

#[async_trait]
pub trait BatchSttAdapter: Send + Sync {
    fn provider_id(&self) -> &'static str;

    fn label(&self) -> &'static str;

    /// True when this adapter accepts the given language code. "auto" means
    /// the provider does language detection itself.
    fn supports_language(&self, lang: &str) -> bool;

    /// True when this adapter returns word-level timestamps for the given
    /// model. Drives whether the playback view's karaoke-style highlight
    /// has data to render for chunks produced by this adapter.
    fn supports_word_timestamps(&self, model: &str) -> bool;

    async fn transcribe(
        &self,
        ctx: TranscribeCtx<'_>,
        audio: &Path,
    ) -> Result<TranscribeResult>;
}

pub struct TranscribeCtx<'a> {
    pub model: &'a str,
    pub language: &'a str,
    /// User-supplied vocabulary (proper nouns, tech terms). Maps to
    /// Whisper's `initial_prompt` for OpenAI/Local/Groq, and to
    /// Deepgram's `keywords` query param.
    pub bias_terms: &'a [&'a str],
    /// Last ~150 transcribed words from this source's stream. Used by
    /// Whisper-shaped adapters as the trailing portion of `initial_prompt`
    /// to keep cross-chunk continuity. Ignored by Deepgram (no
    /// equivalent; Deepgram's keyword bias would actively hurt if fed
    /// transcript text — it boosts per-token probability, not continuation).
    pub prior_context: Option<&'a str>,
    pub api_key: Option<&'a str>,
    pub base_url: Option<&'a str>,
}

#[derive(Clone, Debug, Default)]
pub struct TranscribeResult {
    pub text: String,
    pub words: Vec<Word>,
}

/// One word's display text + millisecond bounds. Mirrors the existing
/// `local_whisper::Word` shape so callers can plumb either provider's
/// output through the same downstream path. Renaming `local_whisper::Word`
/// → `stt::Word` is deferred to a follow-up commit.
#[derive(Clone, Debug, Default)]
pub struct Word {
    pub text: String,
    pub start_ms: u64,
    pub end_ms: u64,
}
