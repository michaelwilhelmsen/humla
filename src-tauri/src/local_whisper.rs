use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use parking_lot::Mutex;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::wav;

// Catalog of GGML Whisper models the app can download. The first Primary
// entry is the default — picked at first-run, used as a fallback when the
// user's selected model isn't downloaded. Sizes are approximate (HF doesn't
// always return Content-Length on the first hop) and used as the progress
// bar's pre-stream estimate.
//
// `kind` separates two roles:
//   - Primary: the user's general-purpose model. Selectable as the active
//     transcription model; one of these is always used unless an addon
//     overrides it.
//   - LanguageAddon { language }: a specialised model that automatically
//     takes over for recordings in that language, but is never the active
//     primary. Downloading it is the opt-in. NB Whisper Large is finetuned
//     by Nasjonalbiblioteket on Norwegian and produces noticeably worse
//     output on other languages, so we don't let users pick it for an
//     English meeting — it kicks in only when the recording's language is
//     "no" and is otherwise dormant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelKind {
    Primary,
    LanguageAddon { language: &'static str },
}

#[derive(Clone, Copy, Debug)]
pub struct ModelInfo {
    pub id: &'static str,
    pub label: &'static str,
    pub filename: &'static str,
    pub url: &'static str,
    pub size_bytes_hint: u64,
    pub description: &'static str,
    pub kind: ModelKind,
}

pub const DEFAULT_MODEL_ID: &str = "large-v3-turbo-q5";

pub fn models() -> &'static [ModelInfo] {
    &MODELS
}

pub fn find_model(id: &str) -> Option<&'static ModelInfo> {
    MODELS.iter().find(|m| m.id == id)
}

pub fn default_model() -> &'static ModelInfo {
    find_model(DEFAULT_MODEL_ID).expect("default model id must be in registry")
}

const MODELS: &[ModelInfo] = &[
    ModelInfo {
        id: "large-v3-turbo-q5",
        label: "Large v3 Turbo (quantized)",
        filename: "ggml-large-v3-turbo-q5_0.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin",
        size_bytes_hint: 601_620_480,
        description: "Multilingual. ~10× realtime on Apple Silicon. The recommended default for almost all use.",
        kind: ModelKind::Primary,
    },
    ModelInfo {
        id: "large-v3-q5",
        label: "Large v3 (quantized)",
        filename: "ggml-large-v3-q5_0.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-q5_0.bin",
        size_bytes_hint: 1_159_868_416,
        description: "Multilingual. The non-turbo Large v3 — slower than Turbo but the highest-accuracy baseline on dense or noisy audio.",
        kind: ModelKind::Primary,
    },
    ModelInfo {
        id: "large-v2-q5",
        label: "Large v2 (quantized)",
        filename: "ggml-large-v2-q5_0.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v2-q5_0.bin",
        size_bytes_hint: 1_159_868_416,
        description: "Multilingual. The previous Large generation — sometimes preferable to v3 on accented speech where v3 introduced regressions.",
        kind: ModelKind::Primary,
    },
    ModelInfo {
        id: "medium-q5",
        label: "Medium (quantized)",
        filename: "ggml-medium-q5_0.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-medium-q5_0.bin",
        size_bytes_hint: 565_182_464,
        description: "Multilingual. Smaller and faster than Large on slower hardware; lower accuracy on dense or technical speech.",
        kind: ModelKind::Primary,
    },
    ModelInfo {
        id: "nb-whisper-large-q5",
        label: "NB Whisper Large (Norwegian add-on)",
        filename: "nb-whisper-large-q5_0.bin",
        url: "https://huggingface.co/NbAiLab/nb-whisper-large/resolve/main/ggml-model-q5_0.bin",
        size_bytes_hint: 1_159_237_632,
        description: "Norwegian-finetuned by Nasjonalbiblioteket. Auto-used for Norwegian recordings when downloaded; English/other-language meetings keep using your active primary model.",
        kind: ModelKind::LanguageAddon { language: "no" },
    },
];

