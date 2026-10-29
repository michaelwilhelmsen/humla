//! The user's own OpenAI-compatible endpoint for transcription (#173), through
//! the shared multipart client. Word timestamps are asked for; on a 400 the
//! chunk is asked again in plain `json`, and when that works the model is asked
//! that way for the rest of the launch, so a long meeting doesn't pay twice per
//! chunk. Its chunks then carry text without per-word timings.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use std::collections::HashSet;
use std::path::Path;
use std::sync::LazyLock;

use crate::stt::adapter::{BatchSttAdapter, TranscribeCtx, TranscribeResult};
use crate::stt::openai_compat::{self, HttpError};

/// `(base URL, model)` pairs that refused word timestamps this launch.
static PLAIN_JSON_ONLY: LazyLock<parking_lot::Mutex<HashSet<(String, String)>>> =
    LazyLock::new(Default::default);

#[derive(Default)]
pub struct CustomAdapter;

impl CustomAdapter {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl BatchSttAdapter for CustomAdapter {
    fn provider_id(&self) -> &'static str {
        "custom"
    }

    fn label(&self) -> &'static str {
        "Custom"
    }

    fn supports_language(&self, _lang: &str) -> bool {
        true
    }

    fn supports_word_timestamps(&self, _model: &str) -> bool {
        true
    }

    async fn transcribe(&self, ctx: TranscribeCtx<'_>, audio: &Path) -> Result<TranscribeResult> {
        let base_url = ctx.base_url.ok_or_else(|| anyhow!(crate::custom_endpoint::NO_BASE_URL))?;
        let endpoint_model = (base_url.to_string(), ctx.model.to_string());
        let call = |verbose| {
            openai_compat::transcribe(
                base_url,
                ctx.api_key,
                ctx.model,
                Some(ctx.language),
                ctx.bias_terms,
                ctx.prior_context,
                audio,
                verbose,
                None,
            )
        };
        let verbose = !PLAIN_JSON_ONLY.lock().contains(&endpoint_model);
        let result = match call(verbose).await {
            Err(e) if verbose && status(&e) == Some(400) => {
                eprintln!("[stt] {} refused verbose_json; asking in plain json", ctx.model);
                let plain = call(false).await;
                if plain.is_ok() {
                    PLAIN_JSON_ONLY.lock().insert(endpoint_model);
                }
                plain
            }
            r => r,
        };
        result.map_err(|e| match e.downcast::<HttpError>() {
            Ok(h) => crate::custom_endpoint::status_error(h.status, &h.url, &h.body, ctx.api_key.is_some()),
            Err(e) => e,
        })
    }
}

