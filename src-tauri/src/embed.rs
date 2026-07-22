//! Embedding provider seam for semantic retrieval (issue #48). Mirrors the
//! `chat::ChatAdapter` / `stt::BatchSttAdapter` pattern: one `EmbeddingAdapter`
//! trait, so the retrieval pipeline is provider-agnostic. The embedding model
//! is auto-derived from the chat provider — there is no user-facing embedding
//! choice:
//!   - chat provider `openai` → `text-embedding-3-small` (cloud API)
//!   - chat provider `ollama` → `embeddinggemma` (local, via Ollama)
//!
//! Both providers speak the OpenAI-compatible `/v1/embeddings` shape, so the two
//! concrete adapters differ only in config (model / base URL / key) — the wire
//! call is shared in `openai::openai_embed`.

use anyhow::Result;
use async_trait::async_trait;

/// Model id for the cloud (OpenAI) embedder. Native 1536 dims; stored untruncated.
pub const OPENAI_EMBED_MODEL: &str = "text-embedding-3-small";
/// Model id for the local (Ollama) embedder. The sole local embedder — no
/// fallback model (see issue #48: tune retrieval, not the model).
pub const OLLAMA_EMBED_MODEL: &str = "embeddinggemma";

#[async_trait]
pub trait EmbeddingAdapter: Send + Sync {
    /// The embedding model id — also the index tag: switching it triggers a
    /// full re-embed under the new key (fixed-dim-per-index).
    fn model_id(&self) -> &str;

    /// Embed a batch of texts, returning one vector per input in input order.
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
}

/// The concrete HTTP embedder for both providers — they share the OpenAI-compat
/// `/v1/embeddings` wire call and differ only in these fields.
pub struct HttpEmbeddingAdapter {
    model: String,
    base_url: String,
    api_key: Option<String>,
}

#[async_trait]
impl EmbeddingAdapter for HttpEmbeddingAdapter {
    fn model_id(&self) -> &str {
        &self.model
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        crate::openai::openai_embed(&self.base_url, self.api_key.as_deref(), &self.model, texts).await
    }
}

/// Resolved embedding config, auto-derived from the chat provider. `None` means
/// no embedder is available (e.g. cloud chat with no API key) → retrieval
/// degrades to keyword-only.
pub struct EmbedConfig {
    pub provider: &'static str,
    pub model: &'static str,
    pub base_url: String,
    pub api_key: Option<String>,
}

impl EmbedConfig {
    pub fn adapter(&self) -> HttpEmbeddingAdapter {
        HttpEmbeddingAdapter {
            model: self.model.to_string(),
            base_url: self.base_url.clone(),
            api_key: self.api_key.clone(),
        }
    }
}

#[cfg(test)]
pub struct FakeEmbeddingAdapter {
    pub model: String,
    /// Fixed dimensionality of the returned vectors.
    pub dims: usize,
}

#[cfg(test)]
#[async_trait]
impl EmbeddingAdapter for FakeEmbeddingAdapter {
    fn model_id(&self) -> &str {
        &self.model
    }
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        // Deterministic: a simple bag-of-chars projection so identical text →
        // identical vector and different text → (usually) different vector.
        Ok(texts
            .iter()
            .map(|t| {
                let mut v = vec![0.0f32; self.dims];
                for b in t.bytes() {
                    v[(b as usize) % self.dims] += 1.0;
                }
                v
            })
            .collect())
    }
}
