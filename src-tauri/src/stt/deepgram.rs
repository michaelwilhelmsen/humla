//! Deepgram batch STT adapter. Different from OpenAI-compat in three
//! ways: auth uses `Token` not `Bearer`, response is nested under
//! `results.channels[0].alternatives[0]`, and Deepgram's `keywords`
//! query param is a per-token probability boost (not a continuation
//! primer like Whisper's `prompt`). We feed it ONLY the user's
//! vocabulary; transcript trail is intentionally dropped because
//! biasing per-token decoding toward last chunk's text would actively
//! hurt the next chunk's accuracy.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::Deserialize;
use std::path::Path;

use crate::stt::adapter::{BatchSttAdapter, TranscribeCtx, TranscribeResult, Word};

const DEEPGRAM_BASE: &str = "https://api.deepgram.com/v1/listen";

#[derive(Deserialize)]
struct ListenResponse {
    results: ResultsBlock,
}

#[derive(Deserialize)]
struct ResultsBlock {
    channels: Vec<Channel>,
}

#[derive(Deserialize)]
struct Channel {
    alternatives: Vec<Alternative>,
}

#[derive(Deserialize)]
struct Alternative {
    transcript: String,
    #[serde(default)]
    words: Vec<DGWord>,
}

#[derive(Deserialize)]
struct DGWord {
    word: String,
    start: f64,
    end: f64,
}

#[derive(Default)]
pub struct DeepgramAdapter;

impl DeepgramAdapter {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl BatchSttAdapter for DeepgramAdapter {
    fn provider_id(&self) -> &'static str {
        "deepgram"
    }

    fn label(&self) -> &'static str {
        "Deepgram"
    }

    fn supports_language(&self, _lang: &str) -> bool {
        true
    }

    fn supports_word_timestamps(&self, _model: &str) -> bool {
        true
    }

    async fn transcribe(
        &self,
        ctx: TranscribeCtx<'_>,
        audio: &Path,
    ) -> Result<TranscribeResult> {
        let api_key = ctx
            .api_key
            .ok_or_else(|| anyhow!("Deepgram adapter requires api_key"))?;
        let base_url = ctx.base_url.unwrap_or(DEEPGRAM_BASE);
        let bytes = tokio::fs::read(audio).await?;

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()?;
        let mut req = client
            .post(base_url)
            .header("Authorization", format!("Token {api_key}"))
            .header("Content-Type", "audio/wav")
            .query(&[
                ("model", ctx.model),
                ("smart_format", "true"),
                ("punctuate", "true"),
            ]);
        if ctx.language != "auto" {
            req = req.query(&[("language", ctx.language)]);
        }
        // Deepgram's vocabulary biasing is a per-token probability boost,
        // NOT a continuation primer. Feeding it transcript trail text
        // would bias decoding toward whatever was said before, which is
        // exactly the wrong signal for the next chunk. We deliberately
        // ignore ctx.prior_context here.
        //
        // Two parameter shapes by model:
        //   nova-3       → `keyterm=Term` (no intensifier; Nova-3
        //                  rejects `keywords` outright with 400
        //                  INVALID_QUERY_PARAMETER).
        //   other models → `keywords=Term:1.5` (intensifier 1.5 — higher
        //                  causes phonetic over-recognition, lower is
        //                  imperceptible).
        //
        // Deepgram caps at 100 entries either way.
        let use_keyterm = ctx.model.starts_with("nova-3");
        let param_name = if use_keyterm { "keyterm" } else { "keywords" };
        let mut keyword_count = 0usize;
        for term in ctx.bias_terms.iter() {
            if keyword_count >= 100 {
                break;
            }
            let cleaned = term.trim_matches(|c: char| !c.is_alphanumeric());
            if cleaned.len() >= 3 {
                let value = if use_keyterm {
                    cleaned.to_string()
                } else {
                    format!("{cleaned}:1.5")
                };
                req = req.query(&[(param_name, &value)]);
                keyword_count += 1;
            }
        }

        let r = req.body(bytes).send().await?;
        if !r.status().is_success() {
            let s = r.status();
            let body = r.text().await.unwrap_or_default();
            return Err(anyhow!("Deepgram {s}: {body}"));
        }
        let body: ListenResponse = r.json().await?;
        let alt = body
            .results
            .channels
            .into_iter()
            .next()
            .and_then(|c| c.alternatives.into_iter().next())
            .ok_or_else(|| anyhow!("Deepgram returned no alternatives"))?;

        let words = alt
            .words
            .into_iter()
            .filter_map(|w| {
                let text = w.word.trim().to_string();
                if text.is_empty() {
                    return None;
                }
                let start_ms = (w.start.max(0.0) * 1000.0).round() as u64;
                let end_ms = (w.end.max(0.0) * 1000.0).round() as u64;
                Some(Word {
                    text,
                    start_ms,
                    end_ms: end_ms.max(start_ms),
                })
            })
            .collect();

        Ok(TranscribeResult {
            text: alt.transcript,
            words,
            // Deepgram only reports a detected language when asked with
            // `detect_language=true`, which is a request-shape change and a
            // nested response field. Left as a follow-up (issue #167); on
            // `auto` these notes fall back to the transcript-anchored
            // directive rather than a resolved code.
            detected_language: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_is_correct() {
        let a = DeepgramAdapter::new();
        assert_eq!(a.provider_id(), "deepgram");
        assert_eq!(a.label(), "Deepgram");
        assert!(a.supports_word_timestamps("nova-3"));
    }

    #[test]
    fn parses_canonical_listen_response() {
        let json = r#"{
          "results": {
            "channels": [{
              "alternatives": [{
                "transcript": "hello world",
                "words": [
                  {"word": "hello", "start": 0.5, "end": 0.9},
                  {"word": "world", "start": 1.0, "end": 1.4}
                ]
              }]
            }]
          }
        }"#;
        let parsed: ListenResponse = serde_json::from_str(json).unwrap();
        let alt = &parsed.results.channels[0].alternatives[0];
        assert_eq!(alt.transcript, "hello world");
        assert_eq!(alt.words.len(), 2);
        assert_eq!(alt.words[0].word, "hello");
        assert!((alt.words[0].start - 0.5).abs() < 1e-6);
    }

    #[test]
    fn handles_response_with_no_words() {
        let json = r#"{
          "results": {
            "channels": [{
              "alternatives": [{
                "transcript": "silence here"
              }]
            }]
          }
        }"#;
        let parsed: ListenResponse = serde_json::from_str(json).unwrap();
        let alt = &parsed.results.channels[0].alternatives[0];
        assert_eq!(alt.transcript, "silence here");
        assert!(alt.words.is_empty());
    }
}
