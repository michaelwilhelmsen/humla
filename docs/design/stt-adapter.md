# STT provider adapter — design doc

Status: proposal · Author: research synthesis from `fastrepl/anarlog` · Target: Humla 0.21+

## Problem

Humla currently supports two transcription paths and they don't share any abstraction:

- `openai::transcribe_file(api_key, model, language, prompt, audio_path)` — multipart POST, returns `(String, Vec<TranscribeWord>)`.
- `local_whisper::transcribe_file_with_words(shared, model_path, use_gpu, language, prompt, preset, audio_path)` — in-process whisper-rs, returns `(String, Vec<Word>)`.

The dispatch is a string match in `commands.rs::transcribe_chunk` (`src-tauri/src/commands.rs:3514-3553`):

```rust
let (text, words) = match cfg.provider.as_str() {
    "local" => { /* assemble shared, model_path, use_gpu, preset; call local_whisper */ }
    _       => { /* assemble api_key, model; call openai */ }
};
```

Settings are stored as flat strings (`transcribe_provider` ∈ `{"openai","local"}`, `transcribe_model`, `whisper_preset`, `language`). Adding a new provider means a new branch, new settings keys, new keychain entry, new validation path. With only 2 providers it's fine; at 4+ it becomes the bottleneck on every quality experiment.

The competitive picture (see `docs/research/competitor-research-2026-05.md` if filed): Deepgram and AssemblyAI ship better speaker turns and faster batch latency than OpenAI Whisper for many use cases; Groq hosts `whisper-large-v3-turbo` at sub-realtime latency for $0.04/h. None of this is currently reachable without rewriting `transcribe_chunk`.

## Goal

Replace the string-match dispatch with a typed adapter trait, modeled on `fastrepl/anarlog`'s `owhisper-client::adapter` design. Goals in priority order:

1. Adding a provider becomes one new module — no edits to `transcribe_chunk` or settings code.
2. Auth schemes (header bearer, custom header, query param, body field) are encoded as data, not duplicated per adapter.
3. Local Whisper plugs into the same trait surface so `transcribe_chunk` has a single code path.
4. Per-note model selection (already a partial concept) becomes per-note **provider+model** with a typed config carrying provider-specific knobs.

Non-goals for this phase:

- WebSocket streaming. Humla transcribes chunks (1–15s WAV files); batch-shaped is the right surface. Anarlog's `RealtimeSttAdapter` exists but isn't needed yet.
- Multi-channel native stereo to providers. Humla already keeps mic/sys parallel as separate WAV streams; we transcribe each chunk mono.
- Diarization providers (Pyannote, Speechmatics). Humla's offline FluidAudio path is shipped and owns diarization; STT adapters return text + words only.

## Reference: anarlog's shape

Three traits in `crates/owhisper-client/src/adapter/mod.rs`:

- `RealtimeSttAdapter` — websocket streaming. Skip.
- `BatchSttAdapter` — one-shot file POST. **This is what Humla needs.**
- `CallbackSttAdapter` — submit + poll for async transcription. Skip for now.

The portable parts:

```rust
// crates/owhisper-client/src/providers.rs:13-26
pub enum Auth {
    Header { name: &'static str, prefix: Option<&'static str> },
    FirstMessage { field_name: &'static str },          // realtime only
    SessionInit { header_name: &'static str },          // Gladia
}
```

```rust
// crates/owhisper-config/src/lib.rs:24-35
#[serde(tag = "type")]
pub enum ModelConfig {
    Aws(AwsModelConfig),
    Deepgram(DeepgramModelConfig),
    WhisperCpp(WhisperCppModelConfig),
    Moonshine(MoonshineModelConfig),
}
```

The `Auth` enum is the cleanest single piece — it covers every cloud STT we'd want without any per-provider auth code. The tagged-union `ModelConfig` is what replaces Humla's flat settings strings.

## Proposed shape for Humla

### Trait

