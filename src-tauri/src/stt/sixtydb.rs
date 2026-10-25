//! 60db batch transcription. Speaker labels remain owned by Humla's
//! offline diarizer; only transcript text and word timings enter this path.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use reqwest::multipart::{Form, Part};
use serde::Deserialize;
use std::{path::Path, time::Duration};
use tokio::io::AsyncReadExt;

use super::{BatchSttAdapter, TranscribeCtx, TranscribeResult, Word};

const BASE: &str = "https://api.60db.ai";
const MAX_AUDIO: u64 = 10_000_000;
const MAX_RESPONSE: usize = 4 * 1024 * 1024;
pub const MODEL: &str = "60db-stt-v01";

#[derive(Default)]
pub struct SixtyDbAdapter;

impl SixtyDbAdapter {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl BatchSttAdapter for SixtyDbAdapter {
    fn provider_id(&self) -> &'static str {
        "sixtydb"
    }
    fn label(&self) -> &'static str {
        "60db"
    }
    fn supports_language(&self, language: &str) -> bool {
        // Reject codes the API explicitly excludes. The server validates the
        // remaining ISO codes against its current language catalog.
        language == "auto"
            || (language.len() == 2
                && language.bytes().all(|c| c.is_ascii_lowercase())
                && ![
                    "ur", "ja", "ko", "zh", "th", "vi", "id", "tl", "sw", "tr", "fa", "he",
                ]
                .contains(&language))
    }
    fn supports_word_timestamps(&self, model: &str) -> bool {
        model == MODEL
    }

    async fn transcribe(&self, ctx: TranscribeCtx<'_>, audio: &Path) -> Result<TranscribeResult> {
        let key = ctx
            .api_key
            .filter(|k| !k.trim().is_empty())
            .context("60db requires an API key saved in Settings")?;
        if ctx.model != MODEL {
            bail!("Unsupported 60db transcription model");
        }
        if !self.supports_language(ctx.language) {
            bail!(
                "60db does not support the selected language: {}",
                ctx.language
            );
        }
        let file = tokio::fs::File::open(audio)
            .await
            .context("Read 60db audio file")?;
        let mut bytes = Vec::new();
        file.take(MAX_AUDIO + 1)
            .read_to_end(&mut bytes)
            .await
            .context("Read 60db audio file")?;
        if bytes.is_empty() || bytes.len() as u64 > MAX_AUDIO {
            bail!("60db audio must be non-empty and no larger than 10 MB");
        }
        let mut form = Form::new()
            .part(
                "file",
                Part::bytes(bytes)
                    .file_name("chunk.wav")
                    .mime_str("audio/wav")?,
            )
            .text("return_timestamps", "word")
            .text("diarize", "false");
        if ctx.language != "auto" {
            form = form.text("language", ctx.language.to_owned());
        }
        if !ctx.bias_terms.is_empty() {
            form = form.text("keywords", ctx.bias_terms.join(","));
        }
        // prior_context is continuation text, not a list of vocabulary boosts.
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(120))
            .build()?;
        let mut response = client
            .post(format!(
                "{}/stt",
                ctx.base_url.unwrap_or(BASE).trim_end_matches('/')
            ))
            .bearer_auth(key)
            .multipart(form)
            .send()
            .await
            .map_err(|_| {
                anyhow::anyhow!("60db transcription request failed; check the connection")
            })?;
        if !response.status().is_success() {
            // Do not expose arbitrary response bodies or credentials in errors.
            bail!(
                "60db transcription failed (HTTP {})",
                response.status().as_u16()
            );
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.context("Read 60db response")? {
            if body.len() + chunk.len() > MAX_RESPONSE {
                bail!("60db response exceeds the size limit");
            }
            body.extend_from_slice(&chunk);
        }
        parse_response(&body, ctx.language == "auto")
    }
}

#[derive(Deserialize)]
struct Response {
    text: String,
    language: Option<String>,
    #[serde(default)]
    words: Vec<ApiWord>,
    #[serde(default)]
    segments: Vec<Segment>,
}
#[derive(Deserialize)]
struct Segment {
    #[serde(default)]
    words: Vec<ApiWord>,
}
#[derive(Deserialize)]
struct ApiWord {
    word: String,
    start: f64,
    end: f64,
}