fn status(e: &anyhow::Error) -> Option<u16> {
    e.downcast_ref::<HttpError>().map(|h| h.status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::test_server::serve;

    fn chunk() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mic-chunk-0001.wav");
        std::fs::write(&path, b"RIFF\x24\x00\x00\x00WAVEfmt ").unwrap();
        (dir, path)
    }

    fn ctx<'a>(base: &'a str, model: &'a str, key: Option<&'a str>) -> TranscribeCtx<'a> {
        TranscribeCtx {
            model,
            language: "en",
            bias_terms: &[],
            prior_context: None,
            api_key: key,
            base_url: Some(base),
        }
    }

    /// The value of one multipart field, if the request has it.
    fn field(raw: &str, name: &str) -> Option<String> {
        let marker = format!("name=\"{name}\"\r\n\r\n");
        let start = raw.find(&marker)? + marker.len();
        let len = raw[start..].find("\r\n--")?;
        Some(raw[start..start + len].to_string())
    }

    #[tokio::test]
    async fn asks_for_word_timestamps_at_the_typed_base_url() {
        let verbose = r#"{"text":"hello world","language":"english","words":[
            {"word":"hello","start":0.0,"end":0.4},{"word":"world","start":0.5,"end":0.9}]}"#;
        let (port, server) = serve(vec![(200, "application/json", verbose.into())]).await;
        let base = format!("http://127.0.0.1:{port}/api/v1");
        let (_dir, wav) = chunk();
        let out = CustomAdapter::new()
            .transcribe(ctx(&base, "elevenlabs/scribe-v2", Some("sk-or-test")), &wav)
            .await
            .unwrap();
        assert_eq!(out.text, "hello world");
        assert_eq!(out.words.len(), 2);
        let raw = &server.await.unwrap()[0];
        assert!(raw.starts_with("POST /api/v1/audio/transcriptions "), "{raw}");
        assert!(raw.to_lowercase().contains("authorization: bearer sk-or-test"));
        assert_eq!(field(raw, "model").as_deref(), Some("elevenlabs/scribe-v2"));
        assert_eq!(field(raw, "response_format").as_deref(), Some("verbose_json"));
        assert_eq!(field(raw, "timestamp_granularities[]").as_deref(), Some("word"));
        assert_eq!(field(raw, "language").as_deref(), Some("en"));
    }

    #[tokio::test]
    async fn a_model_refusing_word_timestamps_is_asked_again_in_plain_json_and_remembered() {
        let (port, server) = serve(vec![
            (400, "application/json", r#"{"error":"timestamp_granularities not supported"}"#.into()),
            (200, "application/json", r#"{"text":"hei"}"#.into()),
            (200, "application/json", r#"{"text":"igjen"}"#.into()),
        ])
        .await;
        let base = format!("http://127.0.0.1:{port}/v1");
        let model = "openai/gpt-4o-transcribe-refusal-test";
        let (_dir, wav) = chunk();
        let adapter = CustomAdapter::new();
        let first = adapter.transcribe(ctx(&base, model, None), &wav).await.unwrap();
        assert_eq!(first.text, "hei");
        assert!(first.words.is_empty());
        let second = adapter.transcribe(ctx(&base, model, None), &wav).await.unwrap();
        assert_eq!(second.text, "igjen");

        let seen = server.await.unwrap();
        let formats: Vec<_> = seen.iter().map(|r| field(r, "response_format").unwrap()).collect();
        assert_eq!(formats, ["verbose_json", "json", "json"]);
        assert!(field(&seen[1], "timestamp_granularities[]").is_none());
        assert!(!seen[0].to_lowercase().contains("authorization"));
    }

    // A 400 can mean anything; only a plain retry that works shows it was the
    // word timings, so only that is remembered.
    #[tokio::test]
    async fn a_400_the_plain_retry_does_not_fix_is_not_remembered() {
        let (port, server) = serve(vec![
            (400, "application/json", r#"{"error":"bad language"}"#.into()),
            (400, "application/json", r#"{"error":"bad language"}"#.into()),
            (200, "application/json", r#"{"text":"hei","words":[]}"#.into()),
        ])
        .await;
        let base = format!("http://127.0.0.1:{port}/v1");
        let model = "elevenlabs/scribe-v2-unrelated-400-test";
        let (_dir, wav) = chunk();
        let adapter = CustomAdapter::new();
        let err = adapter.transcribe(ctx(&base, model, None), &wav).await.unwrap_err().to_string();
        assert!(err.contains("bad language"), "{err}");
        adapter.transcribe(ctx(&base, model, None), &wav).await.unwrap();

        let seen = server.await.unwrap();
        let formats: Vec<_> = seen.iter().map(|r| field(r, "response_format").unwrap()).collect();
        assert_eq!(formats, ["verbose_json", "json", "verbose_json"]);
    }

    #[tokio::test]
    async fn any_other_failure_names_the_url_it_asked() {
        let (port, _server) =
            serve(vec![(401, "application/json", r#"{"error":"bad key"}"#.into())]).await;
        let base = format!("http://127.0.0.1:{port}/v1");
        let (_dir, wav) = chunk();
        let err = CustomAdapter::new()
            .transcribe(ctx(&base, "whisper-large-v3", Some("k")), &wav)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains(&format!("{base}/audio/transcriptions")), "{err}");
        assert!(err.contains("API key"), "{err}");
    }

    #[tokio::test]
    async fn a_keyless_401_asks_for_a_key() {
        let body = r#"{"error":{"message":"No cookie auth credentials found","code":401}}"#;
        let (port, _server) = serve(vec![(401, "application/json", body.into())]).await;
        let base = format!("http://127.0.0.1:{port}/v1");
        let (_dir, wav) = chunk();
        let err = CustomAdapter::new()
            .transcribe(ctx(&base, "elevenlabs/scribe-v2", None), &wav)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("no API key is saved"), "{err}");
    }

    #[tokio::test]
    async fn no_base_url_is_an_error_not_a_guess() {
        let (_dir, wav) = chunk();
        let mut c = ctx("", "m", None);
        c.base_url = None;
        let err = CustomAdapter::new().transcribe(c, &wav).await.unwrap_err().to_string();
        assert!(err.contains("base URL"), "{err}");
    }
}