/// Look up the language-addon model that matches a recording's language.
/// Returns None for "auto" (we don't know the language pre-decode) or
/// when no addon claims this language. Caller still has to check whether
/// the addon is actually downloaded before using its filename.
pub fn addon_for_language(language: &str) -> Option<&'static ModelInfo> {
    if language == "auto" {
        return None;
    }
    MODELS.iter().find(|m| match m.kind {
        ModelKind::LanguageAddon { language: addon_lang } => addon_lang == language,
        _ => false,
    })
}

// A loaded WhisperContext is reusable across calls and is the bulk of the
// startup cost (~1-3s on Apple Silicon). We hold it in AppState so repeated
// chunk transcriptions don't reload the model.
pub type SharedContext = Arc<Mutex<Option<LoadedModel>>>;

pub struct LoadedModel {
    pub path: PathBuf,
    pub use_gpu: bool,
    pub ctx: Arc<WhisperContext>,
}

pub fn new_shared() -> SharedContext {
    Arc::new(Mutex::new(None))
}

/// Trades latency for accuracy. The three presets bundle the underlying
/// whisper parameters that move together: sampling strategy and the
/// no_speech threshold that decides whether borderline segments survive.
#[derive(Clone, Copy)]
pub enum Preset {
    /// Greedy decoding, default no_speech threshold. ~5× realtime on
    /// Apple Silicon. Snappiest, but borderline clauses may be dropped.
    Fast,
    /// Beam search (size 3) with a moderate no_speech threshold. ~3×
    /// realtime. The middle ground.
    Balanced,
    /// Beam search (size 5) with an aggressive no_speech threshold so
    /// almost no segments are silently dropped. ~2× realtime. Best for
    /// meetings, news copy, and dense Norwegian.
    Quality,
}

impl Preset {
    pub fn from_setting(s: &str) -> Self {
        match s {
            "fast" => Preset::Fast,
            "balanced" => Preset::Balanced,
            // Default to Quality on unknown values so a corrupted setting
            // can't accidentally regress users to the old greedy path.
            _ => Preset::Quality,
        }
    }

    fn sampling(self) -> SamplingStrategy {
        match self {
            Preset::Fast => SamplingStrategy::Greedy { best_of: 1 },
            Preset::Balanced => SamplingStrategy::BeamSearch {
                beam_size: 3,
                patience: -1.0,
            },
            Preset::Quality => SamplingStrategy::BeamSearch {
                beam_size: 5,
                patience: -1.0,
            },
        }
    }

    fn no_speech_thold(self) -> f32 {
        match self {
            // Whisper's stock default — drops anything it isn't confident is
            // speech. Pairs naturally with greedy decoding.
            Preset::Fast => 0.6,
            // Loosened so beam search has more candidates to choose from.
            Preset::Balanced => 0.4,
            // Aggressive: keeps almost everything; relies on the wider beam
            // to pick the best hypothesis among low-confidence segments.
            Preset::Quality => 0.3,
        }
    }
}

fn ensure_loaded(
    shared: &SharedContext,
    model_path: &Path,
    use_gpu: bool,
) -> Result<Arc<WhisperContext>> {
    let mut guard = shared.lock();
    if let Some(loaded) = guard.as_ref() {
        if loaded.path == model_path && loaded.use_gpu == use_gpu {
            return Ok(loaded.ctx.clone());
        }
    }
    if !model_path.exists() {
        return Err(anyhow!(
            "Local Whisper model not found at {}",
            model_path.display()
        ));
    }
    // Drop the previously loaded model BEFORE allocating the new one.
    // Metal contexts share unified memory and a freshly constructed
    // WhisperContext briefly coexists with the old one if we don't
    // explicitly clear the slot first — enough on memory-tight machines
    // to push Metal into "failed to allocate context" territory.
    *guard = None;
    let mut params = WhisperContextParameters::default();
    params.use_gpu = use_gpu;
    let ctx = WhisperContext::new_with_params(
        model_path.to_str().ok_or_else(|| anyhow!("non-utf8 model path"))?,
        params,
    )
    .map_err(|e| anyhow!("load whisper model: {e}"))?;
    let arc = Arc::new(ctx);
    *guard = Some(LoadedModel {
        path: model_path.to_path_buf(),
        use_gpu,
        ctx: arc.clone(),
    });
    Ok(arc)
}