```rust
// src-tauri/src/stt/adapter.rs (new)
use std::path::Path;
use anyhow::Result;
use crate::stt::{Auth, TranscribeWord};

#[async_trait::async_trait]
pub trait BatchSttAdapter: Send + Sync {
    /// Stable identifier used in settings + logs. e.g. "openai", "deepgram".
    fn provider_id(&self) -> &'static str;

    /// User-facing label for the settings UI.
    fn label(&self) -> &'static str;

    /// True when this adapter accepts the given language code.
    /// "auto" means the provider does language detection itself.
    fn supports_language(&self, lang: &str) -> bool;

    /// True when this adapter returns word-level timestamps. Drives whether
    /// the playback view can do karaoke-style highlighting for chunks
    /// produced by this adapter.
    fn supports_word_timestamps(&self, model: &str) -> bool;

    async fn transcribe(
        &self,
        ctx: TranscribeCtx<'_>,
        audio: &Path,
    ) -> Result<TranscribeResult>;
}

pub struct TranscribeCtx<'a> {
    pub model: &'a str,
    pub language: &'a str,           // "auto" or BCP-47 code
    pub initial_prompt: Option<&'a str>,
    pub api_key: Option<&'a str>,    // None for in-process providers (local Whisper)
    pub base_url: Option<&'a str>,   // override; None = adapter's default
}

pub struct TranscribeResult {
    pub text: String,
    pub words: Vec<TranscribeWord>,  // empty when adapter doesn't expose them
}
```

Key shape decisions:

- `async fn` via `async-trait`. The implementations are I/O-bound (HTTP) or CPU-bound (whisper-rs via `spawn_blocking`); `async-trait` is fine, no need to invent an associated-type pattern.
- `TranscribeCtx` bundles all dispatch-time data. Keeps the function signature stable as we add knobs (e.g. `temperature`, `vocabulary_id`). Lifetimes mirror what `transcribe_chunk` already holds in locals.
- `TranscribeResult` is a tiny struct, not a tuple, so future fields (e.g. detected language, confidence) don't break callers.
- `provider_id` is `&'static str`, not `String`, because it's a constant. Settings serialize this verbatim.

### Auth (lifted directly from anarlog)

```rust
// src-tauri/src/stt/auth.rs (new)
#[derive(Clone, Copy, Debug)]
pub enum Auth {
    /// Authorization: <prefix><key>
    Header { name: &'static str, prefix: Option<&'static str> },
    /// query string param: ?<name>=<key>
    QueryParam { name: &'static str },
    /// JSON body field set on the request payload
    BodyField { name: &'static str },
}

impl Auth {
    pub fn apply(&self, req: reqwest::RequestBuilder, key: &str) -> reqwest::RequestBuilder {
        match self {
            Auth::Header { name, prefix } => {
                let value = match prefix {
                    Some(p) => format!("{p}{key}"),
                    None => key.to_string(),
                };
                req.header(*name, value)
            }
            Auth::QueryParam { name } => req.query(&[(name, key)]),
            // BodyField needs the caller to mutate the JSON body; helpers below
            Auth::BodyField { .. } => req,
        }
    }
}
```

I dropped anarlog's `FirstMessage` and `SessionInit` variants (websocket-only). Added `QueryParam` (Groq's older Whisper endpoint accepts `?api_key=`) and `BodyField` (some self-hosted forks of whisper.cpp-server take key in JSON body). Three variants cover every cloud provider on the candidate list.

### Settings: tagged-union ModelConfig

Today's settings are a flat blob of strings: `transcribe_provider="openai"`, `transcribe_model="gpt-4o-transcribe"`, `whisper_preset="quality"`, etc. Different providers need different fields and validation.

Replace with one JSON-serialized setting per provider, plus a top-level `transcribe_active` pointer:

