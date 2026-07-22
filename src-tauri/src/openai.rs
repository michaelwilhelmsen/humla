use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const BASE: &str = "https://api.openai.com/v1";

// Walk a reqwest::Error's source chain into a single readable string. The
// outer Display on `Kind::Request` only says "error sending request for url
// (...)" — the actual cause (DNS, TLS, hyper) is buried in `.source()`.
fn error_chain(err: &(dyn std::error::Error + 'static)) -> String {
    let mut parts = vec![err.to_string()];
    let mut src = err.source();
    while let Some(e) = src {
        let s = e.to_string();
        if !parts.iter().any(|p| p == &s) {
            parts.push(s);
        }
        src = e.source();
    }
    parts.join(" -> ")
}

pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .expect("reqwest client")
}

// Local LLM servers (Ollama especially) cold-load the model on first request
// (~10s on a 9B), then generate at ~30 tok/s on Apple Silicon. A long-meeting
// summary can run 60s. 10 minutes is generous enough that genuine slow paths
// complete, while still surfacing a wedged server as an error rather than
// hanging the UI indefinitely.
fn local_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .expect("reqwest client")
}

// Cloud OpenAI summary path. Separate from `client()` (which is shared with
// transcription, ping, list_models — all short ops) because a long meeting
// transcript through a reasoning model can legitimately need 2–3 minutes,
// and the default 120s timeout was tripping on those.
fn summary_cloud_client() -> reqwest::Client {
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

#[derive(Deserialize)]
struct VerboseTranscribeResponse {
    text: String,
    #[serde(default)]
    words: Vec<VerboseWord>,
}

#[derive(Deserialize)]
struct VerboseWord {
    word: String,
    start: f64,
    end: f64,
}

/// One word's display text + chunk-relative ms bounds. Mirrors the local-
/// Whisper `Word` type so callers can plumb either provider's output through
/// the same downstream path. Empty for OpenAI models that don't return
/// word-level timing.
#[derive(Clone, Debug, Default)]
pub struct TranscribeWord {
    pub text: String,
    pub start_ms: u64,
    pub end_ms: u64,
}

/// True iff the OpenAI transcribe model returns word-level timestamps when
/// asked for `verbose_json` + `timestamp_granularities[]=word`. Only the
/// classic `whisper-1` endpoint supports that combination — the gpt-4o
/// transcribe family rejects `verbose_json` outright, and the `-diarize`
/// variant has its own segment-shaped response. Gating here keeps the cloud
/// path single-codepath while still extracting word timings when the model
/// is capable of producing them.
fn supports_verbose_words(model: &str) -> bool {
    model == "whisper-1"
}

pub async fn transcribe_file(
    api_key: &str,
    model: &str,
    language: Option<&str>,
    prompt: Option<&str>,
    audio_path: &Path,
) -> Result<(String, Vec<TranscribeWord>)> {
    let bytes = tokio::fs::read(audio_path).await?;
    let file_name = audio_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("chunk.wav")
        .to_string();
    let part = reqwest::multipart::Part::bytes(bytes)
        .file_name(file_name)
        .mime_str("audio/wav")?;

    let want_words = supports_verbose_words(model);
    let response_format = if want_words { "verbose_json" } else { "json" };

    let mut form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("model", model.to_string())
        .text("response_format", response_format.to_string())
        // Force deterministic decoding so Whisper doesn't hallucinate filler
        // phrases and silently drift to a different language on short audio.
        .text("temperature", "0".to_string());
    if want_words {
        // The API expects an array — multipart `timestamp_granularities[]`
        // is the documented name for the per-element field. Word grain
        // alone is enough; segment grain comes back implicitly.
        form = form.text("timestamp_granularities[]", "word".to_string());
    }
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

    if want_words {
        let body: VerboseTranscribeResponse = r.json().await?;
        let words = body
            .words
            .into_iter()
            .filter_map(|w| {
                let text = w.word.trim().to_string();
                if text.is_empty() {
                    return None;
                }
                // OpenAI returns float seconds; clamp negatives to 0 and saturate
                // on overflow. Anything > u64::MAX ms is six hundred million
                // years of audio so the floor is fine.
                let start_ms = (w.start.max(0.0) * 1000.0).round() as u64;
                let end_ms = (w.end.max(0.0) * 1000.0).round() as u64;
                Some(TranscribeWord {
                    text,
                    start_ms,
                    end_ms: end_ms.max(start_ms),
                })
            })
            .collect();
        Ok((body.text, words))
    } else {
        let body: TranscribeResponse = r.json().await?;
        Ok((body.text, Vec::new()))
    }
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
pub(crate) fn is_reasoning_model(model: &str) -> bool {
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

#[derive(Serialize)]
struct ChatStreamRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage<'a>>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
}

// One Server-Sent-Events frame from OpenAI's streaming /chat/completions.
// Each `data:` line (except the terminal `[DONE]`) is one of these; the
// answer is assembled from `choices[0].delta.content`.
#[derive(Deserialize)]
struct ChatStreamFrame {
    #[serde(default)]
    choices: Vec<ChatStreamChoice>,
}

#[derive(Deserialize)]
struct ChatStreamChoice {
    #[serde(default)]
    delta: ChatStreamDelta,
}

#[derive(Deserialize, Default)]
struct ChatStreamDelta {
    #[serde(default)]
    content: Option<String>,
}

/// Stream a multi-turn chat completion from cloud OpenAI. The summary path
/// deliberately doesn't stream cloud responses, but chat needs live tokens, so
/// this uses `stream: true` and parses the SSE frames. `on_delta` fires once
/// per content delta; the full assembled answer is returned.
pub(crate) async fn openai_chat_stream<F>(
    base_url: &str,
    api_key: &str,
    model: &str,
    messages: &[(&str, &str)],
    mut on_delta: F,
) -> Result<String>
where
    F: FnMut(&str) + Send,
{
    let req = ChatStreamRequest {
        model,
        messages: messages
            .iter()
            .map(|(role, content)| ChatMessage { role, content })
            .collect(),
        stream: true,
        // Reasoning models (gpt-5.x / o-series) reject a custom temperature.
        temperature: if is_reasoning_model(model) { None } else { Some(0.3) },
    };
    let url = format!("{base_url}/chat/completions");
    let started = std::time::Instant::now();
    let r = summary_cloud_client()
        .post(&url)
        .bearer_auth(api_key)
        .json(&req)
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                anyhow!("Timed out waiting for OpenAI. Try again.")
            } else if e.is_connect() {
                anyhow!("Couldn't reach OpenAI. Check your internet connection.")
            } else {
                anyhow!("Network error talking to OpenAI: {}", error_chain(&e))
            }
        })?;
    let status = r.status();
    if !status.is_success() {
        let body = r.text().await.unwrap_or_default();
        return Err(anyhow!("HTTP {status} from {base_url}: {body}"));
    }

    // SSE: newline-delimited `data: {json}` frames, terminated by `data: [DONE]`.
    // Frames can split across byte chunks, so buffer and parse per line.
    use futures_util::StreamExt;
    let mut byte_stream = r.bytes_stream();
    let mut buf = String::new();
    let mut answer = String::new();
    while let Some(chunk_res) = byte_stream.next().await {
        let bytes = chunk_res.map_err(|e| anyhow!("stream read: {e}"))?;
        buf.push_str(&String::from_utf8_lossy(&bytes));
        while let Some(idx) = buf.find('\n') {
            let line: String = buf.drain(..=idx).collect();
            let line = line.trim();
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data == "[DONE]" {
                break;
            }
            let frame: ChatStreamFrame = match serde_json::from_str(data) {
                Ok(v) => v,
                // Skip keep-alive comments / unparseable heartbeats rather than
                // aborting the whole answer over one odd frame.
                Err(_) => continue,
            };
            if let Some(delta) = frame
                .choices
                .into_iter()
                .next()
                .and_then(|c| c.delta.content)
            {
                if !delta.is_empty() {
                    answer.push_str(&delta);
                    on_delta(&delta);
                }
            }
        }
    }
    eprintln!(
        "[llm] openai chat stream done in {:?}, {} chars",
        started.elapsed(),
        answer.len()
    );
    if answer.trim().is_empty() {
        return Err(anyhow!("{model} returned an empty response"));
    }
    Ok(answer)
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
    let http = if is_local { local_client() } else { summary_cloud_client() };
    let url = format!("{base_url}/chat/completions");
    let started = std::time::Instant::now();
    eprintln!(
        "[llm] POST {url} model={model} system_chars={} user_chars={}",
        system_prompt.len(),
        transcript.len()
    );
    // Retry transient send-side errors. reqwest reuses HTTP/2 connections
    // from its pool; OpenAI's edge silently half-closes idle ones, so a
    // long-running app's first request after a quiet period can fail with
    // Kind::Request ("error sending request for url") before any bytes
    // leave the wire. A fresh connection usually succeeds, but in the wild
    // we've seen the second attempt also fail (brief DNS / TLS hiccups),
    // so allow up to two retries. Don't retry on timeout (genuine slowness
    // — a retry just doubles the wait) or connect-refused (the server is
    // unreachable, retrying is pointless).
    const MAX_RETRIES: u32 = 2;
    let mut attempt: u32 = 0;
    let r = loop {
        let send_res = http
            .post(&url)
            .bearer_auth(api_key)
            .json(&req)
            .send()
            .await;
        match send_res {
            Ok(resp) => break resp,
            Err(e) => {
                let retryable =
                    !e.is_timeout() && !e.is_connect() && attempt < MAX_RETRIES;
                eprintln!(
                    "[llm] send error after {:?}: timeout={} connect={} attempt={} retrying={} body={} source={}",
                    started.elapsed(),
                    e.is_timeout(),
                    e.is_connect(),
                    attempt,
                    retryable,
                    e,
                    error_chain(&e),
                );
                if retryable {
                    attempt += 1;
                    // Backoff lengthens with each retry so we don't immediately
                    // reuse the same stale pooled connection and so brief
                    // network blips have time to clear. 500ms then 1.5s.
                    let backoff_ms = 500u64.saturating_mul(attempt as u64).max(500);
                    tokio::time::sleep(std::time::Duration::from_millis(backoff_ms))
                        .await;
                    continue;
                }
                if e.is_timeout() {
                    let secs = started.elapsed().as_secs();
                    if is_local {
                        return Err(anyhow!(
                            "Timed out after {secs}s waiting for {base_url}. \
                             The local model may be stuck — restart your \
                             local-LLM server (e.g. `pkill ollama && ollama serve`)."
                        ));
                    }
                    return Err(anyhow!(
                        "Timed out after {secs}s waiting for {base_url}. \
                         OpenAI's response was unusually slow — try again, \
                         or switch the summary provider to Local."
                    ));
                }
                if e.is_connect() {
                    if is_local {
                        return Err(anyhow!(
                            "Couldn't reach {base_url}. Is your local-LLM \
                             server running? (ollama serve, etc.)"
                        ));
                    }
                    return Err(anyhow!(
                        "Couldn't reach {base_url}. Check your internet \
                         connection and try again."
                    ));
                }
                let cause = error_chain(&e);
                if is_local {
                    return Err(anyhow!(
                        "Network error talking to {base_url}: {cause}. \
                         Check that your local-LLM server is reachable."
                    ));
                }
                return Err(anyhow!(
                    "Network error talking to OpenAI: {cause}. \
                     Check your internet connection (DNS / VPN / proxy) and try again."
                ));
            }
        }
    };

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
pub(crate) fn ollama_native_url(openai_compat_url: &str) -> Option<String> {
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
    // Seconds to keep the model resident after the response finishes.
    // 0 = unload immediately; default would be 300 (5 min). With
    // num_ctx=65536 the KV cache is multi-GB, so leaving it warm pins a
    // big chunk of RAM and keeps the OS memory compressor busy long
    // after the summary completes. Trade-off: the next summary in the
    // same session reloads the model (~5-15s).
    keep_alive: i32,
    options: OllamaOptions,
}

