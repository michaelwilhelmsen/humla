// Local LLM module — mirrors local_whisper.rs in shape. A SharedContext holds
// the lazily-loaded model handle; ensure_loaded gates first use; generate runs
// inference on a blocking thread. This file currently defines types + paths;
// model loading lands in a follow-up task.

use anyhow::{anyhow, Result};
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel, Special};
use llama_cpp_2::sampling::LlamaSampler;
use parking_lot::Mutex;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

// Default download targets. Three managed tiers:
// - Qwen 3 1.7B Q4_K_M: ~1.1 GB, ultra-budget. Apache 2.0. Multilingual
//   incl. Bokmål/Nynorsk/Swedish/Danish per the Qwen 3 release notes.
// - Qwen 3 4B Q4_K_M: ~2.5 GB, the recommended budget tier.
// - Gemma 4 E4B Q4_K_M: ~5.0 GB, the quality tier. Apache 2.0 since v4.
//
// Qwen GGUFs come from unsloth (the closest thing to a canonical Qwen 3 GGUF
// distribution — the official Qwen org only publishes Q8_0). Sizes verified
// via HF Content-Length on 2026-04-30.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ManagedSpec {
    pub variant: &'static str,    // setting/IPC identifier
    pub label: &'static str,      // human-readable name
    pub file: &'static str,       // on-disk filename
    pub url: &'static str,        // download source
    pub bytes_hint: u64,          // approximate, used as fallback for the progress bar denominator
}

pub const QWEN_1_7B: ManagedSpec = ManagedSpec {
    variant: "qwen-1.7b",
    label: "Qwen 3 1.7B",
    file: "Qwen3-1.7B-Q4_K_M.gguf",
    url: "https://huggingface.co/unsloth/Qwen3-1.7B-GGUF/resolve/main/Qwen3-1.7B-Q4_K_M.gguf",
    bytes_hint: 1_107_409_472,
};

pub const QWEN_4B: ManagedSpec = ManagedSpec {
    variant: "qwen-4b",
    label: "Qwen 3 4B",
    file: "Qwen3-4B-Q4_K_M.gguf",
    url: "https://huggingface.co/unsloth/Qwen3-4B-GGUF/resolve/main/Qwen3-4B-Q4_K_M.gguf",
    bytes_hint: 2_497_281_312,
};

pub const GEMMA_E4B: ManagedSpec = ManagedSpec {
    variant: "gemma-e4b",
    label: "Gemma 4 E4B",
    file: "gemma-4-E4B-it-Q4_K_M.gguf",
    url: "https://huggingface.co/ggml-org/gemma-4-E4B-it-GGUF/resolve/main/gemma-4-E4B-it-Q4_K_M.gguf",
    bytes_hint: 5_100_000_000,
};

pub const ALL_MANAGED: &[ManagedSpec] = &[QWEN_1_7B, QWEN_4B, GEMMA_E4B];

pub fn spec_for_variant(variant: &str) -> Option<&'static ManagedSpec> {
    ALL_MANAGED.iter().find(|s| s.variant == variant)
}

// What model the user has selected. Persisted to settings as a string in the
// format "managed:<variant>" / "path:/abs/path/to.gguf" so we can round-trip
// through the SQLite settings table without a custom encoding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelKind {
    Managed(&'static ManagedSpec),
    Custom(PathBuf),
}

impl ModelKind {
    pub fn from_setting(value: &str) -> Option<Self> {
        if let Some(rest) = value.strip_prefix("managed:") {
            // Backwards compat: "managed:e4b" was the original Gemma E4B
            // identifier before we added Qwen variants.
            let normalized = if rest == "e4b" { "gemma-e4b" } else { rest };
            return spec_for_variant(normalized).map(ModelKind::Managed);
        }
        if let Some(rest) = value.strip_prefix("path:") {
            return Some(ModelKind::Custom(PathBuf::from(rest)));
        }
        None
    }

    pub fn to_setting(&self) -> String {
        match self {
            ModelKind::Managed(spec) => format!("managed:{}", spec.variant),
            ModelKind::Custom(p) => format!("path:{}", p.display()),
        }
    }

    pub fn is_managed(&self) -> bool {
        matches!(self, ModelKind::Managed(_))
    }
}

pub fn managed_dir(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("models").join("llm")
}

pub fn resolve_path(kind: &ModelKind, app_data_dir: &Path) -> PathBuf {
    match kind {
        ModelKind::Managed(spec) => managed_dir(app_data_dir).join(spec.file),
        ModelKind::Custom(p) => p.clone(),
    }
}