```rust
// src-tauri/src/stt/config.rs (new)
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "provider")]
pub enum ProviderConfig {
    #[serde(rename = "openai")]
    OpenAi(OpenAiConfig),
    #[serde(rename = "local")]
    Local(LocalWhisperConfig),
    #[serde(rename = "deepgram")]
    Deepgram(DeepgramConfig),
    #[serde(rename = "groq")]
    Groq(GroqConfig),
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct OpenAiConfig {
    pub model: String,           // e.g. "whisper-1", "gpt-4o-transcribe"
    pub base_url: Option<String>,// override for OpenAI-compat servers
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct LocalWhisperConfig {
    pub model_id: String,        // matches local_whisper::ModelInfo::id
    pub preset: String,          // "fast" | "balanced" | "quality"
    pub use_gpu: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DeepgramConfig {
    pub model: String,           // "nova-3", "nova-2", etc.
    pub base_url: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct GroqConfig {
    pub model: String,           // "whisper-large-v3-turbo"
}
```

Stored under settings key `transcribe_config` as JSON. Migration: on first read, if `transcribe_config` is missing, build it from the legacy `transcribe_provider` + `transcribe_model` + `whisper_preset` keys. This is the same pattern Humla already uses for the legacy keychain migration in `read_openai_api_key`.

API keys stay in Keychain, one entry per provider (`service = "no.humla.app"`, `account = "openai_api_key" | "deepgram_api_key" | …`).

### Adapter implementations

Three concrete files in `src-tauri/src/stt/`:

```
src-tauri/src/stt/
├── mod.rs              — pub use, registry
├── adapter.rs          — BatchSttAdapter trait, TranscribeCtx, TranscribeResult
├── auth.rs             — Auth enum
├── config.rs           — ProviderConfig tagged union
├── openai.rs           — OpenAiAdapter (replaces openai::transcribe_file)
├── local.rs            — LocalWhisperAdapter (wraps local_whisper::transcribe_file_with_words)
├── deepgram.rs         — DeepgramAdapter (new)
└── groq.rs             — GroqAdapter (new — OpenAI-compat, but separate label/limits)
```

`local_whisper.rs` stays — the model registry, DTW preset selection, and `SharedContext` are not adapter concerns. `LocalWhisperAdapter` is a thin wrapper that holds an `Arc<AppState>` reference and forwards into the existing functions.

`openai.rs` (the existing file) keeps its summary functions untouched — the summary path is a separate concern from STT and shares only the `client()` builder. Move the `transcribe_file` function out, into `stt/openai.rs`, and delete the old call site once `transcribe_chunk` migrates.

### Registry

```rust
// src-tauri/src/stt/mod.rs
pub fn build_adapter(cfg: &ProviderConfig, state: &AppState) -> Box<dyn BatchSttAdapter> {
    match cfg {
        ProviderConfig::OpenAi(_)    => Box::new(OpenAiAdapter::new()),
        ProviderConfig::Local(_)     => Box::new(LocalWhisperAdapter::new(state.whisper.clone())),
        ProviderConfig::Deepgram(_)  => Box::new(DeepgramAdapter::new()),
        ProviderConfig::Groq(_)      => Box::new(GroqAdapter::new()),
    }
}

pub fn provider_options() -> &'static [ProviderOption] {
    // For the Settings UI: id, label, default config skeleton
    &PROVIDER_OPTIONS
}
```

Static dispatch over `Box<dyn>` is fine — `transcribe_chunk` calls one adapter per chunk, allocation cost is irrelevant next to the network round-trip or whisper-rs inference.

## Migration of `transcribe_chunk`

Before (`src-tauri/src/commands.rs:3514-3553`):

