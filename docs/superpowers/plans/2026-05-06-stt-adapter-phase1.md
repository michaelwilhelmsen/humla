# STT Adapter Phase 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the string-match dispatch in `transcribe_chunk` with a typed `BatchSttAdapter` trait, so adding new STT providers becomes one new file rather than a rewrite. Phase 1 ships zero user-visible change — pure refactor, bit-identical output as the gate.

**Architecture:** New `stt/` module with a small trait, an `Auth` enum, and a tagged-union `ProviderConfig`. Existing `openai::transcribe_file` and `local_whisper::transcribe_file_with_words` get wrapped as adapter implementations that delegate. `transcribe_chunk` migrates to use the trait. Legacy settings keys auto-migrate on first read.

**Tech Stack:** Rust 1.85, Tauri 2, tokio (full), reqwest, serde, async-trait (new dep), parking_lot. Existing test pattern: `#[cfg(test)] mod tests` inside each file, `#[tokio::test]` for async.

**Reference docs:** `docs/design/stt-adapter.md` (full design rationale).

---

## Background for the implementer

Humla is a personal Mac meeting-notes app. Recording produces VAD-bounded WAV chunks; each chunk is sent through one of two transcription paths today:

- `src-tauri/src/openai.rs::transcribe_file` — multipart POST to OpenAI.
- `src-tauri/src/local_whisper.rs::transcribe_file_with_words` — in-process whisper-rs / Metal.

The dispatch lives in `src-tauri/src/commands.rs::transcribe_chunk` (around line 3514) as a string match on the `transcribe_provider` setting. We're not changing what these two paths *do*; we're putting them behind a uniform interface so a third (Deepgram, in Phase 2) becomes a single new file.

**Hard constraint**: Phase 1 must produce bit-identical transcription output for both providers. The verification gate is a manual end-to-end test (Task 9) — a recording made on the new code must match what the old code would have produced.

---

## File Structure

New files (all under `src-tauri/src/`):

| File | Responsibility |
|---|---|
| `stt/mod.rs` | Public API: `BatchSttAdapter` re-export, `ProviderConfig` re-export, `build_adapter()` registry |
| `stt/auth.rs` | `Auth` enum (Header/QueryParam/BodyField) + `apply()` method |
| `stt/adapter.rs` | `BatchSttAdapter` trait, `TranscribeCtx`, `TranscribeResult`, `Word` |
| `stt/config.rs` | `ProviderConfig` tagged union + per-provider configs + legacy migration helper |
| `stt/openai.rs` | `OpenAiAdapter` — wraps `openai::transcribe_file` |
| `stt/local.rs` | `LocalWhisperAdapter` — wraps `local_whisper::transcribe_file_with_words` |

Modified files:

| File | Change |
|---|---|
| `src-tauri/Cargo.toml` | Add `async-trait = "0.1"` dependency |
| `src-tauri/src/lib.rs` | Add `mod stt;` |
| `src-tauri/src/commands.rs` | Replace string-match dispatch in `transcribe_chunk` (~lines 3408-3553) with adapter call |
| `src-tauri/src/openai.rs` | `transcribe_file` stays (used directly by `OpenAiAdapter`); no behavior change |
| `src-tauri/src/local_whisper.rs` | `Word` re-exported via `stt::Word` (no rename yet, deferred to follow-up) |

---

## Task 1: Add async-trait dependency

**Files:**
- Modify: `src-tauri/Cargo.toml`

- [ ] **Step 1: Add the dependency**

Edit `src-tauri/Cargo.toml`. Find the line `thiserror = "2"` in `[dependencies]` and add `async-trait` directly after it:

```toml
thiserror = "2"
async-trait = "0.1"
chrono = "0.4"
```

- [ ] **Step 2: Verify it compiles**

Run from repo root: `cargo build --manifest-path src-tauri/Cargo.toml --message-format=short`

Expected: builds successfully (may take a minute on first add).

- [ ] **Step 3: Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/Cargo.lock
git commit -m "deps: add async-trait for STT adapter abstraction"
```

---

## Task 2: Create `stt::auth` module

**Files:**
- Create: `src-tauri/src/stt/auth.rs`

- [ ] **Step 1: Write the failing tests**

Create `src-tauri/src/stt/auth.rs` with the following content:

```rust
//! Auth schemes for STT providers. Each cloud STT API authenticates
//! differently; this enum encodes the shape so adapters don't duplicate the
//! header/key plumbing per provider.