pub fn unload(shared: &SharedContext) {
    *shared.lock() = None;
}

/// Load the model into memory + Metal context if it isn't already. Cheap
/// no-op on subsequent calls. Called from `recording_start` so the first
/// chunk doesn't pay the 1–2 second cold-start tax — by the time VAD
/// rotates the first chunk (≥ 1 s of speech + 500 ms silence), the model
/// is ready and inference runs at ~5× realtime.
pub async fn prewarm(shared: SharedContext, model_path: PathBuf, use_gpu: bool) -> Result<()> {
    tokio::task::spawn_blocking(move || -> Result<()> {
        ensure_loaded(&shared, &model_path, use_gpu)?;
        Ok(())
    })
    .await
    .map_err(|e| anyhow!("prewarm task: {e}"))?
}

pub async fn transcribe_file(
    shared: SharedContext,
    model_path: PathBuf,
    use_gpu: bool,
    language: &str,
    initial_prompt: Option<&str>,
    preset: Preset,
    audio_path: &Path,
) -> Result<String> {
    let (text, _words) = transcribe_file_with_words(
        shared,
        model_path,
        use_gpu,
        language,
        initial_prompt,
        preset,
        audio_path,
    )
    .await?;
    Ok(text)
}

/// Variant that returns the joined transcript text *and* a flat list
/// of word-level timestamps across the whole file. Used by the live
/// chunk path so each chunk's words can be persisted into
/// timeline.jsonl for the playback view's karaoke-style highlight.
pub async fn transcribe_file_with_words(
    shared: SharedContext,
    model_path: PathBuf,
    use_gpu: bool,
    language: &str,
    initial_prompt: Option<&str>,
    preset: Preset,
    audio_path: &Path,
) -> Result<(String, Vec<Word>)> {
    let segs = transcribe_file_segments(
        shared,
        model_path,
        use_gpu,
        language,
        initial_prompt,
        preset,
        audio_path,
    )
    .await?;
    let mut text = String::new();
    let mut words = Vec::new();
    for seg in segs {
        if !text.is_empty() && !text.ends_with(' ') {
            text.push(' ');
        }
        text.push_str(seg.text.trim());
        words.extend(seg.words);
    }
    Ok((text, words))
}

/// One whisper-emitted text segment with its time bounds in milliseconds
/// relative to the input WAV. Returned by `transcribe_file_segments`; used
/// by the post-stop final-pass path to align text against the offline
/// diarizer's speaker segments.
#[derive(Clone, Debug)]
pub struct TextSegment {
    pub text: String,
    pub start_ms: u64,
    pub end_ms: u64,
    pub words: Vec<Word>,
}

/// One word's display text + millisecond bounds, derived from whisper's
/// token-level timestamps. Tokens are subword pieces (BPE), so consecutive
/// tokens are grouped into a single word whenever the next token starts
/// with a leading space (whisper.cpp's word-boundary convention). Used by
/// the playback view's karaoke-style highlighting.
#[derive(Clone, Debug)]
pub struct Word {
    pub text: String,
    pub start_ms: u64,
    pub end_ms: u64,
}