// Per Qwen team's HuggingFace model cards for Qwen 3.5 (9B/4B/2B/0.8B):
//   thinking, general:   temp=1.0, top_p=0.95, top_k=20, min_p=0.0,
//                        presence_penalty=1.5, repetition_penalty=1.0
//   non-thinking, general: temp=0.7, top_p=0.8,  top_k=20, min_p=0.0,
//                        presence_penalty=1.5, repetition_penalty=1.0
//
// presence_penalty=1.5 breaks *thinking-phase* loops (the "Wait, I need to
// check the language. Okay, let's write." cycle) because the cycle alternates
// between distinct constraint phrases — penalizing each token's first
// reappearance is enough to push the sampler off-track.
//
// presence_penalty does NOT reliably stop *content-phase* token loops like
// "Note: Wilma sa nei. Note: Michael tilbød yoghurt." repeating 100×. Once
// every token in the looped phrase has appeared once, presence_penalty
// applies a uniform constant — no differential pressure remains. For that
// case we add frequency_penalty (scales with token count, so each loop
// iteration further suppresses the looped tokens) and a final post-processing
// pass in `trim_runaway_repetition()`. Qwen team's recs leave
// frequency_penalty at default 0; we override only because their tuning
// targets benchmark prompts where content runaway is rare. Long structured
// summaries on small models hit it more often.
#[derive(Serialize)]
struct OllamaOptions {
    temperature: f32,
    top_p: f32,
    top_k: i32,
    min_p: f32,
    presence_penalty: f32,
    frequency_penalty: f32,
    repeat_penalty: f32,
    // Hard cap on generated tokens. Without this, Qwen 3+ thinking mode can
    // burn 5K+ tokens reasoning before answering even on tiny inputs.
    num_predict: i32,
    // Input context window. Ollama's default is 2048 tokens — way too small
    // for meeting transcripts. Anything longer than ~1500 words is silently
    // truncated from the front. Sized adaptively at the call site based on
    // actual prompt length + output budget, bounded [8192, 65536]. Caller
    // computes the value because too-large a window inflates KV-cache RAM
    // 2-4 GB and OOMs Ollama on tighter machines.
    num_ctx: i32,
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
    on_chunk: F,
) -> Result<String>
where
    F: FnMut(StreamChunk) + Send,
{
    // Summaries are always a fixed system+user pair. The streaming core is
    // shared with chat (which sends a multi-turn history) via a (role, content)
    // slice so the Qwen sampling tuning + adaptive num_ctx live in one place.
    ollama_chat_stream(
        native_base,
        model,
        think,
        &[("system", system_prompt), ("user", user_message)],
        on_chunk,
    )
    .await
}