```rust
let (text, words) = match cfg.provider.as_str() {
    "local" => {
        let model_path = local_model_path(&app, &cfg.language)?;
        let (shared, use_gpu) = { /* … */ };
        let preset = local_whisper::Preset::from_setting(&cfg.whisper_preset);
        local_whisper::transcribe_file_with_words(shared, model_path, use_gpu, &cfg.language, prompt.as_deref(), preset, &path).await?
    }
    _ => {
        let (text, ow) = openai::transcribe_file(&cfg.api_key, &cfg.openai_model, Some(&cfg.language), prompt.as_deref(), &path).await?;
        let words: Vec<local_whisper::Word> = ow.into_iter().map(|w| local_whisper::Word { /* … */ }).collect();
        (text, words)
    }
};
```

After:

```rust
let provider_cfg = read_provider_config(&state)?;
let api_key = read_provider_api_key(&state, provider_cfg.provider_id())?;
let adapter = stt::build_adapter(&provider_cfg, &state);
let ctx = stt::TranscribeCtx {
    model: provider_cfg.model(),
    language: &cfg.language,
    initial_prompt: prompt.as_deref(),
    api_key: api_key.as_deref(),
    base_url: provider_cfg.base_url(),
};
let stt::TranscribeResult { text, words } = adapter.transcribe(ctx, &path).await?;
```

The `TranscribeWord` ↔ `local_whisper::Word` conversion currently inlined in the `_` arm gets pushed into the adapter — each adapter owns its own marshalling. Final `Vec<local_whisper::Word>` shape stays unchanged so downstream timeline serialization doesn't move.

Word type unification: rename `local_whisper::Word` → `stt::Word` and re-export from `local_whisper` as a deprecated alias. Pure mechanical, deferred to a follow-up commit so the trait introduction stays tight.

## Phasing

### Phase 1 — trait + OpenAI + Local (no behaviour change)

1. Create `src-tauri/src/stt/{mod,adapter,auth,config}.rs`. No public-API change yet; nothing imports it.
2. Move OpenAI transcription out of `openai.rs` into `stt/openai.rs` as `OpenAiAdapter`. Keep the existing `openai::transcribe_file` as a thin wrapper that delegates, marked `#[deprecated]`. Summary path untouched.
3. Wrap `local_whisper::transcribe_file_with_words` as `LocalWhisperAdapter`.
4. Switch `transcribe_chunk` to use `build_adapter`. Run the full test suite + a manual recording of each provider end-to-end.
5. Migrate settings on read: when `transcribe_config` is missing, synthesise it from legacy keys.

Phase 1 ships zero user-visible change. It's pure refactor; verifies the trait shape against the two providers we already have. Diff size estimate: ~500 LOC added, ~150 deleted.

**Verification gate**: a recording started before the migration must be transcribable post-migration with bit-identical output. (Easy to test by capturing a chunk WAV + golden transcript pre-migration and running it through the new path.)

### Phase 2 — Deepgram

1. Add `stt/deepgram.rs`. Multipart POST to `https://api.deepgram.com/v1/listen`, model defaults to `nova-3`. Auth: `Auth::Header { name: "Authorization", prefix: Some("Token ") }`. Returns word-level timestamps natively.
2. Add Deepgram entry to settings UI.
3. Add Keychain account `deepgram_api_key`.
4. Add Deepgram-specific test fixture (a 5s WAV → known transcript).

Diff size estimate: ~250 LOC added.

**User-visible change**: new option in settings dropdown. No regression risk for existing OpenAI/local users — the trait dispatch routes them away.

### Phase 3 — Groq

Groq hosts `whisper-large-v3-turbo` at OpenAI-compat `/v1/audio/transcriptions`. So the adapter is a 50-line subclass of OpenAI with a different base URL, label, and pricing/latency profile in the docs. Auth: same Bearer header.

