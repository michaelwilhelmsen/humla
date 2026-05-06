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