/// Stream a multi-turn chat completion from an Ollama server via its native
/// `/api/chat` endpoint. Each `(role, content)` pair becomes a message in
/// order. Shared by the summary path (system+user) and the chat command
/// (system + reference + turns). `num_ctx` is sized adaptively from the total
/// prompt length so long histories get a bigger KV cache without a flat 65K
/// pinning multi-GB of RAM on tighter Macs.
pub(crate) async fn ollama_chat_stream<F>(
    native_base: &str,
    model: &str,
    think: bool,
    messages: &[(&str, &str)],
    mut on_chunk: F,
) -> Result<String>
where
    F: FnMut(StreamChunk) + Send,
{
    let url = format!("{native_base}/chat");
    let num_predict: i32 = if think { 8192 } else { 4096 };
    let prompt_chars: usize = messages.iter().map(|(_, c)| c.len()).sum();
    // Adaptive num_ctx: size the KV cache to the actual prompt + output
    // budget, not a fixed 65536. A flat 65K was killing Ollama on tighter
    // machines ("model runner has unexpectedly stopped") because the KV
    // cache for that window can run 2-4 GB on top of model weights — fine
    // on a 32 GB Mac with nothing else running, OOM otherwise. Rough
    // estimate: ~4 chars/token for English/Norwegian; round up to the
    // next power of two for clean Ollama allocation; bound to
    // [8192, 65536]. A typical 2-hour meeting (~20K input tokens) gets
    // 32K context — half the RAM of the old fixed value, still 10× more
    // than Ollama's silent-truncating 2048 default.
    let approx_input_tokens = prompt_chars / 4;
    let need = approx_input_tokens + (num_predict as usize) + 512;
    let mut ctx: usize = 8192;
    while ctx < need && ctx < 65536 {
        ctx *= 2;
    }
    let num_ctx = ctx as i32;
    let req = OllamaChatRequest {
        model,
        messages: messages
            .iter()
            .map(|(role, content)| ChatMessage { role, content })
            .collect(),
        stream: true,
        think,
        keep_alive: 0,
        options: OllamaOptions {
            // Mode-specific temp + top_p per Qwen team. Higher temp in
            // thinking is counter-intuitive but their reasoning is that
            // determinism (low temp) is exactly what locks the model into
            // the same loop branch each step — sampling diversity is the
            // escape hatch, with presence_penalty preventing it from
            // wandering into repetition.
            temperature: if think { 1.0 } else { 0.7 },
            top_p: if think { 0.95 } else { 0.8 },
            top_k: 20,
            min_p: 0.0,
            presence_penalty: 1.5,
            frequency_penalty: 0.5,
            repeat_penalty: 1.0,
            // Thinking burns thousands of reasoning tokens before the final
            // answer; 4096 is enough for the fast path, 8192 gives thinking
            // headroom while still failing fast on degenerate loops (was
            // 16384, but a stuck Qwen takes ~9 minutes to hit that — too
            // long to wait for the timeout to free up Ollama).
            num_predict,
            num_ctx,
        },
    };
    let started = std::time::Instant::now();
    eprintln!(
        "[llm] POST {url} (ollama-native, streaming) model={model} think={think} messages={} prompt_chars={prompt_chars} num_ctx={num_ctx}",
        messages.len()
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
                let secs = started.elapsed().as_secs();
                anyhow!("Timed out after {secs}s waiting for {url}. Restart Ollama and try again.")
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
    let trimmed = trim_runaway_repetition(&content);
    if trimmed.len() < content.len() {
        eprintln!(
            "[llm] trimmed runaway repetition: {} → {} chars",
            content.len(),
            trimmed.len()
        );
    }
    Ok(trimmed)
}

// ── Tool-calling steps for agentic chat (issue #47) ─────────────────────────
// These run ONE step of the loop: offer `tools` (JSON-Schema tool defs already
// lowered to each provider's envelope by the caller) alongside the full
// re-lowered conversation, and return the assistant's text plus any tool calls
// it requested. The caller (`chat::run_chat`) executes the calls and loops.
// `messages`/`tools` are pre-lowered `serde_json::Value`s so this wire layer
// stays free of the chat module's types.

use serde_json::Value;

/// A tool call parsed off the wire: provider-supplied (or synthesised) `id`,
/// tool `name`, and raw JSON `arguments` as a string.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RawToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Serialize)]
struct ToolChatRequest<'a> {
    model: &'a str,
    messages: &'a [Value],
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [Value]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
}