Wait — that's a hint. If Groq is just OpenAI-compat with a different base URL, do we need a separate adapter? Yes, because:
- The settings UI should show Groq as a first-class choice with its own config (no "model" field — it's fixed to `whisper-large-v3-turbo`).
- Groq's rate limits and error messages are different; user-facing error mapping should mention Groq, not OpenAI.
- Future drift (Groq adding non-standard fields) is easier to absorb if it has its own module.

But the *implementation* can share code. `OpenAiCompatAdapter` as a private base struct with `provider_id`, `label`, default `base_url`, and rate-limit messages overridable per concrete adapter. Groq, OpenAI, and self-hosted Whisper.cpp servers all use this base.

Diff size estimate: ~150 LOC added.

### Phase 4 — open question: in-process axum server for local Whisper

Anarlog's biggest architectural win is `crates/local-stt-server/src/axum_server.rs:14-70` — they run whisper.cpp inside an in-process axum server bound to `127.0.0.1:0` and route to it via the same OpenAI-compat code path as cloud providers. `LocalWhisperAdapter` would then disappear; local becomes "OpenAI-compat against an internal endpoint."

Pros:
- One code path. Adapter registry shrinks. Reduces Humla's special-cased local path.
- Future: a power user can point any external tool at this localhost endpoint to use Humla's loaded Whisper model — incidental API surface for free.

Cons:
- Network hop, JSON serialization overhead per chunk. For local Whisper at ~5× realtime this is negligible (sub-1ms vs 200–500ms of inference).
- Adds an axum dependency to the Tauri app (~300 KB).
- Requires reshaping `local_whisper`'s API around HTTP request/response rather than direct calls. Word timestamps round-trip through verbose-JSON.
- TIght coupling to the OpenAI-compat schema; if that schema lacks a knob we want (DTW alignment heads?), we lose access to it.

**Decision**: defer. Phase 1–3 ship the trait abstraction and 2 new providers. Phase 4 is a separate decision after we've used the trait for a few months.

## Risks and open questions

- **Adapter trait + `async-trait` tax**: `async-trait` boxes futures. ~1µs overhead per call vs hundreds of ms per chunk transcription — irrelevant. Confirmed.
- **`TranscribeCtx` lifetime parameter**: caller must keep references alive across the await. In `transcribe_chunk` they're all locals owned by the function — fine. If we ever spawn the transcribe future to a detached task, this becomes a problem; revisit if/when that happens.
- **Per-note provider override**: should a user be able to pick "this note uses Deepgram" while their global default is local Whisper? Probably yes — Humla already has per-note language. But that's a settings-UI decision, not a trait decision. The trait supports it trivially: `read_provider_config` takes a `note_id` and falls back to global.
- **Vocabulary support varies**: Deepgram has `keywords`, Whisper/OpenAI has `prompt`. Right now Humla packs vocabulary into `initial_prompt`. Deepgram won't honour that — needs to accept the vocab list and stuff it into Deepgram's `keywords` query param. Solution: rename `initial_prompt` to `bias_terms: Option<&str>` (provider-neutral) and let each adapter wire it to its own slot. Defer the rename to Phase 2 when Deepgram lands.
- **Trail context per source**: orthogonal — the trait sees a single `initial_prompt` string. The per-source trail logic in `transcribe_chunk` stays exactly as-is, builds the string, passes it in.
- **Hallucination + repetition filters in `transcribe_chunk`**: stay where they are. The trait returns raw provider output; downstream filtering is provider-agnostic. (Long-term we may want a per-adapter `is_likely_hallucination` since each provider hallucinates differently — Deepgram doesn't produce "Subtitles by Amara" tails. Defer.)

## What I'd hold off on

- Realtime websocket adapters (anarlog's `RealtimeSttAdapter`). Humla's chunk-then-batch flow is intentional — it pairs with VAD-bounded chunking and the per-source trail prompt. Switching to streaming would unwind those.
- Provider-side diarization. Humla's offline FluidAudio path is a moat; routing diarization through Deepgram's speaker-id (cloud) would weaken the privacy story.
- A full plugin system. We're in single-binary territory; trait + match registry is enough.

## Concrete first PR scope

Phase 1 only. ~500 LOC added, ~150 deleted, no user-visible behaviour change, full test parity. After that lands and stews for a week, Phase 2 (Deepgram) lands as a single ~250 LOC PR.