fn parse_response(bytes: &[u8], auto: bool) -> Result<TranscribeResult> {
    let response: Response = serde_json::from_slice(bytes)
        .map_err(|_| anyhow::anyhow!("Invalid 60db transcription response"))?;
    let source = if response.words.is_empty() {
        response
            .segments
            .into_iter()
            .flat_map(|s| s.words)
            .collect()
    } else {
        response.words
    };
    let mut words = Vec::new();
    for w in source {
        if !w.start.is_finite()
            || !w.end.is_finite()
            || w.start < 0.0
            || w.end < w.start
            || w.end > 3600.0
        {
            bail!("Invalid 60db word timestamps");
        }
        words.push(Word {
            text: w.word,
            start_ms: (w.start * 1000.0).round() as u64,
            end_ms: (w.end * 1000.0).round() as u64,
        });
    }
    Ok(TranscribeResult {
        text: response.text,
        words,
        detected_language: if auto { response.language } else { None },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_preserves_text_language_and_real_word_times() {
        let r = parse_response(br#"{"text":"Hello!","language":"en","words":[{"word":"Hello!","start":0.125,"end":0.5}]}"#, true).unwrap();
        assert_eq!(r.text, "Hello!");
        assert_eq!(r.detected_language.as_deref(), Some("en"));
        assert_eq!(r.words[0].start_ms, 125);
        assert_eq!(r.words[0].end_ms, 500);
        let r = parse_response(br#"{"text":"Hello","language":"en","segments":[{"words":[{"word":"Hello","start":0,"end":1}]}]}"#, false).unwrap();
        assert_eq!(r.words.len(), 1);
        assert!(r.detected_language.is_none());
        let r = parse_response(
            br#"{"text":"","language":null,"words":[],"warning_codes":["no_speech_detected"]}"#,
            true,
        )
        .unwrap();
        assert!(r.text.is_empty() && r.words.is_empty());
        assert!(parse_response(
            br#"{"text":"bad","words":[{"word":"bad","start":2,"end":1}]}"#,
            true
        )
        .is_err());
        assert!(parse_response(br#"{"error":"not a transcript"}"#, true).is_err());
        assert!(!SixtyDbAdapter::new().supports_language("ja"));
        assert!(!SixtyDbAdapter::new().supports_language("ar-eg"));
    }
    async fn serve(status: u16, body: &'static str) -> (String, tokio::task::JoinHandle<String>) {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let header_end;
            loop {
                let mut chunk = [0; 4096];
                let n = stream.read(&mut chunk).await.unwrap();
                assert!(n > 0);
                request.extend_from_slice(&chunk[..n]);
                if let Some(i) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    header_end = i + 4;
                    break;
                }
            }
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    if name.eq_ignore_ascii_case("content-length") {
                        value.trim().parse().ok()
                    } else {
                        None
                    }
                })
                .unwrap();
            while request.len() < header_end + length {
                let mut chunk = [0; 4096];
                let n = stream.read(&mut chunk).await.unwrap();
                assert!(n > 0);
                request.extend_from_slice(&chunk[..n]);
            }
            stream.write_all(format!("HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            String::from_utf8(request).unwrap()
        });
        (url, task)
    }

    #[tokio::test]
    async fn multipart_request_preserves_audio_and_vocabulary_without_trail_context() {
        tokio::time::timeout(Duration::from_secs(10), async {
            let (base, task) = serve(200, r#"{"text":"Hello","language":"en","words":[{"word":"Hello","start":0.1,"end":0.8}]}"#).await;
            let audio = tempfile::NamedTempFile::new().unwrap();
            std::fs::write(audio.path(), b"RIFF-test-audio").unwrap();
            let result = SixtyDbAdapter::new().transcribe(TranscribeCtx {
                model: MODEL, language: "en", bias_terms: &["Humla", "Tauri"],
                prior_context: Some("private trailing transcript"), api_key: Some("fixture-key"), base_url: Some(&base),
            }, audio.path()).await.unwrap();
            assert_eq!(result.words[0].start_ms, 100);
            let request = task.await.unwrap();
            assert!(request.starts_with("POST /stt HTTP/1.1"));
            assert!(request.to_lowercase().contains("authorization: bearer fixture-key"));
            for field in ["file", "language", "keywords", "return_timestamps", "diarize"] {
                assert!(request.contains(&format!("name=\"{field}\"")));
            }
            assert!(request.contains("RIFF-test-audio"));
            assert!(request.contains("Humla,Tauri"));
            assert!(request.contains("\r\n\r\nword\r\n"));
            assert!(!request.contains("private trailing transcript"));
            assert!(!request.contains("name=\"model\""));
            assert!(!request.contains("name=\"context\""));
        }).await.unwrap();
    }

    #[tokio::test]
    async fn auto_detection_omits_language_and_http_errors_do_not_echo_body() {
        tokio::time::timeout(Duration::from_secs(10), async {
            let audio = tempfile::NamedTempFile::new().unwrap();
            std::fs::write(audio.path(), b"RIFF-test-audio").unwrap();
            for status in [401, 402, 429, 503] {
                let (base, task) = serve(status, r#"{"error":"private-fixture-secret"}"#).await;
                let error = SixtyDbAdapter::new()
                    .transcribe(
                        TranscribeCtx {
                            model: MODEL,
                            language: "auto",
                            bias_terms: &[],
                            prior_context: None,
                            api_key: Some("fixture-key"),
                            base_url: Some(&base),
                        },
                        audio.path(),
                    )
                    .await
                    .unwrap_err()
                    .to_string();
                assert!(error.contains(&status.to_string()));
                assert!(!error.contains("private-fixture-secret"));
                assert!(!error.contains("fixture-key"));
                let request = task.await.unwrap();
                assert!(!request.contains("name=\"language\""));
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn invalid_inputs_fail_before_network_upload() {
        let audio = tempfile::NamedTempFile::new().unwrap();
        for (size, language, key, expected) in [
            (0, "en", Some("fixture-key"), "non-empty"),
            (MAX_AUDIO + 1, "en", Some("fixture-key"), "10 MB"),
            (1, "ja", Some("fixture-key"), "selected language"),
            (1, "en", None, "API key"),
        ] {
            audio.as_file().set_len(size).unwrap();
            let error = SixtyDbAdapter::new()
                .transcribe(
                    TranscribeCtx {
                        model: MODEL,
                        language,
                        bias_terms: &[],
                        prior_context: None,
                        api_key: key,
                        base_url: Some("http://127.0.0.1:1"),
                    },
                    audio.path(),
                )
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains(expected), "{error}");
        }
    }
}