// Streaming SSE delta shapes for OpenAI tool calls. `tool_calls` arrive as
// index-keyed fragments: the first fragment for an index carries `id` + name,
// later ones append `arguments` text. We accumulate by index.
#[derive(Deserialize, Default)]
struct ToolStreamDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ToolCallFragment>,
}
#[derive(Deserialize)]
struct ToolCallFragment {
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<ToolFnFragment>,
}
#[derive(Deserialize, Default)]
struct ToolFnFragment {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}
#[derive(Deserialize)]
struct ToolStreamFrame {
    #[serde(default)]
    choices: Vec<ToolStreamChoice>,
}
#[derive(Deserialize)]
struct ToolStreamChoice {
    #[serde(default)]
    delta: ToolStreamDelta,
}

/// Run one cloud-OpenAI step with tool-calling. Streams answer tokens via
/// `on_delta`; buffers streamed tool-call fragments and returns them assembled.
/// `tools` is a slice of OpenAI tool-def objects (empty = force a text answer).
pub(crate) async fn openai_chat_step<F>(
    base_url: &str,
    api_key: &str,
    model: &str,
    messages: &[Value],
    tools: &[Value],
    mut on_delta: F,
) -> Result<(String, Vec<RawToolCall>)>
where
    F: FnMut(&str) + Send,
{
    let req = ToolChatRequest {
        model,
        messages,
        stream: true,
        tools: if tools.is_empty() { None } else { Some(tools) },
        temperature: if is_reasoning_model(model) { None } else { Some(0.2) },
    };
    let url = format!("{base_url}/chat/completions");
    let r = summary_cloud_client()
        .post(&url)
        .bearer_auth(api_key)
        .json(&req)
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                anyhow!("Timed out waiting for OpenAI. Try again.")
            } else if e.is_connect() {
                anyhow!("Couldn't reach OpenAI. Check your internet connection.")
            } else {
                anyhow!("Network error talking to OpenAI: {}", error_chain(&e))
            }
        })?;
    let status = r.status();
    if !status.is_success() {
        let body = r.text().await.unwrap_or_default();
        return Err(anyhow!("HTTP {status} from {base_url}: {body}"));
    }

    use futures_util::StreamExt;
    let mut byte_stream = r.bytes_stream();
    let mut buf = String::new();
    let mut answer = String::new();
    // Accumulate tool-call fragments by index → (id, name, arguments).
    let mut calls: Vec<(String, String, String)> = Vec::new();
    while let Some(chunk_res) = byte_stream.next().await {
        let bytes = chunk_res.map_err(|e| anyhow!("stream read: {e}"))?;
        buf.push_str(&String::from_utf8_lossy(&bytes));
        while let Some(idx) = buf.find('\n') {
            let line: String = buf.drain(..=idx).collect();
            let line = line.trim();
            let Some(data) = line.strip_prefix("data:") else { continue };
            let data = data.trim();
            if data == "[DONE]" {
                break;
            }
            let frame: ToolStreamFrame = match serde_json::from_str(data) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let Some(choice) = frame.choices.into_iter().next() else { continue };
            if let Some(delta) = choice.delta.content {
                if !delta.is_empty() {
                    answer.push_str(&delta);
                    on_delta(&delta);
                }
            }
            for frag in choice.delta.tool_calls {
                if calls.len() <= frag.index {
                    calls.resize(frag.index + 1, (String::new(), String::new(), String::new()));
                }
                let slot = &mut calls[frag.index];
                if let Some(id) = frag.id {
                    slot.0 = id;
                }
                if let Some(f) = frag.function {
                    if let Some(name) = f.name {
                        slot.1 = name;
                    }
                    if let Some(args) = f.arguments {
                        slot.2.push_str(&args);
                    }
                }
            }
        }
    }

    let tool_calls: Vec<RawToolCall> = calls
        .into_iter()
        .enumerate()
        .filter(|(_, (_, name, _))| !name.is_empty())
        .map(|(i, (id, name, arguments))| RawToolCall {
            id: if id.is_empty() { format!("call_{i}") } else { id },
            name,
            arguments: if arguments.trim().is_empty() { "{}".into() } else { arguments },
        })
        .collect();

    if answer.trim().is_empty() && tool_calls.is_empty() {
        return Err(anyhow!("{model} returned an empty response"));
    }
    Ok((answer, tool_calls))
}