pub struct LoadedModel {
    pub path: PathBuf,
    pub kind: ModelKind,
    pub model: Arc<LlamaModel>,
}

pub type SharedContext = Arc<Mutex<Option<LoadedModel>>>;

pub fn new_shared() -> SharedContext {
    Arc::new(Mutex::new(None))
}

pub fn unload(shared: &SharedContext) {
    *shared.lock() = None;
}

// LlamaBackend::init() is process-global state — calling it twice is undefined.
// OnceLock makes the second call a cheap no-op and the first call ~50ms.
fn backend() -> &'static LlamaBackend {
    static B: OnceLock<LlamaBackend> = OnceLock::new();
    B.get_or_init(|| LlamaBackend::init().expect("llama backend init"))
}

// Load the model from disk if it isn't already, otherwise return the cached
// handle. Loading a 5 GB Q4 model takes ~5–10s on M2; subsequent calls are
// instant. Caller must hold a write to `shared` for the duration to avoid
// two threads racing the load and ending up with two copies in RAM.
fn ensure_loaded(
    shared: &SharedContext,
    kind: &ModelKind,
    model_path: &Path,
) -> Result<Arc<LlamaModel>> {
    let mut guard = shared.lock();
    if let Some(loaded) = guard.as_ref() {
        if loaded.path == model_path {
            return Ok(loaded.model.clone());
        }
    }
    if !model_path.exists() {
        return Err(anyhow!(
            "Local LLM model not found at {}",
            model_path.display()
        ));
    }
    // n_gpu_layers=999 is llama.cpp's "offload everything" sentinel — the C
    // side clamps it to the model's actual layer count. On Apple Silicon the
    // Metal backend is built in by default, so this routes through the GPU
    // without any explicit feature flag.
    let params = LlamaModelParams::default().with_n_gpu_layers(999);
    let model = LlamaModel::load_from_file(backend(), model_path, &params)
        .map_err(|e| anyhow!("load llama model: {e}"))?;
    let arc = Arc::new(model);
    *guard = Some(LoadedModel {
        path: model_path.to_path_buf(),
        kind: kind.clone(),
        model: arc.clone(),
    });
    Ok(arc)
}

// Async wrapper around ensure_loaded so callers can await the load on a
// blocking thread without stalling the tokio reactor. Used by polish_transcript
// to surface a "Loading model…" toast before generation starts.
pub async fn prewarm(shared: SharedContext, kind: ModelKind, model_path: PathBuf) -> Result<()> {
    tokio::task::spawn_blocking(move || -> Result<()> {
        ensure_loaded(&shared, &kind, &model_path)?;
        Ok(())
    })
    .await
    .map_err(|e| anyhow!("prewarm task: {e}"))?
}

// Gemma uses a control-token chat template: <start_of_turn>user\n...<end_of_turn>
// followed by <start_of_turn>model\n. There's no real "system" role — convention
// is to prepend the system content to the first user turn. AddBos::Always lets
// the tokenizer add the model's BOS marker before our first control token.
fn format_gemma_prompt(system: &str, user: &str) -> String {
    let user_with_system = if system.is_empty() {
        user.to_string()
    } else {
        format!("{system}\n\n{user}")
    };
    format!("<start_of_turn>user\n{user_with_system}<end_of_turn>\n<start_of_turn>model\n")
}

