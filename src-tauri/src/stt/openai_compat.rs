//! Shared HTTP transcriber for OpenAI-compatible STT endpoints.
//! OpenAI itself, Groq, and self-hosted Whisper.cpp servers all expose
//! `POST /v1/audio/transcriptions` with the same multipart shape. The
//! per-provider adapter (OpenAiAdapter, GroqAdapter) configures base
//! URL + model + word-timestamp policy and calls into here.

use anyhow::{anyhow, Result};
use serde::Deserialize;
use std::path::Path;

use crate::stt::adapter::{TranscribeResult, Word};

#[derive(Deserialize)]
struct PlainResponse {
    text: String,
}

#[derive(Deserialize)]
struct VerboseResponse {
    text: String,
    /// English *name* of the detected language ("english", "norwegian"),
    /// not a code — see `verbose_to_result`. Absent on some compat servers.
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    words: Vec<VerboseWord>,
}

#[derive(Deserialize)]
struct VerboseWord {
    word: String,
    start: f64,
    end: f64,
}

/// One transcription against a `/v1/audio/transcriptions` endpoint.
/// `verbose` true requests `verbose_json` + `timestamp_granularities[]=word`
/// (only valid for OpenAI's `whisper-1`; gpt-4o-transcribe family rejects
/// it; Groq's whisper-large-v3-turbo accepts both shapes).
///
/// `bias_terms` and `prior_context` are merged into Whisper's
/// `initial_prompt` slot via `build_whisper_prompt`. Skipped entirely
/// when `skip_prompt_for_model` matches `model` (gpt-4o-transcribe-diarize
/// rejects the field).
pub async fn transcribe(
    base_url: &str,
    api_key: &str,
    model: &str,
    language: Option<&str>,
    bias_terms: &[&str],
    prior_context: Option<&str>,
    audio_path: &Path,
    verbose: bool,
    skip_prompt_for_model: Option<&str>,
) -> Result<TranscribeResult> {
    // A reported language is only a *detection* when we didn't name one —
    // otherwise the response just echoes what we forced, and storing an
    // echo as a detection would defeat the point (issue #167).
    let asked_to_detect = matches!(language, None | Some("auto"));
    let bytes = tokio::fs::read(audio_path).await?;
    let file_name = audio_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("chunk.wav")
        .to_string();
    let part = reqwest::multipart::Part::bytes(bytes)
        .file_name(file_name)
        .mime_str("audio/wav")?;

    let response_format = if verbose { "verbose_json" } else { "json" };
    let mut form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("model", model.to_string())
        .text("response_format", response_format.to_string())
        // Force deterministic decoding so Whisper-shaped models don't
        // hallucinate filler phrases / drift to a different language on
        // short audio.
        .text("temperature", "0".to_string());
    if verbose {
        form = form.text("timestamp_granularities[]", "word".to_string());
    }
    if let Some(l) = language {
        if l != "auto" {
            form = form.text("language", l.to_string());
        }
    }
    if skip_prompt_for_model != Some(model) {
        if let Some(prompt) = build_whisper_prompt(bias_terms, prior_context) {
            form = form.text("prompt", prompt);
        }
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()?;
    let r = client
        .post(format!("{base_url}/audio/transcriptions"))
        .bearer_auth(api_key)
        .multipart(form)
        .send()
        .await?;

    if !r.status().is_success() {
        let s = r.status();
        let body = r.text().await.unwrap_or_default();
        return Err(anyhow!("{base_url} {s}: {body}"));
    }

    if verbose {
        let body: VerboseResponse = r.json().await?;
        let mut out = verbose_to_result(body);
        if !asked_to_detect {
            out.detected_language = None;
        }
        Ok(out)
    } else {
        let body: PlainResponse = r.json().await?;
        Ok(TranscribeResult {
            text: body.text,
            ..Default::default()
        })
    }
}

/// Shape a `verbose_json` payload into the adapter result. Split out from
/// the request so the parsing is testable against a fixture.
///
/// `language` arrives as an English name and is mapped back to a code;
/// anything outside Whisper's table yields `None` rather than a guess.
fn verbose_to_result(body: VerboseResponse) -> TranscribeResult {
    let words = body
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
    let detected_language = body
        .language
        .as_deref()
        .and_then(crate::languages::code_for_english_name)
        .map(str::to_string);
    TranscribeResult {
        text: body.text,
        words,
        detected_language,
    }
}

/// Glue trailing transcript context + bias terms into Whisper's
/// `initial_prompt` slot. Prior context comes first, vocabulary last:
/// Whisper hard-caps the prompt at 224 tokens and keeps only the
/// *trailing* tokens when over budget, and trailing tokens exert greater
/// influence on decoding. Putting vocab at the end gives it both the
/// strongest bias and protection from silent truncation when the rolling
/// trail is dense (especially in non-English where words tokenize wider).
/// Returns None when neither is present so the API call omits the field.
pub fn build_whisper_prompt(
    bias_terms: &[&str],
    prior_context: Option<&str>,
) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(ctx) = prior_context {
        let trimmed = ctx.trim();
        if !trimmed.is_empty() {
            parts.push(trimmed.to_string());
        }
    }
    if !bias_terms.is_empty() {
        parts.push(bias_terms.join(", "));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(". "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_inputs_produce_no_prompt() {
        assert_eq!(build_whisper_prompt(&[], None), None);
        assert_eq!(build_whisper_prompt(&[], Some("")), None);
        assert_eq!(build_whisper_prompt(&[], Some("   ")), None);
    }

    #[test]
    fn vocab_only_returns_joined() {
        assert_eq!(
            build_whisper_prompt(&["Humla", "Tauri"], None),
            Some("Humla, Tauri".to_string())
        );
    }

    #[test]
    fn trail_only_returns_trimmed() {
        assert_eq!(
            build_whisper_prompt(&[], Some("  hello world  ")),
            Some("hello world".to_string())
        );
    }

    #[test]
    fn vocab_and_trail_join_with_period() {
        assert_eq!(
            build_whisper_prompt(&["Humla"], Some("hello world")),
            Some("hello world. Humla".to_string())
        );
    }

    // Issue #167 — verbose_json reports the detected language as an English
    // name, and that is the only place OpenAI/Groq tell us what was spoken.
    #[test]
    fn verbose_language_name_becomes_a_code() {
        let json = r#"{
          "text": "hello world",
          "language": "english",
          "words": [{"word": "hello", "start": 0.5, "end": 0.9}]
        }"#;
        let parsed: VerboseResponse = serde_json::from_str(json).unwrap();
        let out = verbose_to_result(parsed);
        assert_eq!(out.text, "hello world");
        assert_eq!(out.words.len(), 1);
        assert_eq!(out.detected_language.as_deref(), Some("en"));
    }

    #[test]
    fn verbose_without_a_language_field_detects_nothing() {
        let json = r#"{"text": "hei", "words": []}"#;
        let parsed: VerboseResponse = serde_json::from_str(json).unwrap();
        assert_eq!(verbose_to_result(parsed).detected_language, None);
    }

    // A name outside Whisper's table is an absent detection, not English.
    #[test]
    fn an_unmappable_language_name_detects_nothing() {
        let json = r#"{"text": "x", "language": "klingon", "words": []}"#;
        let parsed: VerboseResponse = serde_json::from_str(json).unwrap();
        assert_eq!(verbose_to_result(parsed).detected_language, None);
    }
}