// Ollama native `/api/chat` step with tools. Per spike #45 this uses
// `stream: false` (buffered) — the reliability probe validated tool-calling in
// exactly this mode, and small local models frame tool calls inconsistently
// when streamed. The buffered content is delivered to `on_delta` in one piece
// so the UI still renders the final answer. Ollama tool calls carry no id, so
// we synthesise one; `arguments` may be an object or a string and is
// normalised to a JSON string.
#[derive(Serialize)]
struct OllamaToolChatRequest<'a> {
    model: &'a str,
    messages: &'a [Value],
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [Value]>,
    stream: bool,
    think: bool,
    keep_alive: i32,
    options: OllamaToolOptions,
}
// Tool-calling wants determinism, not the Qwen anti-loop sampling used for
// long summaries — a low temperature makes tool selection + args reliable
// (the spike ran at temperature 0).
#[derive(Serialize)]
struct OllamaToolOptions {
    temperature: f32,
    num_predict: i32,
    num_ctx: i32,
}
#[derive(Deserialize)]
struct OllamaToolResponse {
    message: OllamaToolMessage,
}
#[derive(Deserialize, Default)]
struct OllamaToolMessage {
    #[serde(default)]
    content: String,
    #[serde(default)]
    tool_calls: Vec<OllamaToolCallWire>,
}
#[derive(Deserialize)]
struct OllamaToolCallWire {
    function: OllamaToolFn,
}
#[derive(Deserialize)]
struct OllamaToolFn {
    name: String,
    #[serde(default)]
    arguments: Value,
}

