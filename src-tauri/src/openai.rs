use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const BASE: &str = "https://api.openai.com/v1";

pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .expect("reqwest client")
}

// Local LLM servers (Ollama especially) cold-load the model on first request,
// which adds 5-15s before any tokens stream. A long-meeting summary can then
// generate for 30-60s on a 9B model. The 120s default isn't enough headroom
// for first-call-of-the-day on slower hardware. 5 minutes is a comfortable
// ceiling that still surfaces a stuck server as an error rather than hanging
// the UI forever.
fn local_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .expect("reqwest client")
}

pub async fn ping(api_key: &str) -> Result<bool> {
    let r = client()
        .get(format!("{BASE}/models"))
        .bearer_auth(api_key)
        .send()
        .await?;
    Ok(r.status().is_success())
}

#[derive(Deserialize)]
struct TranscribeResponse {
    text: String,
}

pub async fn transcribe_file(
    api_key: &str,
    model: &str,
    language: Option<&str>,
    prompt: Option<&str>,
    audio_path: &Path,
) -> Result<String> {
    let bytes = tokio::fs::read(audio_path).await?;
    let file_name = audio_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("chunk.wav")
        .to_string();
    let part = reqwest::multipart::Part::bytes(bytes)
        .file_name(file_name)
        .mime_str("audio/wav")?;

    let mut form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("model", model.to_string())
        .text("response_format", "json".to_string())
        // Force deterministic decoding so Whisper doesn't hallucinate filler
        // phrases and silently drift to a different language on short audio.
        .text("temperature", "0".to_string());
    if let Some(l) = language {
        if l != "auto" {
            form = form.text("language", l.to_string());
        }
    }
    // Per OpenAI docs, gpt-4o-transcribe-diarize does not accept prompt.
    if let Some(p) = prompt {
        if !p.is_empty() && model != "gpt-4o-transcribe-diarize" {
            form = form.text("prompt", p.to_string());
        }
    }

    let r = client()
        .post(format!("{BASE}/audio/transcriptions"))
        .bearer_auth(api_key)
        .multipart(form)
        .send()
        .await?;

    if !r.status().is_success() {
        let s = r.status();
        let body = r.text().await.unwrap_or_default();
        return Err(anyhow!("OpenAI {s}: {body}"));
    }
    let body: TranscribeResponse = r.json().await?;
    Ok(body.text)
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage<'a>>,
    // Reasoning models (gpt-5.x family + o-series) reject custom temperature
    // values with a 400 error; only the default (1) is allowed. Traditional
    // chat models (gpt-4o, gpt-4, gpt-3.5) accept it. `skip_serializing_if`
    // lets us send the right shape per model without a per-model payload.
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatMessageOwned,
}

#[derive(Deserialize)]
struct ChatMessageOwned {
    content: String,
}

/// Reasoning models: gpt-5.x family and the o-series. They reject the
/// `temperature` parameter and accept extra knobs like `reasoning_effort`
/// (which we leave at the API default).
fn is_reasoning_model(model: &str) -> bool {
    if let Some(rest) = model.strip_prefix("gpt-5") {
        // "gpt-5", "gpt-5-mini", "gpt-5-nano", "gpt-5.4", "gpt-5.4-mini",
        // "gpt-5.5", … all match. "gpt-50" (hypothetical future non-reasoning
        // brand) wouldn't match because the next char would be a digit.
        rest.is_empty() || rest.starts_with('.') || rest.starts_with('-')
    } else if let Some(rest) = model.strip_prefix('o') {
        // "o1", "o3", "o4-mini" — but not "openai-something" or other
        // o-prefixed names that aren't reasoning models.
        rest.chars().next().is_some_and(|c| c.is_ascii_digit())
    } else {
        false
    }
}

pub async fn summarize(
    api_key: &str,
    model: &str,
    system_prompt: &str,
    transcript: &str,
) -> Result<String> {
    summarize_with_base(BASE, api_key, model, system_prompt, transcript).await
}

