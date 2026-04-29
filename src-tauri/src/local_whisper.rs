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
    audio_path: &Path,
) -> Result<String> {
    let samples = wav::read_f32_mono_16k(audio_path).await?;
    let lang = if language == "auto" { None } else { Some(language.to_string()) };
    let prompt = initial_prompt.map(|s| s.to_string());

    // whisper-rs is sync and CPU/GPU-bound. Run on a blocking thread so we
    // don't stall the tokio reactor. Each call gets its own state; the
    // underlying model is shared.
    tokio::task::spawn_blocking(move || -> Result<String> {
        let ctx = ensure_loaded(&shared, &model_path)?;
        let mut state = ctx
            .create_state()
            .map_err(|e| anyhow!("create whisper state: {e}"))?;
        // Beam search recovers from confusion (proper nouns, dense Norwegian
        // news copy) far better than greedy at ~2× the cost. patience = -1.0
        // disables Whisper's beam-patience early exit; beam_size = 5 is the
        // sweet spot — beam_size = 8+ adds < 1% accuracy at 1.5× the latency.
        let mut params = FullParams::new(SamplingStrategy::BeamSearch {
            beam_size: 5,
            patience: -1.0,
        });
        params.set_print_progress(false);
        params.set_print_special(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_translate(false);
        params.set_temperature(0.0);
        // Default no_speech_thold (0.6) is aggressive — Whisper drops whole
        // borderline clauses (proper-noun-heavy Norwegian sentences are the
        // typical victim). 0.3 keeps low-confidence segments; the user can
        // edit a slightly-wrong transcript but can't recover dropped words.
        params.set_no_speech_thold(0.3);
        // Default logprob_thold (-1.0) is fine — keeps fallback decoding on
        // for confused segments, which complements the lowered no_speech.
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
        let mut out = String::new();
        for i in 0..n {
            let seg = state
                .full_get_segment_text(i)
                .map_err(|e| anyhow!("segment text: {e}"))?;
            if !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
            out.push_str(seg.trim());
        }
        Ok(out)
    })
    .await
    .map_err(|e| anyhow!("blocking task: {e}"))?
}