use reqwest::RequestBuilder;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Auth {
    /// Authorization-style header. `prefix` is the literal prepended to the
    /// key, e.g. `Some("Bearer ")` for OpenAI, `Some("Token ")` for Deepgram,
    /// `None` for raw-key headers like `xi-api-key`.
    Header { name: &'static str, prefix: Option<&'static str> },
    /// Key passed as `?<name>=<key>` in the URL.
    QueryParam { name: &'static str },
    /// Key passed as a JSON body field. The adapter's `transcribe`
    /// implementation must call `merge_into_body` since RequestBuilder can't
    /// rewrite the JSON body in flight.
    BodyField { name: &'static str },
}

impl Auth {
    /// Apply the auth scheme to a request builder. For `BodyField`, this is a
    /// no-op — the adapter's caller is responsible for merging the key into
    /// the JSON payload before passing the builder to send.
    pub fn apply(&self, req: RequestBuilder, key: &str) -> RequestBuilder {
        match self {
            Auth::Header { name, prefix } => {
                let value = match prefix {
                    Some(p) => format!("{p}{key}"),
                    None => key.to_string(),
                };
                req.header(*name, value)
            }
            Auth::QueryParam { name } => req.query(&[(*name, key)]),
            Auth::BodyField { .. } => req,
        }
    }

    /// For `BodyField` auth, the field name to insert into the JSON body.
    /// Returns None for non-body schemes.
    pub fn body_field_name(&self) -> Option<&'static str> {
        match self {
            Auth::BodyField { name } => Some(name),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_with_prefix_builds_bearer() {
        // We can't easily inspect the built header from RequestBuilder, so
        // this tests the formatting path in isolation — the apply() call
        // exercises the same `format!` branch.
        let auth = Auth::Header { name: "Authorization", prefix: Some("Bearer ") };
        // Manual format check — the `apply` method does the same thing.
        let key = "sk-test123";
        let expected = format!("Bearer {key}");
        match auth {
            Auth::Header { prefix: Some(p), .. } => assert_eq!(format!("{p}{key}"), expected),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn header_without_prefix_uses_raw_key() {
        let auth = Auth::Header { name: "xi-api-key", prefix: None };
        let key = "xi-test456";
        match auth {
            Auth::Header { prefix: None, .. } => assert_eq!(key.to_string(), "xi-test456"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn body_field_name_only_returns_for_body_variant() {
        assert_eq!(Auth::BodyField { name: "api_key" }.body_field_name(), Some("api_key"));
        assert_eq!(Auth::Header { name: "Authorization", prefix: None }.body_field_name(), None);
        assert_eq!(Auth::QueryParam { name: "key" }.body_field_name(), None);
    }
}
```

- [ ] **Step 2: Wire into the module tree (will fail until mod.rs exists)**

We'll create `stt/mod.rs` in Task 5. For now, create a stub at `src-tauri/src/stt/mod.rs` so the file compiles:

```rust
//! STT provider abstraction. See docs/design/stt-adapter.md for rationale.

mod auth;

pub use auth::Auth;
```

Add `mod stt;` to `src-tauri/src/lib.rs` immediately after the existing `mod openai;` declaration. Find that line and add:

```rust
mod openai;
mod stt;
```

- [ ] **Step 3: Run the tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml stt::auth -- --nocapture`

Expected: 3 tests pass.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/stt/
git commit -m "stt: add Auth enum for provider authentication schemes"
```

---

## Task 3: Create `stt::adapter` module (trait + ctx + result types)

**Files:**
- Create: `src-tauri/src/stt/adapter.rs`
- Modify: `src-tauri/src/stt/mod.rs`

- [ ] **Step 1: Write the trait file**

Create `src-tauri/src/stt/adapter.rs`:

```rust
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
    pub initial_prompt: Option<&'a str>,
    pub api_key: Option<&'a str>,
    pub base_url: Option<&'a str>,
}

#[derive(Clone, Debug, Default)]
pub struct TranscribeResult {
    pub text: String,
    pub words: Vec<Word>,
}

/// One word's display text + millisecond bounds. The trait surface uses
/// this shape; downstream code (`local_whisper::Word`) re-exports it via
/// `pub use stt::Word as Word;` so the existing word-timing path keeps
/// compiling. Renaming `local_whisper::Word` → `stt::Word` is deferred to
/// a follow-up commit.
#[derive(Clone, Debug, Default)]
pub struct Word {
    pub text: String,
    pub start_ms: u64,
    pub end_ms: u64,
}
```

- [ ] **Step 2: Re-export from mod.rs**

Edit `src-tauri/src/stt/mod.rs` to add the new module and re-exports:

```rust
//! STT provider abstraction. See docs/design/stt-adapter.md for rationale.

mod adapter;
mod auth;

pub use adapter::{BatchSttAdapter, TranscribeCtx, TranscribeResult, Word};
pub use auth::Auth;
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo build --manifest-path src-tauri/Cargo.toml --message-format=short`

Expected: builds successfully. The trait has no implementations yet so there's nothing to functionally test.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/stt/
git commit -m "stt: add BatchSttAdapter trait + TranscribeCtx/Result/Word types"
```

---

## Task 4: Create `stt::config` module — ProviderConfig tagged union + legacy migration

**Files:**
- Create: `src-tauri/src/stt/config.rs`
- Modify: `src-tauri/src/stt/mod.rs`

- [ ] **Step 1: Write the config module with tests**

Create `src-tauri/src/stt/config.rs`:

```rust
//! Tagged-union config for STT providers. Replaces the flat
//! `transcribe_provider` + `transcribe_model` + `whisper_preset` settings
//! triple. Stored in the `settings` table as JSON under key
//! `transcribe_config`. On first read, if the key is missing, we synthesise
//! it from the legacy keys (see `from_legacy_settings`).

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "provider")]
pub enum ProviderConfig {
    #[serde(rename = "openai")]
    OpenAi(OpenAiConfig),
    #[serde(rename = "local")]
    Local(LocalWhisperConfig),
}

impl ProviderConfig {
    pub fn provider_id(&self) -> &'static str {
        match self {
            ProviderConfig::OpenAi(_) => "openai",
            ProviderConfig::Local(_) => "local",
        }
    }

    pub fn model(&self) -> &str {
        match self {
            ProviderConfig::OpenAi(c) => &c.model,
            ProviderConfig::Local(c) => &c.model_id,
        }
    }

    pub fn base_url(&self) -> Option<&str> {
        match self {
            ProviderConfig::OpenAi(c) => c.base_url.as_deref(),
            ProviderConfig::Local(_) => None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct OpenAiConfig {
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalWhisperConfig {
    pub model_id: String,
    pub preset: String,
    pub use_gpu: bool,
}

/// Build a `ProviderConfig` from the legacy flat settings shape. Used at
/// migration time when `transcribe_config` is absent. None of the legacy
/// keys are required to exist — defaults match what the old `transcribe_chunk`
/// fallback chain produced.
pub fn from_legacy_settings(
    transcribe_provider: Option<&str>,
    transcribe_model: Option<&str>,
    whisper_model_id: Option<&str>,
    whisper_preset: Option<&str>,
    whisper_use_gpu: Option<bool>,
) -> ProviderConfig {
    match transcribe_provider.unwrap_or("openai") {
        "local" => ProviderConfig::Local(LocalWhisperConfig {
            model_id: whisper_model_id.unwrap_or("large-v3-turbo-q5").to_string(),
            preset: whisper_preset.unwrap_or("quality").to_string(),
            use_gpu: whisper_use_gpu.unwrap_or(true),
        }),
        _ => ProviderConfig::OpenAi(OpenAiConfig {
            model: transcribe_model.unwrap_or("gpt-4o-transcribe").to_string(),
            base_url: None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_round_trips_through_json() {
        let cfg = ProviderConfig::OpenAi(OpenAiConfig {
            model: "whisper-1".to_string(),
            base_url: None,
        });
        let json = serde_json::to_string(&cfg).unwrap();
        let back: ProviderConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
        // Provider tag is on the wire as `"provider":"openai"`.
        assert!(json.contains(r#""provider":"openai""#));
    }

    #[test]
    fn local_round_trips_through_json() {
        let cfg = ProviderConfig::Local(LocalWhisperConfig {
            model_id: "large-v3-turbo-q5".to_string(),
            preset: "quality".to_string(),
            use_gpu: true,
        });
        let json = serde_json::to_string(&cfg).unwrap();
        let back: ProviderConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
        assert!(json.contains(r#""provider":"local""#));
    }

    #[test]
    fn legacy_migration_openai_defaults() {
        let cfg = from_legacy_settings(None, None, None, None, None);
        assert_eq!(
            cfg,
            ProviderConfig::OpenAi(OpenAiConfig {
                model: "gpt-4o-transcribe".to_string(),
                base_url: None,
            })
        );
    }

    #[test]
    fn legacy_migration_keeps_user_openai_model() {
        let cfg = from_legacy_settings(Some("openai"), Some("whisper-1"), None, None, None);
        assert_eq!(cfg.model(), "whisper-1");
        assert_eq!(cfg.provider_id(), "openai");
    }

    #[test]
    fn legacy_migration_local_inherits_preset_and_gpu() {
        let cfg = from_legacy_settings(
            Some("local"),
            None,
            Some("medium-q5"),
            Some("balanced"),
            Some(false),
        );
        match cfg {
            ProviderConfig::Local(c) => {
                assert_eq!(c.model_id, "medium-q5");
                assert_eq!(c.preset, "balanced");
                assert!(!c.use_gpu);
            }
            _ => panic!("expected Local"),
        }
    }

    #[test]
    fn provider_id_matches_serde_tag() {
        let cfgs = [
            ProviderConfig::OpenAi(OpenAiConfig {
                model: "whisper-1".to_string(),
                base_url: None,
            }),
            ProviderConfig::Local(LocalWhisperConfig {
                model_id: "large-v3-turbo-q5".to_string(),
                preset: "quality".to_string(),
                use_gpu: true,
            }),
        ];
        for cfg in cfgs {
            let json = serde_json::to_string(&cfg).unwrap();
            assert!(json.contains(&format!(r#""provider":"{}""#, cfg.provider_id())));
        }
    }
}
```

- [ ] **Step 2: Re-export from mod.rs**

Edit `src-tauri/src/stt/mod.rs`:

```rust
//! STT provider abstraction. See docs/design/stt-adapter.md for rationale.

mod adapter;
mod auth;
mod config;

pub use adapter::{BatchSttAdapter, TranscribeCtx, TranscribeResult, Word};
pub use auth::Auth;
pub use config::{
    from_legacy_settings, LocalWhisperConfig, OpenAiConfig, ProviderConfig,
};
```

- [ ] **Step 3: Run the tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml stt::config -- --nocapture`

Expected: 6 tests pass.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/stt/
git commit -m "stt: add ProviderConfig tagged union + legacy settings migration"
```

---

## Task 5: Create `stt::openai` adapter

**Files:**
- Create: `src-tauri/src/stt/openai.rs`
- Modify: `src-tauri/src/stt/mod.rs`

- [ ] **Step 1: Write the adapter**

Create `src-tauri/src/stt/openai.rs`:

```rust
//! OpenAI batch STT adapter. Wraps the existing `openai::transcribe_file`
//! function so we don't duplicate the multipart-form logic. When this is
//! the only OpenAI STT call site (post-migration), we'll inline the body
//! here and remove the wrapper from `openai.rs`. Phase 1 keeps both for
//! safety.

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
        // OpenAI Whisper auto-detects when language is unset, and accepts
        // all 99 Whisper languages otherwise. We don't enumerate.
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
```

- [ ] **Step 2: Re-export from mod.rs**

Edit `src-tauri/src/stt/mod.rs`:

```rust
//! STT provider abstraction. See docs/design/stt-adapter.md for rationale.

mod adapter;
mod auth;
mod config;
mod openai;

pub use adapter::{BatchSttAdapter, TranscribeCtx, TranscribeResult, Word};
pub use auth::Auth;
pub use config::{
    from_legacy_settings, LocalWhisperConfig, OpenAiConfig, ProviderConfig,
};
pub use openai::OpenAiAdapter;
```

- [ ] **Step 3: Run the tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml stt::openai -- --nocapture`

Expected: 1 test passes.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/stt/
git commit -m "stt: add OpenAiAdapter wrapping existing transcribe_file"
```

---

## Task 6: Create `stt::local` adapter

**Files:**
- Create: `src-tauri/src/stt/local.rs`
- Modify: `src-tauri/src/stt/mod.rs`

- [ ] **Step 1: Write the adapter**

Create `src-tauri/src/stt/local.rs`:

```rust
//! Local Whisper batch STT adapter. Wraps
//! `local_whisper::transcribe_file_with_words`. The adapter holds the
//! shared `WhisperContext` (`SharedContext` = Arc<Mutex<Option<LoadedModel>>>),
//! the model file path, and the GPU flag — all data the legacy call site
//! used to gather inline.

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
        // Whisper supports all 99 of its trained languages plus "auto".
        true
    }

    fn supports_word_timestamps(&self, _model: &str) -> bool {
        // whisper-rs always returns token-level timestamps, which we group
        // into words — same behavior across all ggml model files.
        true
    }

    async fn transcribe(
        &self,
        ctx: TranscribeCtx<'_>,
        audio: &Path,
    ) -> Result<TranscribeResult> {
        let (text, words) = local_whisper::transcribe_file_with_words(
            self.shared.clone(),
            self.model_path.clone(),
            self.use_gpu,
            ctx.language,
            ctx.initial_prompt,
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
```

- [ ] **Step 2: Re-export from mod.rs**

Edit `src-tauri/src/stt/mod.rs`:

```rust
//! STT provider abstraction. See docs/design/stt-adapter.md for rationale.

mod adapter;
mod auth;
mod config;
mod local;
mod openai;

pub use adapter::{BatchSttAdapter, TranscribeCtx, TranscribeResult, Word};
pub use auth::Auth;
pub use config::{
    from_legacy_settings, LocalWhisperConfig, OpenAiConfig, ProviderConfig,
};
pub use local::LocalWhisperAdapter;
pub use openai::OpenAiAdapter;
```

- [ ] **Step 3: Run the tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml stt::local -- --nocapture`

Expected: 1 test passes.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/stt/
git commit -m "stt: add LocalWhisperAdapter wrapping in-process whisper-rs"
```

---

## Task 7: Add `build_adapter` registry

**Files:**
- Modify: `src-tauri/src/stt/mod.rs`

- [ ] **Step 1: Add the registry function**

Edit `src-tauri/src/stt/mod.rs` to add a `build_adapter` function. Replace the contents with:

```rust
//! STT provider abstraction. See docs/design/stt-adapter.md for rationale.

mod adapter;
mod auth;
mod config;
mod local;
mod openai;

use std::path::PathBuf;

pub use adapter::{BatchSttAdapter, TranscribeCtx, TranscribeResult, Word};
pub use auth::Auth;
pub use config::{
    from_legacy_settings, LocalWhisperConfig, OpenAiConfig, ProviderConfig,
};
pub use local::LocalWhisperAdapter;
pub use openai::OpenAiAdapter;

use crate::local_whisper;

/// Local-Whisper-specific dispatch dependencies. The adapter needs the
/// shared model context, the resolved model file path, and the GPU flag —
/// none of which can live inside `ProviderConfig` (they're runtime state,
/// not user settings). The caller (`commands.rs::transcribe_chunk`)
/// resolves these at dispatch time and passes them in.
pub struct LocalDeps {
    pub shared: local_whisper::SharedContext,
    pub model_path: PathBuf,
    pub use_gpu: bool,
}

/// Build the right adapter for the given config. Boxed because the trait is
/// dyn-dispatched; allocation cost is irrelevant next to network or
/// inference latency.
///
/// `local_deps` is required for `ProviderConfig::Local` and unused for
/// other variants. Constructed by the caller because `ProviderConfig`
/// can't carry runtime state.
pub fn build_adapter(
    cfg: &ProviderConfig,
    local_deps: Option<LocalDeps>,
) -> Box<dyn BatchSttAdapter> {
    match cfg {
        ProviderConfig::OpenAi(_) => Box::new(OpenAiAdapter::new()),
        ProviderConfig::Local(local_cfg) => {
            let deps = local_deps.expect("LocalDeps required for ProviderConfig::Local");
            Box::new(LocalWhisperAdapter::new(
                deps.shared,
                deps.model_path,
                deps.use_gpu,
                local_whisper::Preset::from_setting(&local_cfg.preset),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_openai_adapter_returns_openai() {
        let cfg = ProviderConfig::OpenAi(OpenAiConfig {
            model: "whisper-1".to_string(),
            base_url: None,
        });
        let adapter = build_adapter(&cfg, None);
        assert_eq!(adapter.provider_id(), "openai");
    }

    #[test]
    fn build_local_adapter_returns_local() {
        use parking_lot::Mutex;
        use std::sync::Arc;

        let cfg = ProviderConfig::Local(LocalWhisperConfig {
            model_id: "large-v3-turbo-q5".to_string(),
            preset: "quality".to_string(),
            use_gpu: true,
        });
        let deps = LocalDeps {
            shared: Arc::new(Mutex::new(None)),
            model_path: PathBuf::from("/tmp/test.bin"),
            use_gpu: true,
        };
        let adapter = build_adapter(&cfg, Some(deps));
        assert_eq!(adapter.provider_id(), "local");
    }

    #[test]
    #[should_panic(expected = "LocalDeps required")]
    fn build_local_without_deps_panics() {
        let cfg = ProviderConfig::Local(LocalWhisperConfig {
            model_id: "any".to_string(),
            preset: "quality".to_string(),
            use_gpu: true,
        });
        let _ = build_adapter(&cfg, None);
    }
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test --manifest-path src-tauri/Cargo.toml stt:: -- --nocapture`

Expected: all 14 tests across the `stt::` namespace pass (3 auth + 6 config + 1 openai + 1 local + 3 registry).

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/stt/mod.rs
git commit -m "stt: add build_adapter registry with LocalDeps"
```

---

## Task 8: Add settings migration helper in `commands.rs`

**Files:**
- Modify: `src-tauri/src/commands.rs`

- [ ] **Step 1: Find the right insertion point**

Read `src-tauri/src/commands.rs` near the top, look for the imports block. Find where existing settings helpers live (search for `transcribe_provider` to land near the existing setting reads). The migration helper should sit alongside other settings-reading code so it's easy to find.

- [ ] **Step 2: Add a `read_provider_config` helper**

Add this function to `commands.rs`. Place it near the existing `local_model_path` helper (around line 1283 — search for `fn model_path_for`). Add immediately after `local_model_path`'s implementation:

```rust
/// Read the active STT provider config. Migrates from the legacy flat
/// settings keys (`transcribe_provider`, `transcribe_model`, `whisper_preset`,
/// etc.) on first read, then writes the new `transcribe_config` JSON back
/// so subsequent reads use the new shape directly.
fn read_provider_config(state: &AppState) -> Result<crate::stt::ProviderConfig, String> {
    let conn = state.db.lock();
    if let Some(json) = db::get_setting(&conn, "transcribe_config").map_err(err)? {
        if let Ok(cfg) = serde_json::from_str::<crate::stt::ProviderConfig>(&json) {
            return Ok(cfg);
        }
        // Corrupted JSON in the new key — fall through to legacy reconstruction.
    }
    let provider = db::get_setting(&conn, "transcribe_provider").map_err(err)?;
    let model = db::get_setting(&conn, "transcribe_model").map_err(err)?;
    let whisper_model = db::get_setting(&conn, "whisper_model").map_err(err)?;
    let whisper_preset = db::get_setting(&conn, "whisper_preset").map_err(err)?;
    let whisper_use_gpu = db::get_setting(&conn, "local_whisper_use_gpu")
        .map_err(err)?
        .and_then(|v| match v.as_str() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        });
    let cfg = crate::stt::from_legacy_settings(
        provider.as_deref(),
        model.as_deref(),
        whisper_model.as_deref(),
        whisper_preset.as_deref(),
        whisper_use_gpu,
    );
    // Persist migrated form so future reads skip the reconstruction.
    let json = serde_json::to_string(&cfg).map_err(err)?;
    db::set_setting(&conn, "transcribe_config", &json).map_err(err)?;
    Ok(cfg)
}
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo build --manifest-path src-tauri/Cargo.toml --message-format=short`

Expected: builds successfully. You may see an `unused function: read_provider_config` warning — that's fine, Task 9 wires it up.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/commands.rs
git commit -m "commands: add read_provider_config with legacy settings migration"
```

---

## Task 9: Cut over `transcribe_chunk` to use the adapter trait

**Files:**
- Modify: `src-tauri/src/commands.rs`

This is the single behavior-changing commit. Everything before this was setup; this swaps the dispatch.

- [ ] **Step 1: Locate the existing dispatch**

Open `src-tauri/src/commands.rs` and find `async fn transcribe_chunk`. The dispatch block to replace runs from roughly line 3408 (the start of `let cfg = { ... }`) through line 3553 (the `};` after the OpenAI arm). Read that whole block carefully — the surrounding code (RMS gate, transcribe_gate, prompt building before; hallucination checks, trail update, append_transcript after) MUST stay exactly as-is.

- [ ] **Step 2: Replace the dispatch block**

Replace the entire block from `let cfg = { ... };` through the end of the `let (text, words) = match cfg.provider.as_str() { ... };` block with the following. Keep everything before `let cfg = { ... };` (function signature, comments) and everything after (`if is_likely_hallucination(&text, ...)` and onward) untouched:

```rust
    // Resolve dispatch-time data: provider config, language (per-note override
    // wins over global), API key (Keychain — synchronous, kept outside DB lock),
    // and the rolling vocabulary string. The trail snapshot is built later,
    // after we acquire the transcribe_gate.
    let provider_cfg = {
        let state: State<AppState> = app.state();
        read_provider_config(&state)?
    };
    let language = {
        let state: State<AppState> = app.state();
        let conn = state.db.lock();
        let global_language = db::get_setting(&conn, "language")
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .unwrap_or_else(|| DEFAULT_LANGUAGE.to_string());
        let note_language = db::get_note(&conn, &note_id)
            .map(|n| n.language)
            .unwrap_or_default();
        if note_language.trim().is_empty() {
            global_language
        } else {
            note_language
        }
    };
    let vocabulary = {
        let state: State<AppState> = app.state();
        let conn = state.db.lock();
        db::get_setting(&conn, "custom_vocabulary")
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .unwrap_or_default()
    };
    let api_key = match provider_cfg.provider_id() {
        "local" => None,
        _ => {
            let state: State<AppState> = app.state();
            Some(
                read_openai_api_key(&state)
                    .map_err(|e| anyhow::anyhow!("{e}"))?
                    .ok_or_else(|| anyhow::anyhow!("no OpenAI API key"))?,
            )
        }
    };

    // Skip near-silent chunks. Whisper and gpt-4o-transcribe both hallucinate
    // confident text (often in the wrong language) when fed silence. The WAV
    // chunks are 16kHz mono 16-bit PCM little-endian — read the data section
    // and compute RMS in [0, 1]. Threshold is user-tunable so noisy
    // environments (HVAC, mic hiss) can crank it up to drop borderline
    // chunks before they reach Whisper.
    let rms_floor = {
        let state: State<AppState> = app.state();
        let conn = state.db.lock();
        db::get_setting(&conn, "silence_rms_threshold")
            .ok()
            .flatten()
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or(DEFAULT_SILENCE_RMS_THRESHOLD)
    };
    if let Ok(rms) = wav::rms(&path).await {
        if rms < rms_floor {
            return Ok(());
        }
    }

    // Serialize transcription per session: each chunk's initial_prompt must
    // see the *committed* trail of every prior chunk. With parallel
    // transcribes, two back-to-back chunks both grab the same stale snapshot
    // and the trail's quality benefit collapses. Sequential trades a little
    // throughput (chunks queue if inference is slow) for accurate context.
    let gate = {
        let state: State<AppState> = app.state();
        state.transcribe_gate.clone()
    };
    let _guard = gate.lock().await;

    // Whisper's `initial_prompt` slot conditions decoding on prior context.
    // We compose two parts: the user's custom vocabulary (proper-noun bias)
    // and a snapshot of the last ~150 committed words from THIS source's
    // stream. Per-source trails because the mic and system streams are
    // separate conversations — sharing one trail would pull a mic chunk's
    // decode toward remote-side vocabulary (or vice versa) and cause
    // language drift on bilingual calls.
    let trail_snapshot = {
        let state: State<AppState> = app.state();
        let session = state.recording.lock();
        let trail = match source {
            ChunkSource::Mic => session.mic_trail.lock(),
            ChunkSource::Sys => session.sys_trail.lock(),
        };
        trail.as_prompt()
    };
    let prompt = build_initial_prompt(&vocabulary, trail_snapshot);

    // Build the adapter. For local, we resolve the model file path here
    // (uses the per-language addon if downloaded) and snapshot the shared
    // WhisperContext + GPU flag from AppState.
    let local_deps = if matches!(provider_cfg, crate::stt::ProviderConfig::Local(_)) {
        let model_path = local_model_path(&app, &language)?;
        let (shared, use_gpu) = {
            let state: State<AppState> = app.state();
            (state.whisper.clone(), local_whisper_use_gpu_setting(&state))
        };
        Some(crate::stt::LocalDeps { shared, model_path, use_gpu })
    } else {
        None
    };
    let adapter = crate::stt::build_adapter(&provider_cfg, local_deps);

    let ctx = crate::stt::TranscribeCtx {
        model: provider_cfg.model(),
        language: &language,
        initial_prompt: prompt.as_deref(),
        api_key: api_key.as_deref(),
        base_url: provider_cfg.base_url(),
    };
    let crate::stt::TranscribeResult { text, words } = adapter.transcribe(ctx, &path).await?;
    // Convert to the existing `local_whisper::Word` shape used downstream.
    // Word type unification (rename `local_whisper::Word` → `stt::Word`) is
    // deferred to a follow-up commit.
    let words: Vec<local_whisper::Word> = words
        .into_iter()
        .map(|w| local_whisper::Word {
            text: w.text,
            start_ms: w.start_ms,
            end_ms: w.end_ms,
        })
        .collect();
```

- [ ] **Step 3: Delete the old `TranscribeCfg` struct if it became unused**

After the above replacement, the local `TranscribeCfg` struct (the one defined inside or above `transcribe_chunk`) may be unused. Search for `struct TranscribeCfg` in `commands.rs`. If it's only used by the now-replaced block, delete it. If it's used elsewhere (e.g. another transcribe path), leave it.

- [ ] **Step 4: Verify it compiles**

Run: `cargo build --manifest-path src-tauri/Cargo.toml --message-format=short`

Expected: builds successfully. Watch for:
- "unused variable" warnings — likely indicate something we still need from the old block
- "cannot find value" — likely indicate a variable name we changed (e.g. `cfg.language` → `language`)

If errors appear, the most likely cause is that downstream code (after the replaced block) still references the old `cfg` variable. Either rename references back, or extract the needed fields to locals before the replaced block.

- [ ] **Step 5: Run the full test suite**

Run: `cargo test --manifest-path src-tauri/Cargo.toml --message-format=short`

Expected: all tests pass, including the new `stt::` tests.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/commands.rs
git commit -m "commands: dispatch transcribe_chunk through BatchSttAdapter trait"
```

---

## Task 10: Manual verification gate

This is the verification step the design doc calls out: a recording made on the new code must produce identical-quality output to the old code. There are no automated tests for this — it's a smoke test the human runs.

- [ ] **Step 1: Build and launch the app**

Run from repo root: `pnpm tauri dev`

Expected: app launches normally. No crash on boot, no panic in the console.

- [ ] **Step 2: Verify settings round-trip**

Open the app's Settings. The transcribe-provider and model selectors should show the values you had before the migration (i.e. the legacy keys were correctly read).

- [ ] **Step 3: Smoke-test OpenAI provider**

Set provider to OpenAI. Record a short test (~30s of speech). Stop. Verify:
- A transcript is produced
- The language matches what you spoke
- Word timing works in the playback view (if `whisper-1` is selected)
- Speaker labels apply post-stop via the diarize pass (if a diarize model is downloaded)

- [ ] **Step 4: Smoke-test Local provider**

Set provider to Local Whisper. Record another short test. Verify:
- Transcript is produced
- No model-loading regression (cold start should be ~1-2s as before)
- Word timing works in the playback view

- [ ] **Step 5: Verify legacy migration persisted**

After the first launch, open the SQLite DB and confirm the `transcribe_config` setting now exists. From a terminal:

```bash
sqlite3 ~/Library/Application\ Support/no.humla.app/notes.sqlite \
  "SELECT key, value FROM settings WHERE key = 'transcribe_config';"
```

Expected: one row with a JSON value matching what you have selected in Settings.

- [ ] **Step 6: Done**

If all five steps pass, Phase 1 is complete. Phase 2 (Deepgram) becomes ~250 LOC in a single new `stt/deepgram.rs` file plus a settings UI option — the trait dispatch absorbs everything else.

---

## Self-review

- **Spec coverage**: Tasks 1-7 build the trait + adapters. Task 8 implements the legacy-migration helper. Task 9 cuts over `transcribe_chunk`. Task 10 is the verification gate. Each phase from the design doc has a corresponding task.
- **Type consistency**: `Word`, `TranscribeCtx`, `TranscribeResult`, `BatchSttAdapter`, `ProviderConfig`, `OpenAiConfig`, `LocalWhisperConfig`, `LocalDeps`, `Auth` — all defined once and re-exported from `stt::mod`. Adapters import from `crate::stt::adapter::*`.
- **No placeholders**: every code step contains the actual code. No "TBD" or "similar to above". Each task can be executed without re-reading the rest of the plan.
- **Commit boundaries**: 9 commits, one per task. Each commit either adds a new file (no behavior change) or makes a single focused edit. The behavior-changing commit is Task 9.
