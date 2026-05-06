//! STT provider abstraction. See docs/design/stt-adapter.md for rationale.

mod adapter;
mod auth;

pub use adapter::{BatchSttAdapter, TranscribeCtx, TranscribeResult, Word};
pub use auth::Auth;
