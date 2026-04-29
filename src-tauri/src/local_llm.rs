// Local LLM module — mirrors local_whisper.rs in shape. A SharedContext holds
// the lazily-loaded model handle; ensure_loaded gates first use; generate runs
// inference on a blocking thread. This file currently defines types + paths;
// model loading lands in a follow-up task.

use anyhow::Result;
use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::sync::Arc;

// Default download targets. Both are sourced from the canonical ggml-org
// repos on HuggingFace. E2B has no Q4_K_M published — Q8_0 of a 2B model is
// roughly the same disk footprint as Q4 of a 4B and preserves more quality,
// so we use it as the "small" tier.
pub const E2B_FILE: &str = "gemma-4-E2B-it-Q8_0.gguf";
pub const E2B_URL: &str =
    "https://huggingface.co/ggml-org/gemma-4-E2B-it-GGUF/resolve/main/gemma-4-E2B-it-Q8_0.gguf";
pub const E2B_BYTES_HINT: u64 = 2_900_000_000;

pub const E4B_FILE: &str = "gemma-4-E4B-it-Q4_K_M.gguf";
pub const E4B_URL: &str =
    "https://huggingface.co/ggml-org/gemma-4-E4B-it-GGUF/resolve/main/gemma-4-E4B-it-Q4_K_M.gguf";
pub const E4B_BYTES_HINT: u64 = 5_100_000_000;

// What model the user has selected. Persisted to settings as a string in the
// format "managed:e2b" / "managed:e4b" / "path:/abs/path/to.gguf" so we can
// round-trip it through the SQLite settings table without a custom encoding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelKind {
    GemmaE2b,
    GemmaE4b,
    Custom(PathBuf),
}

impl ModelKind {
    pub fn from_setting(value: &str) -> Option<Self> {
        if let Some(rest) = value.strip_prefix("managed:") {
            return match rest {
                "e2b" => Some(ModelKind::GemmaE2b),
                "e4b" => Some(ModelKind::GemmaE4b),
                _ => None,
            };
        }
        if let Some(rest) = value.strip_prefix("path:") {
            return Some(ModelKind::Custom(PathBuf::from(rest)));
        }
        None
    }

    pub fn to_setting(&self) -> String {
        match self {
            ModelKind::GemmaE2b => "managed:e2b".into(),
            ModelKind::GemmaE4b => "managed:e4b".into(),
            ModelKind::Custom(p) => format!("path:{}", p.display()),
        }
    }

    pub fn is_managed(&self) -> bool {
        matches!(self, ModelKind::GemmaE2b | ModelKind::GemmaE4b)
    }
}

pub fn managed_dir(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("models").join("llm")
}

pub fn resolve_path(kind: &ModelKind, app_data_dir: &Path) -> PathBuf {
    match kind {
        ModelKind::GemmaE2b => managed_dir(app_data_dir).join(E2B_FILE),
        ModelKind::GemmaE4b => managed_dir(app_data_dir).join(E4B_FILE),
        ModelKind::Custom(p) => p.clone(),
    }
}

pub struct LoadedModel {
    pub path: PathBuf,
    pub kind: ModelKind,
    // Model handle lands in the next task once we wire llama-cpp-2.
}

pub type SharedContext = Arc<Mutex<Option<LoadedModel>>>;

pub fn new_shared() -> SharedContext {
    Arc::new(Mutex::new(None))
}

pub fn unload(shared: &SharedContext) {
    *shared.lock() = None;
}

#[allow(dead_code)] // wired up in Task 5
pub async fn prewarm(_shared: SharedContext, _kind: ModelKind, _model_path: PathBuf) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_managed() {
        let k = ModelKind::GemmaE4b;
        let s = k.to_setting();
        assert_eq!(s, "managed:e4b");
        assert_eq!(ModelKind::from_setting(&s), Some(k));
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
    fn resolve_managed_uses_managed_dir() {
        let base = PathBuf::from("/tmp/humla");
        let p = resolve_path(&ModelKind::GemmaE4b, &base);
        assert_eq!(p, base.join("models").join("llm").join(E4B_FILE));
    }

    #[test]
    fn resolve_custom_returns_path_as_is() {
        let base = PathBuf::from("/tmp/humla");
        let custom = PathBuf::from("/elsewhere/model.gguf");
        let p = resolve_path(&ModelKind::Custom(custom.clone()), &base);
        assert_eq!(p, custom);
    }
}