pub(crate) async fn ollama_chat_step<F>(
    native_base: &str,
    model: &str,
    messages: &[Value],
    tools: &[Value],
    mut on_delta: F,
) -> Result<(String, Vec<RawToolCall>)>
where
    F: FnMut(&str) + Send,
{
    let url = format!("{native_base}/chat");
    let prompt_chars: usize = messages
        .iter()
        .map(|m| m.get("content").and_then(|c| c.as_str()).map(str::len).unwrap_or(0))
        .sum();
    let num_predict = 4096i32;
    let approx_input_tokens = prompt_chars / 4;
    let need = approx_input_tokens + (num_predict as usize) + 512;
    let mut ctx: usize = 8192;
    while ctx < need && ctx < 65536 {
        ctx *= 2;
    }
    let req = OllamaToolChatRequest {
        model,
        messages,
        tools: if tools.is_empty() { None } else { Some(tools) },
        stream: false,
        think: false,
        keep_alive: 0,
        options: OllamaToolOptions { temperature: 0.0, num_predict, num_ctx: ctx as i32 },
    };
    let started = std::time::Instant::now();
    eprintln!(
        "[chat] POST {url} (ollama-native, tools={}) model={model} messages={} prompt_chars={prompt_chars} num_ctx={ctx}",
        tools.len(),
        messages.len(),
    );
    let r = local_client()
        .post(&url)
        .json(&req)
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                anyhow!("Timed out waiting for {url}. Restart Ollama and try again.")
            } else if e.is_connect() {
                anyhow!("Couldn't reach {url}. Is `ollama serve` running?")
            } else {
                anyhow!("network error talking to {url}: {e}")
            }
        })?;
    let status = r.status();
    if !status.is_success() {
        let body = r.text().await.unwrap_or_default();
        return Err(anyhow!("HTTP {status} from {url}: {body}"));
    }
    let resp: OllamaToolResponse = r
        .json()
        .await
        .map_err(|e| anyhow!("could not parse Ollama response from {url}: {e}"))?;
    let tool_calls: Vec<RawToolCall> = resp
        .message
        .tool_calls
        .into_iter()
        .enumerate()
        .map(|(i, tc)| {
            let arguments = match tc.function.arguments {
                Value::String(s) => s,
                other => serde_json::to_string(&other).unwrap_or_else(|_| "{}".into()),
            };
            RawToolCall { id: format!("call_{i}"), name: tc.function.name, arguments }
        })
        .collect();
    let content = trim_runaway_repetition(&resp.message.content);
    if !content.is_empty() {
        on_delta(&content);
    }
    eprintln!(
        "[chat] ollama step done in {:?}: {} chars, {} tool calls",
        started.elapsed(),
        content.len(),
        tool_calls.len(),
    );
    if content.trim().is_empty() && tool_calls.is_empty() {
        return Err(anyhow!("{model} returned an empty response"));
    }
    Ok((content, tool_calls))
}

