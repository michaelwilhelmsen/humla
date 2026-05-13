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
// summary can run 60s. 10 minutes is generous enough that genuine slow paths
// complete, while still surfacing a wedged server as an error rather than
// hanging the UI indefinitely.
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
    // One retry on transient send-side errors. reqwest reuses HTTP/2
    // connections from its pool; OpenAI's edge silently half-closes idle
    // ones, so a long-running app's first request after a quiet period can
    // fail with Kind::Request ("error sending request for url") before any
    // bytes leave the wire. A fresh connection always succeeds. Don't retry
    // on timeout (genuine slowness — a retry just doubles the wait) or
    // connect-refused (the server is unreachable, retrying is pointless).
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
                let retryable = !e.is_timeout() && !e.is_connect() && attempt == 0;
                eprintln!(
                    "[llm] send error after {:?}: timeout={} connect={} attempt={} retrying={} body={}",
                    started.elapsed(),
                    e.is_timeout(),
                    e.is_connect(),
                    attempt,
                    retryable,
                    e
                );
                if retryable {
                    attempt += 1;
                    // Short backoff so we don't immediately reuse the same
                    // stale pooled connection. reqwest's pool is FIFO-ish;
                    // by the time we re-enter `.send()` the bad entry is
                    // typically already discarded by hyper's keepalive
                    // checker. 500ms is empirically enough.
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    continue;
                }
                if e.is_timeout() {
                    return Err(anyhow!(
                        "Timed out after 10 minutes waiting for {base_url}. \
                         The local model may be stuck — restart your \
                         local-LLM server (e.g. `pkill ollama && ollama serve`)."
                    ));
                }
                if e.is_connect() {
                    return Err(anyhow!(
                        "Couldn't reach {base_url}. Is your local-LLM \
                         server running? (ollama serve, etc.)"
                    ));
                }
                return Err(anyhow!("network error talking to {base_url}: {e}"));
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
    mut on_chunk: F,
) -> Result<String>
where
    F: FnMut(StreamChunk) + Send,
{
    let url = format!("{native_base}/chat");
    let num_predict: i32 = if think { 8192 } else { 4096 };
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
    let approx_input_tokens = (system_prompt.len() + user_message.len()) / 4;
    let need = approx_input_tokens + (num_predict as usize) + 512;
    let mut ctx: usize = 8192;
    while ctx < need && ctx < 65536 {
        ctx *= 2;
    }
    let num_ctx = ctx as i32;
    let req = OllamaChatRequest {
        model,
        messages: vec![
            ChatMessage { role: "system", content: system_prompt },
            ChatMessage { role: "user", content: user_message },
        ],
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
        "[llm] POST {url} (ollama-native, streaming) model={model} think={think} system_chars={} user_chars={} num_ctx={num_ctx}",
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
