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

// Local LLM servers (Ollama especially) cold-load the model on first request
// (~10s on a 9B), then generate at ~30 tok/s on Apple Silicon. A long-meeting
// summary can run 60s; long-meeting *polish* (which regenerates the whole
// transcript) can run several minutes. 10 minutes is generous enough that
// genuine slow paths complete, while still surfacing a wedged server as an
// error rather than hanging the UI indefinitely.
fn local_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
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
    // Qwen 3+ via Ollama puts internal reasoning here (extension to the
    // OpenAI schema). If `content` is empty but this is set, the model
    // ran out of tokens or context inside the thinking phase and never
    // produced an answer — surface that as a clear error.
    #[serde(default)]
    reasoning_content: Option<String>,
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
    summarize_with_base(BASE, api_key, model, false, system_prompt, transcript, |_| {}).await
}

/// Same shape as `summarize` but takes an explicit base URL. Used to route
/// summary calls at any OpenAI-compatible HTTP endpoint — most local-LLM
/// runtimes (Ollama, LM Studio, llama-server, vLLM) implement this exact
/// schema, so a one-line change in the caller flips between cloud OpenAI
/// and a local server.
///
/// `api_key` is forwarded as a bearer token regardless of base URL; local
/// servers typically ignore it but Ollama accepts any non-empty string.
pub async fn summarize_with_base<F>(
    base_url: &str,
    api_key: &str,
    model: &str,
    think: bool,
    system_prompt: &str,
    transcript: &str,
    on_chunk: F,
) -> Result<String>
where
    F: FnMut(StreamChunk) + Send,
{
    let is_local = base_url != BASE;
    // For Ollama, route through the native /api/chat endpoint so we can
    // pass an explicit `think` flag and reliably control Qwen 3+'s
    // thinking mode. The OpenAI-compat endpoint renders the chat template
    // internally and strips user-message /no_think directives. The native
    // path also streams, so the callback fires per-frame while the model
    // works.
    if is_local {
        if let Some(native_base) = ollama_native_url(base_url) {
            return ollama_native_chat(
                &native_base, model, think, system_prompt, transcript, on_chunk,
            )
            .await;
        }
    }
    // Cloud OpenAI-compat path is non-streaming; on_chunk is unused.
    let _ = on_chunk;
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
    let url = format!("{base_url}/chat/completions");
    let started = std::time::Instant::now();
    eprintln!(
        "[llm] POST {url} model={model} system_chars={} user_chars={}",
        system_prompt.len(),
        transcript.len()
    );
    let r = http
        .post(&url)
        .bearer_auth(api_key)
        .json(&req)
        .send()
        .await
        .map_err(|e| {
            eprintln!(
                "[llm] send error after {:?}: timeout={} connect={} body={}",
                started.elapsed(),
                e.is_timeout(),
                e.is_connect(),
                e
            );
            // reqwest's timeout error looks like "request timed out" and
            // its connect error like "error sending request". Both are
            // opaque to the user; map to something actionable.
            if e.is_timeout() {
                anyhow!(
                    "Timed out after 10 minutes waiting for {base_url}. \
                     The local model may be stuck — restart your \
                     local-LLM server (e.g. `pkill ollama && ollama serve`)."
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

    let status = r.status();
    eprintln!("[llm] response {status} after {:?}", started.elapsed());
    if !status.is_success() {
        let body = r.text().await.unwrap_or_default();
        eprintln!("[llm] error body: {body}");
        return Err(anyhow!("HTTP {status} from {base_url}: {body}"));
    }
    // Read the body once so we can log it on parse failure (Ollama's error
    // shape on quirky responses isn't always OpenAI-compat).
    let body_text = r.text().await?;
    let body: ChatResponse = match serde_json::from_str(&body_text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "[llm] could not parse response as ChatResponse: {e}\n\
                 [llm] body (first 500 chars): {}",
                &body_text.chars().take(500).collect::<String>()
            );
            return Err(anyhow!("unexpected response shape from {base_url}: {e}"));
        }
    };
    let first = body.choices.into_iter().next();
    let reasoning_chars = first
        .as_ref()
        .and_then(|c| c.message.reasoning_content.as_deref())
        .map(str::len)
        .unwrap_or(0);
    let content = first.map(|c| c.message.content).unwrap_or_default();
    eprintln!(
        "[llm] success in {:?}, content {} chars, reasoning {} chars",
        started.elapsed(),
        content.len(),
        reasoning_chars
    );
    if content.trim().is_empty() {
        // The model returned only reasoning, or nothing at all. Either way
        // we have no usable answer — surface a clear error rather than
        // saving an empty summary.
        if reasoning_chars > 0 {
            return Err(anyhow!(
                "{model} produced reasoning but no final answer ({} reasoning chars). \
                 Try a non-thinking model (e.g. qwen3.5:4b) or shorten the input.",
                reasoning_chars
            ));
        }
        return Err(anyhow!("{model} returned an empty response"));
    }
    Ok(content)
}

/// Try to derive the Ollama native API base URL from an OpenAI-compat URL.
/// Ollama exposes its own API at `/api/...` and an OpenAI-compat shim at
/// `/v1/...`; the convention is the same host:port. Returns None for non-
/// Ollama-shaped URLs (LM Studio at :1234, llama-server, vLLM) — those keep
/// the OpenAI-compat path.
fn ollama_native_url(openai_compat_url: &str) -> Option<String> {
    // Heuristic: Ollama's default port is 11434. If the URL doesn't mention
    // it, assume the user is on a different runtime (LM Studio :1234, etc.)
    // and stay on OpenAI-compat. Users can override by pointing
    // local_llm_base_url at any host:11434.
    if !openai_compat_url.contains(":11434") {
        return None;
    }
    let trimmed = openai_compat_url.trim_end_matches('/');
    let stripped = trimmed.strip_suffix("/v1")?;
    Some(format!("{stripped}/api"))
}

#[derive(Serialize)]
struct OllamaChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage<'a>>,
    stream: bool,
    // Ollama 0.6+: bool toggles reasoning for most models (Qwen 3+,
    // DeepSeek-R1, etc). GPT-OSS is the exception — it expects a string
    // ("low" | "medium" | "high") and silently ignores booleans. If we
    // ever default to a GPT-OSS local model we'll need to make this
    // serde-untagged with both shapes; for Qwen 3.5 / DeepSeek users a
    // bool is correct.
    think: bool,
    options: OllamaOptions,
}

#[derive(Serialize)]
struct OllamaOptions {
    temperature: f32,
    // Hard cap on generated tokens. Without this, Qwen 3+ thinking mode can
    // burn 5K+ tokens reasoning before answering even on tiny inputs.
    num_predict: i32,
    // Penalize tokens that appeared in the last N positions, mitigating the
    // "Okay, let's write. Wait, I need to check the language. Norwegian.
    //  Okay, let's write. Wait, I need to check the format. Markdown..."
    // degenerate loop Qwen 3.5:9B falls into during thinking. Default in
    // Ollama is 1.1 (mild); 1.3 is meaningfully stronger without distorting
    // normal output. last_n=256 covers a few sentences of context.
    repeat_penalty: f32,
    repeat_last_n: i32,
}

// One JSON object per newline-delimited frame Ollama emits when stream:true.
// Each frame's `message.thinking` and `message.content` carry the delta since
// the previous frame; we accumulate content for the return value and forward
// thinking deltas to the caller's callback for live UI rendering.
#[derive(Deserialize)]
struct OllamaStreamChunk {
    message: OllamaStreamMessage,
    #[serde(default)]
    done: bool,
}

#[derive(Deserialize, Default)]
struct OllamaStreamMessage {
    #[serde(default)]
    content: String,
    #[serde(default)]
    thinking: Option<String>,
}

/// Tagged delta for streaming summary callbacks. `Thinking` is the model's
/// reasoning trace (only emitted when think:true); `Content` is the actual
/// answer being assembled. Caller decides what to do with each kind — most
/// commonly emit Tauri events for live UI rendering.
#[derive(Clone, Copy, Debug)]
pub enum StreamChunk<'a> {
    Thinking(&'a str),
    Content(&'a str),
}

async fn ollama_native_chat<F>(
    native_base: &str,
    model: &str,
    think: bool,
    system_prompt: &str,
    user_message: &str,
    mut on_chunk: F,
) -> Result<String>
where
    F: FnMut(StreamChunk) + Send,
{
    let url = format!("{native_base}/chat");
    let req = OllamaChatRequest {
        model,
        messages: vec![
            ChatMessage { role: "system", content: system_prompt },
            ChatMessage { role: "user", content: user_message },
        ],
        stream: true,
        think,
        options: OllamaOptions {
            temperature: 0.2,
            // Thinking burns thousands of reasoning tokens before the final
            // answer; 4096 is enough for the fast path, 8192 gives thinking
            // headroom while still failing fast on degenerate loops (was
            // 16384, but a stuck Qwen takes ~9 minutes to hit that — too
            // long to wait for the timeout to free up Ollama).
            num_predict: if think { 8192 } else { 4096 },
            repeat_penalty: 1.3,
            repeat_last_n: 256,
        },
    };
    let started = std::time::Instant::now();
    eprintln!(
        "[llm] POST {url} (ollama-native, streaming) model={model} think={think} system_chars={} user_chars={}",
        system_prompt.len(),
        user_message.len()
    );
    let r = local_client()
        .post(&url)
        .json(&req)
        .send()
        .await
        .map_err(|e| {
            eprintln!(
                "[llm] ollama send error after {:?}: timeout={} connect={} body={}",
                started.elapsed(), e.is_timeout(), e.is_connect(), e
            );
            if e.is_timeout() {
                anyhow!("Timed out after 10 minutes waiting for {url}. Restart Ollama and try again.")
            } else if e.is_connect() {
                anyhow!("Couldn't reach {url}. Is `ollama serve` running?")
            } else {
                anyhow!("network error talking to {url}: {e}")
            }
        })?;
    let status = r.status();
    eprintln!("[llm] ollama response {status} after {:?}", started.elapsed());
    if !status.is_success() {
        let body = r.text().await.unwrap_or_default();
        eprintln!("[llm] ollama error body: {body}");
        return Err(anyhow!("HTTP {status} from {url}: {body}"));
    }

    // Ollama streams newline-delimited JSON. Each chunk frame can land at any
    // byte boundary, so we accumulate into a buffer and parse on '\n'. Each
    // frame's content/thinking fields are *deltas* — we accumulate content
    // for the return value and forward thinking to the caller's callback.
    use futures_util::StreamExt;
    let mut byte_stream = r.bytes_stream();
    let mut buf = String::new();
    let mut content = String::new();
    let mut thinking_chars: usize = 0;
    let mut chunks_seen: usize = 0;

    while let Some(chunk_res) = byte_stream.next().await {
        let bytes = chunk_res.map_err(|e| anyhow!("stream read: {e}"))?;
        // Lossy is fine — Ollama's frames are ASCII/UTF-8 JSON; if a multibyte
        // character spans frames the next chunk will replay the prefix.
        buf.push_str(&String::from_utf8_lossy(&bytes));
        while let Some(idx) = buf.find('\n') {
            let line: String = buf.drain(..=idx).collect();
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let frame: OllamaStreamChunk = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("[llm] could not parse stream frame: {e}\n[llm] frame: {line}");
                    return Err(anyhow!("unexpected stream frame from {url}: {e}"));
                }
            };
            chunks_seen += 1;
            if let Some(t) = frame.message.thinking.as_deref() {
                if !t.is_empty() {
                    thinking_chars += t.len();
                    on_chunk(StreamChunk::Thinking(t));
                }
            }
            if !frame.message.content.is_empty() {
                content.push_str(&frame.message.content);
                on_chunk(StreamChunk::Content(&frame.message.content));
            }
            if frame.done {
                break;
            }
        }
    }

    eprintln!(
        "[llm] ollama success in {:?}, content {} chars, thinking {} chars, frames {}",
        started.elapsed(),
        content.len(),
        thinking_chars,
        chunks_seen
    );
    if content.trim().is_empty() {
        if thinking_chars > 0 {
            return Err(anyhow!(
                "{model} spent {thinking_chars} chars thinking and ran out of tokens \
                 before producing an answer. Disable thinking mode in Settings or \
                 increase the cap. Thinking is rarely worth the latency for \
                 summary work."
            ));
        }
        return Err(anyhow!("{model} returned an empty response"));
    }
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