/// Detect runaway repetition (the same non-empty line repeated 3+ times
/// consecutively) and truncate at the first repetition. Qwen 3.5 sometimes
/// produces a clean summary, then degenerates into "Note: Wilma sa nei.
/// Note: Michael tilbød yoghurt." for thousands of tokens — sampling
/// penalties (presence_penalty=1.5, frequency_penalty=0.5) slow this down
/// but don't always kill it before num_predict expires. Final safety net.
///
/// Conservative on purpose: only triggers on *exact* line equality (after
/// trim) and requires 3+ consecutive copies. False positives (truncating
/// a list that legitimately repeats a short phrase) are worse than missing
/// some tail spam.
fn trim_runaway_repetition(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() < 3 {
        return text.to_string();
    }
    let mut i = 0;
    while i + 2 < lines.len() {
        let normalized = lines[i].trim();
        if !normalized.is_empty()
            && lines[i + 1].trim() == normalized
            && lines[i + 2].trim() == normalized
        {
            return lines[..i].join("\n").trim_end().to_string();
        }
        i += 1;
    }
    text.to_string()
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
    fn trim_truncates_at_3plus_consecutive_dupes() {
        let input = "Hovedtemaer\n- A\n- B\n\nNote: spam.\nNote: spam.\nNote: spam.\nNote: spam.";
        let out = trim_runaway_repetition(input);
        assert_eq!(out, "Hovedtemaer\n- A\n- B");
    }

    #[test]
    fn trim_keeps_clean_output() {
        let input = "Hovedtemaer\n- A\n- B\n- C\n\nTilbakemeldinger\n- One\n- Two";
        let out = trim_runaway_repetition(input);
        assert_eq!(out, input);
    }

    #[test]
    fn trim_keeps_two_consecutive_dupes() {
        // A list with two identical entries shouldn't trigger; only 3+ does.
        let input = "- Same\n- Same\n- Different";
        let out = trim_runaway_repetition(input);
        assert_eq!(out, input);
    }

    #[test]
    fn trim_ignores_empty_line_runs() {
        // Multiple blank lines must not count as repetition.
        let input = "Header\n\n\n\nBody";
        let out = trim_runaway_repetition(input);
        assert_eq!(out, input);
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
