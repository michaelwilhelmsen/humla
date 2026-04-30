// Local LLM module — mirrors local_whisper.rs in shape. A SharedContext holds
// the lazily-loaded model handle; ensure_loaded gates first use; generate runs
// inference on a blocking thread. This file currently defines types + paths;
// model loading lands in a follow-up task.

// In-process llama-cpp-2 was reverted: linking it alongside whisper-rs put
// two copies of GGML in the binary, and Metal backend registration aborted
// during whisper init. The model lifecycle UI (download / scan / select /
// path display) stays intact via the constants and ModelKind below; the
// `generate` and `prewarm` functions now return a "not yet wired" error
// until the sidecar implementation lands.
use anyhow::{anyhow, Result};
use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
}

pub type SharedContext = Arc<Mutex<Option<LoadedModel>>>;

pub fn new_shared() -> SharedContext {
    Arc::new(Mutex::new(None))
}

pub fn unload(shared: &SharedContext) {
    *shared.lock() = None;
}

const NOT_WIRED: &str =
    "Local summarization is not wired yet. The plumbing is in place; \
     inference will arrive once the sidecar process is in place. \
     Pick Cloud (OpenAI) for now.";

pub async fn prewarm(_shared: SharedContext, _kind: ModelKind, _model_path: PathBuf) -> Result<()> {
    Err(anyhow!(NOT_WIRED))
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

// Stub: in-process inference is not wired yet (see file-top comment).
// Sidecar implementation will replace this with a subprocess invocation.
#[allow(clippy::too_many_arguments)]
pub async fn generate(
    _shared: SharedContext,
    _kind: ModelKind,
    _model_path: PathBuf,
    _system: String,
    _user: String,
    _max_tokens: usize,
) -> Result<String> {
    Err(anyhow!(NOT_WIRED))
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