/// Same shape as `summarize` but takes an explicit base URL. Used to route
/// summary calls at any OpenAI-compatible HTTP endpoint — most local-LLM
/// runtimes (Ollama, LM Studio, llama-server, vLLM) implement this exact
/// schema, so a one-line change in the caller flips between cloud OpenAI
/// and a local server.
///
/// `api_key` is forwarded as a bearer token regardless of base URL; local
/// servers typically ignore it but Ollama accepts any non-empty string.
pub async fn summarize_with_base(
    base_url: &str,
    api_key: &str,
    model: &str,
    system_prompt: &str,
    transcript: &str,
) -> Result<String> {
    let is_local = base_url != BASE;
    let req = ChatRequest {
        model,
        // Local OpenAI-compat servers accept temperature; reasoning-model
        // suppression only applies when the actual server is OpenAI's.
        temperature: if is_local || !is_reasoning_model(model) {
            Some(0.2)
        } else {
            None
        },
        messages: vec![
            ChatMessage { role: "system", content: system_prompt },
            ChatMessage { role: "user", content: transcript },
        ],
    };
    let http = if is_local { local_client() } else { client() };
    let r = http
        .post(format!("{base_url}/chat/completions"))
        .bearer_auth(api_key)
        .json(&req)
        .send()
        .await
        .map_err(|e| {
            // reqwest's timeout error looks like "request timed out" and
            // its connect error like "error sending request". Both are
            // opaque to the user; map to something actionable.
            if e.is_timeout() {
                anyhow!(
                    "Timed out after 5 minutes waiting for {base_url}. \
                     The local model may be cold-loading or stuck — \
                     try again, or restart your local-LLM server."
                )
            } else if e.is_connect() {
                anyhow!(
                    "Couldn't reach {base_url}. Is your local-LLM \
                     server running? (ollama serve, etc.)"
                )
            } else {
                anyhow!("network error talking to {base_url}: {e}")
            }
        })?;

    if !r.status().is_success() {
        let s = r.status();
        let body = r.text().await.unwrap_or_default();
        return Err(anyhow!("HTTP {s} from {base_url}: {body}"));
    }
    let body: ChatResponse = r.json().await?;
    let content = body.choices.into_iter().next()
        .map(|c| c.message.content)
        .unwrap_or_default();
    Ok(content)
}

/// Fetch the list of models a local OpenAI-compat server has loaded. Used by
/// the Settings UI to populate a model dropdown when the user picks Local
/// provider. Hits `<base_url>/models` and returns the `id` field for each
/// entry — the universal OpenAI/Ollama/LM Studio shape.
pub async fn list_models(base_url: &str) -> Result<Vec<String>> {
    #[derive(Deserialize)]
    struct ListResponse {
        data: Vec<ModelEntry>,
    }
    #[derive(Deserialize)]
    struct ModelEntry {
        id: String,
    }
    let r = client()
        .get(format!("{base_url}/models"))
        .send()
        .await?;
    if !r.status().is_success() {
        let s = r.status();
        return Err(anyhow!("HTTP {s} from {base_url}/models"));
    }
    let body: ListResponse = r.json().await?;
    Ok(body.data.into_iter().map(|m| m.id).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasoning_models_detected() {
        for m in [
            "gpt-5", "gpt-5.4", "gpt-5.4-mini", "gpt-5.4-nano", "gpt-5.5",
            "gpt-5-mini", "gpt-5-nano",
            "o1", "o3", "o4-mini",
        ] {
            assert!(is_reasoning_model(m), "expected reasoning: {m}");
        }
    }

    #[test]
    fn traditional_chat_models_not_reasoning() {
        for m in [
            "gpt-4o", "gpt-4o-mini", "gpt-4-turbo", "gpt-4",
            "gpt-3.5-turbo", "chatgpt-4o-latest",
            "openai-internal", // "o" prefix but not followed by a digit
        ] {
            assert!(!is_reasoning_model(m), "expected NOT reasoning: {m}");
        }
    }
}
