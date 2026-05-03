use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use parking_lot::Mutex;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::wav;

// Default model: large-v3-turbo Q5_0 (~547 MB). Excellent quality on Apple
// Silicon with Metal acceleration. Sourced from the canonical whisper.cpp
// HuggingFace repo.
pub const MODEL_FILE: &str = "ggml-large-v3-turbo-q5_0.bin";
pub const MODEL_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin";
// Approximate; the HF resolver doesn't always return Content-Length on the
// first hop. Used as a fallback for the UI progress bar.
pub const MODEL_BYTES_HINT: u64 = 574_041_600;

// A loaded WhisperContext is reusable across calls and is the bulk of the
// startup cost (~1-3s on Apple Silicon). We hold it in AppState so repeated
// chunk transcriptions don't reload the model.
pub type SharedContext = Arc<Mutex<Option<LoadedModel>>>;

pub struct LoadedModel {
    pub path: PathBuf,
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

fn ensure_loaded(shared: &SharedContext, model_path: &Path) -> Result<Arc<WhisperContext>> {
    let mut guard = shared.lock();
    if let Some(loaded) = guard.as_ref() {
        if loaded.path == model_path {
            return Ok(loaded.ctx.clone());
        }
    }
    if !model_path.exists() {
        return Err(anyhow!(
            "Local Whisper model not found at {}",
            model_path.display()
        ));
    }
    let ctx = WhisperContext::new_with_params(
        model_path.to_str().ok_or_else(|| anyhow!("non-utf8 model path"))?,
        WhisperContextParameters::default(),
    )
    .map_err(|e| anyhow!("load whisper model: {e}"))?;
    let arc = Arc::new(ctx);
    *guard = Some(LoadedModel { path: model_path.to_path_buf(), ctx: arc.clone() });
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
pub async fn prewarm(shared: SharedContext, model_path: PathBuf) -> Result<()> {
    tokio::task::spawn_blocking(move || -> Result<()> {
        ensure_loaded(&shared, &model_path)?;
        Ok(())
    })
    .await
    .map_err(|e| anyhow!("prewarm task: {e}"))?
}

pub async fn transcribe_file(
    shared: SharedContext,
    model_path: PathBuf,
    language: &str,
    initial_prompt: Option<&str>,
    preset: Preset,
    audio_path: &Path,
) -> Result<String> {
    let segs =
        transcribe_file_segments(shared, model_path, language, initial_prompt, preset, audio_path)
            .await?;
    let mut out = String::new();
    for seg in segs {
        if !out.is_empty() && !out.ends_with(' ') {
            out.push(' ');
        }
        out.push_str(seg.text.trim());
    }
    Ok(out)
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
}

pub async fn transcribe_file_segments(
    shared: SharedContext,
    model_path: PathBuf,
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
        let ctx = ensure_loaded(&shared, &model_path)?;
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
            out.push(TextSegment {
                text: trimmed,
                start_ms: t0.saturating_mul(10),
                end_ms: t1.saturating_mul(10),
            });
        }
        Ok(out)
    })
    .await
    .map_err(|e| anyhow!("blocking task: {e}"))?
}