// Run inference. Loads the model on first call (slow), then generates up to
// `max_tokens` from the chat-formatted prompt. Greedy decoding at temp 0.2 —
// summary/polish work, not creative writing, so determinism beats variety.
pub async fn generate(
    shared: SharedContext,
    kind: ModelKind,
    model_path: PathBuf,
    system: String,
    user: String,
    max_tokens: usize,
) -> Result<String> {
    tokio::task::spawn_blocking(move || -> Result<String> {
        let model = ensure_loaded(&shared, &kind, &model_path)?;
        let prompt = format_gemma_prompt(&system, &user);

        // 8K is enough for ~6K input + 2K output. Bumping past the model's
        // trained context (Gemma 4 supports 128K) costs RAM linearly without
        // a quality win for our short-form polish/summary tasks.
        let n_ctx: u32 = 8192;
        let ctx_params = LlamaContextParams::default().with_n_ctx(NonZeroU32::new(n_ctx));
        let mut ctx = model
            .new_context(backend(), ctx_params)
            .map_err(|e| anyhow!("create llama context: {e}"))?;

        let tokens = model
            .str_to_token(&prompt, AddBos::Always)
            .map_err(|e| anyhow!("tokenize: {e}"))?;
        let prompt_len = tokens.len();
        // Leave a 512-token buffer for generation; if the prompt is bigger than
        // that, the user has fed us a transcript longer than the context window.
        if prompt_len + 512 >= n_ctx as usize {
            return Err(anyhow!(
                "prompt too long: {prompt_len} tokens (limit {} with generation buffer)",
                n_ctx as usize - 512
            ));
        }

        let mut batch = LlamaBatch::new(n_ctx as usize, 1);
        let last_idx = prompt_len - 1;
        for (i, tok) in tokens.iter().enumerate() {
            batch
                .add(*tok, i as i32, &[0], i == last_idx)
                .map_err(|e| anyhow!("batch add: {e}"))?;
        }
        ctx.decode(&mut batch)
            .map_err(|e| anyhow!("decode prompt: {e}"))?;

        let mut sampler = LlamaSampler::chain_simple([
            LlamaSampler::temp(0.2),
            LlamaSampler::greedy(),
        ]);

        let mut out = String::new();
        let mut n_cur = prompt_len as i32;
        let mut produced = 0usize;
        while produced < max_tokens {
            let new_tok = sampler.sample(&ctx, batch.n_tokens() - 1);
            sampler.accept(new_tok);
            if model.is_eog_token(new_tok) {
                break;
            }
            let frag = model
                .token_to_str(new_tok, Special::Tokenize)
                .map_err(|e| anyhow!("detokenize: {e}"))?;
            out.push_str(&frag);
            batch.clear();
            batch
                .add(new_tok, n_cur, &[0], true)
                .map_err(|e| anyhow!("batch add gen: {e}"))?;
            ctx.decode(&mut batch)
                .map_err(|e| anyhow!("decode gen: {e}"))?;
            n_cur += 1;
            produced += 1;
        }
        Ok(out)
    })
    .await
    .map_err(|e| anyhow!("generate task: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_each_managed() {
        for spec in ALL_MANAGED {
            let k = ModelKind::Managed(spec);
            let s = k.to_setting();
            assert_eq!(s, format!("managed:{}", spec.variant));
            assert_eq!(ModelKind::from_setting(&s), Some(k));
        }
    }

    #[test]
    fn round_trip_custom() {
        let k = ModelKind::Custom("/tmp/foo.gguf".into());
        let s = k.to_setting();
        assert_eq!(s, "path:/tmp/foo.gguf");
        assert_eq!(ModelKind::from_setting(&s), Some(k));
    }

    #[test]
    fn invalid_setting_returns_none() {
        assert_eq!(ModelKind::from_setting("garbage"), None);
        assert_eq!(ModelKind::from_setting("managed:xxl"), None);
    }

    #[test]
    fn legacy_managed_e4b_maps_to_gemma() {
        // Older builds persisted "managed:e4b" as the Gemma identifier.
        // Make sure existing settings keep working after the rename.
        let k = ModelKind::from_setting("managed:e4b");
        assert!(matches!(k, Some(ModelKind::Managed(s)) if s.variant == "gemma-e4b"));
    }

    #[test]
    fn resolve_managed_uses_managed_dir() {
        let base = PathBuf::from("/tmp/humla");
        let p = resolve_path(&ModelKind::Managed(&GEMMA_E4B), &base);
        assert_eq!(p, base.join("models").join("llm").join(GEMMA_E4B.file));
    }

    #[test]
    fn resolve_custom_returns_path_as_is() {
        let base = PathBuf::from("/tmp/humla");
        let custom = PathBuf::from("/elsewhere/model.gguf");
        let p = resolve_path(&ModelKind::Custom(custom.clone()), &base);
        assert_eq!(p, custom);
    }

    #[test]
    fn formats_with_system() {
        let p = format_gemma_prompt("you are helpful", "hi");
        assert!(p.contains("<start_of_turn>user\nyou are helpful\n\nhi<end_of_turn>"));
        assert!(p.ends_with("<start_of_turn>model\n"));
    }

    #[test]
    fn formats_without_system() {
        let p = format_gemma_prompt("", "hi");
        assert!(p.contains("<start_of_turn>user\nhi<end_of_turn>"));
        assert!(!p.contains("\n\nhi"));
    }
}