pub async fn transcribe_file_segments(
    shared: SharedContext,
    model_path: PathBuf,
    use_gpu: bool,
    language: &str,
    initial_prompt: Option<&str>,
    preset: Preset,
    audio_path: &Path,
) -> Result<Vec<TextSegment>> {
    let samples = wav::read_f32_mono_16k(audio_path).await?;
    let lang = if language == "auto" { None } else { Some(language.to_string()) };
    let prompt = initial_prompt.map(|s| s.to_string());

    // whisper-rs is sync and CPU/GPU-bound. Run on a blocking thread so we
    // don't stall the tokio reactor. Each call gets its own state; the
    // underlying model is shared.
    tokio::task::spawn_blocking(move || -> Result<Vec<TextSegment>> {
        let ctx = ensure_loaded(&shared, &model_path, use_gpu)?;
        let mut state = ctx
            .create_state()
            .map_err(|e| anyhow!("create whisper state: {e}"))?;
        let mut params = FullParams::new(preset.sampling());
        params.set_print_progress(false);
        params.set_print_special(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_translate(false);
        params.set_temperature(0.0);
        params.set_no_speech_thold(preset.no_speech_thold());
        params.set_logprob_thold(-1.0);
        // Token-level timestamps drive the playback view's word-by-word
        // highlight. Cheap on Apple Silicon (~1% extra) since the model
        // already produces token logits; this just exposes them.
        params.set_token_timestamps(true);
        if let Some(l) = lang.as_deref() {
            params.set_language(Some(l));
        }
        if let Some(p) = prompt.as_deref() {
            params.set_initial_prompt(p);
        }

        state
            .full(params, &samples)
            .map_err(|e| anyhow!("whisper full: {e}"))?;

        let n = state
            .full_n_segments()
            .map_err(|e| anyhow!("n_segments: {e}"))?;
        let mut out = Vec::with_capacity(n as usize);
        for i in 0..n {
            let text = state
                .full_get_segment_text(i)
                .map_err(|e| anyhow!("segment text: {e}"))?;
            // whisper.cpp returns t0/t1 in centiseconds (10ms units).
            let t0 = state
                .full_get_segment_t0(i)
                .map_err(|e| anyhow!("segment t0: {e}"))? as u64;
            let t1 = state
                .full_get_segment_t1(i)
                .map_err(|e| anyhow!("segment t1: {e}"))? as u64;
            let trimmed = text.trim().to_string();
            if trimmed.is_empty() {
                continue;
            }
            // Walk this segment's tokens to build word-level timing.
            // BPE tokens are subword pieces — group them by leading-
            // space convention (whisper.cpp tokens that begin a word
            // have a leading space; continuation tokens don't). Skip
            // tokens whose text starts with "[" or "<|" — those are
            // whisper specials (timestamps, language tags, etc.).
            let words = extract_words_for_segment(&state, i)?;
            out.push(TextSegment {
                text: trimmed,
                start_ms: t0.saturating_mul(10),
                end_ms: t1.saturating_mul(10),
                words,
            });
        }
        Ok(out)
    })
    .await
    .map_err(|e| anyhow!("blocking task: {e}"))?
}

/// Pull token-level data for one whisper segment and group BPE tokens
/// into words by the leading-space convention. Token timestamps are in
/// centiseconds (×10 → ms). Special tokens (`[_BEG_]`, `<|...|>`, etc.)
/// are skipped — they don't correspond to spoken text.
fn extract_words_for_segment(
    state: &whisper_rs::WhisperState,
    seg_i: i32,
) -> Result<Vec<Word>> {
    let n_tokens = state
        .full_n_tokens(seg_i)
        .map_err(|e| anyhow!("n_tokens: {e}"))?;
    let mut words: Vec<Word> = Vec::new();
    for tok_i in 0..n_tokens {
        let raw = state
            .full_get_token_text(seg_i, tok_i)
            .map_err(|e| anyhow!("token text: {e}"))?;
        // Filter out whisper specials. Empty / whitespace-only fragments
        // aren't useful as standalone words but can be valid
        // continuations within a multi-token word — handled below.
        if raw.starts_with("[_") || raw.starts_with("<|") {
            continue;
        }
        let data = state
            .full_get_token_data(seg_i, tok_i)
            .map_err(|e| anyhow!("token data: {e}"))?;
        let t0 = (data.t0 as u64).saturating_mul(10);
        let t1 = (data.t1 as u64).saturating_mul(10);
        let starts_word = raw.starts_with(' ') || words.is_empty();
        if starts_word {
            let trimmed = raw.trim_start().to_string();
            if trimmed.is_empty() {
                continue;
            }
            words.push(Word {
                text: trimmed,
                start_ms: t0,
                end_ms: t1,
            });
        } else if let Some(last) = words.last_mut() {
            last.text.push_str(&raw);
            // Extend the word's end time with the continuation token's
            // upper bound. start stays as the first token's t0.
            if t1 > last.end_ms {
                last.end_ms = t1;
            }
        }
    }
    // Trim residual whitespace + drop empties; punctuation tokens
    // ("," ".") get glued onto the previous word above so they don't
    // surface as their own entry, which is what we want for clicking.
    words.retain(|w| !w.text.trim().is_empty());
    Ok(words)
}
