//! Model lifecycle commands — local Whisper model files + speaker
//! diarization engine models. Download / status / delete, surfaced to
//! the frontend via `commands::*` (re-exported from the parent module).
//! `local_model_path` is also called by the recording / transcribe paths
//! that stay in the parent module, hence `pub(crate)`.

use super::err;
use crate::diarize;
use crate::local_whisper;
use crate::AppState;
use futures_util::StreamExt;
use std::path::PathBuf;
use tauri::{AppHandle, Emitter, Manager, State};

// ---- Speaker diarization model management ---------------------------------

fn parse_engine(engine: Option<String>) -> diarize::Engine {
    engine
        .as_deref()
        .map(diarize::Engine::from_setting)
        .unwrap_or(diarize::Engine::Community1)
}

#[tauri::command]
pub async fn diarize_status(
    app: AppHandle,
    engine: Option<String>,
) -> Result<diarize::ModelStatus, String> {
    diarize::status(&app, parse_engine(engine)).await.map_err(err)
}

#[tauri::command]
pub async fn diarize_download(app: AppHandle, engine: Option<String>) -> Result<(), String> {
    diarize::download(&app, parse_engine(engine)).await.map_err(err)
}

#[tauri::command]
pub async fn diarize_delete(app: AppHandle, engine: Option<String>) -> Result<(), String> {
    diarize::delete(&app, parse_engine(engine)).await.map_err(err)
}

// ---- Local Whisper model management ----------------------------------------

fn local_model_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(err)?.join("models");
    Ok(dir)
}

fn model_path_for(app: &AppHandle, info: &local_whisper::ModelInfo) -> Result<PathBuf, String> {
    Ok(local_model_dir(app)?.join(info.filename))
}

/// Resolve the model file path for a `model_id` from `LocalWhisperConfig`.
/// Caller is responsible for providing the right id — either the user's
/// default-provider model_id, or one from a per-language override. A
/// stale id (model removed from the registry) falls through to the
/// hardcoded default model.
///
/// Returns the resolved path even when the file doesn't exist on disk —
/// that's how the caller's "not downloaded" error surfaces with a real
/// path the user can recognise.
///
/// `_language` is unused after Phase 4 dropped the auto-route addon
/// mechanism; kept in the signature so callers don't have to thread it
/// out (and so a future language-aware behaviour can slot back in
/// without re-touching every call site).
pub(crate) fn local_model_path(
    app: &AppHandle,
    _language: &str,
    model_id: &str,
) -> Result<PathBuf, String> {
    let dir = local_model_dir(app)?;
    // Accept any kind — Multilingual is the user's general default; a
    // LanguageSpecific id is legitimate when it comes from a per-language
    // override's `model_id`. The fallback to default_model() handles
    // unknown ids (e.g. a stale config pointing at a removed model).
    let info =
        local_whisper::find_model(model_id).unwrap_or_else(local_whisper::default_model);
    let path = dir.join(info.filename);
    if path.exists() {
        return Ok(path);
    }
    Ok(dir.join(local_whisper::default_model().filename))
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalWhisperModelStatus {
    id: String,
    label: String,
    description: String,
    filename: String,
    size_bytes_hint: u64,
    /// "multilingual" — selectable as the default transcription model.
    /// "language_specific" — usable only as the model behind a per-
    /// language override in `transcribe_config.per_language`. Never the
    /// default.
    kind: &'static str,
    /// Set for `kind == "language_specific"`. The ISO 639-1 code this
    /// model is tuned for. None for multilingual models.
    specific_language: Option<String>,
    downloaded: bool,
    size_bytes: Option<u64>,
    path: Option<String>,
}

#[tauri::command]
pub fn local_whisper_models(app: AppHandle) -> Result<Vec<LocalWhisperModelStatus>, String> {
    let dir = local_model_dir(&app)?;
    let mut out = Vec::with_capacity(local_whisper::models().len());
    for info in local_whisper::models() {
        let path = dir.join(info.filename);
        let downloaded = path.exists();
        let size_bytes = if downloaded {
            std::fs::metadata(&path).ok().map(|m| m.len())
        } else {
            None
        };
        let (kind, specific_language) = match info.kind {
            local_whisper::ModelKind::Multilingual => ("multilingual", None),
            local_whisper::ModelKind::LanguageSpecific { language } => {
                ("language_specific", Some(language.to_string()))
            }
        };
        out.push(LocalWhisperModelStatus {
            id: info.id.to_string(),
            label: info.label.to_string(),
            description: info.description.to_string(),
            filename: info.filename.to_string(),
            size_bytes_hint: info.size_bytes_hint,
            kind,
            specific_language,
            downloaded,
            size_bytes,
            path: if downloaded { path.to_str().map(|s| s.to_string()) } else { None },
        });
    }
    Ok(out)
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DownloadProgress {
    model_id: String,
    received: u64,
    total: Option<u64>,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DownloadError {
    model_id: String,
    message: String,
}

#[tauri::command]
pub async fn local_whisper_download(app: AppHandle, model_id: String) -> Result<(), String> {
    let r = local_whisper_download_inner(&app, &model_id).await;
    // Failure must reach the UI as an event, not only as this command's Err:
    // the invoke promise that started the download may belong to a page that
    // has since unmounted, and progress events alone would leave any mounted
    // listener showing a forever-progress bar.
    if let Err(ref message) = r {
        let _ = app.emit("local_whisper_download_error", DownloadError {
            model_id: model_id.clone(),
            message: message.clone(),
        });
    }
    r
}

async fn local_whisper_download_inner(app: &AppHandle, model_id: &str) -> Result<(), String> {
    let info = local_whisper::find_model(model_id)
        .ok_or_else(|| format!("unknown model id: {model_id}"))?;
    let dir = local_model_dir(app)?;
    tokio::fs::create_dir_all(&dir).await.map_err(|e| format!("mkdir: {e}"))?;
    let final_path = dir.join(info.filename);
    // Download to a temp file in the same dir, then rename atomically so a
    // crash mid-download never leaves a half-written model in place.
    let tmp_path = dir.join(format!("{}.partial", info.filename));

    let resp = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60 * 30))
        .build()
        .map_err(|e| format!("client: {e}"))?
        .get(info.url)
        .send()
        .await
        .map_err(|e| format!("network: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("download {}: HTTP {}", info.url, resp.status()));
    }
    let total = resp.content_length();
    let _ = app.emit("local_whisper_progress", DownloadProgress {
        model_id: info.id.to_string(),
        received: 0,
        total,
    });

    use tokio::io::AsyncWriteExt;
    let mut file = tokio::fs::File::create(&tmp_path)
        .await
        .map_err(|e| format!("create tmp: {e}"))?;
    let mut received: u64 = 0;
    let mut last_emit = std::time::Instant::now();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|e| format!("stream: {e}"))?;
        file.write_all(&bytes).await.map_err(|e| format!("write: {e}"))?;
        received += bytes.len() as u64;
        // Throttle progress events to ~10/sec; UI doesn't need every chunk.
        if last_emit.elapsed() >= std::time::Duration::from_millis(100) {
            let _ = app.emit("local_whisper_progress", DownloadProgress {
                model_id: info.id.to_string(),
                received,
                total,
            });
            last_emit = std::time::Instant::now();
        }
    }
    file.flush().await.map_err(|e| format!("flush: {e}"))?;
    drop(file);
    tokio::fs::rename(&tmp_path, &final_path)
        .await
        .map_err(|e| format!("rename: {e}"))?;
    // Post-rename event: the file is fully in place, so `received` IS the
    // total. Stamping it in (even when the server sent no Content-Length and
    // `total` was None all along) is what lets listeners detect completion
    // as `received >= total` — the invoke promise that started the download
    // may belong to a page that has since unmounted.
    let _ = app.emit("local_whisper_progress", DownloadProgress {
        model_id: info.id.to_string(),
        received,
        total: total.or(Some(received)),
    });
    Ok(())
}

#[tauri::command]
pub async fn local_whisper_delete(
    app: AppHandle,
    state: State<'_, AppState>,
    model_id: String,
) -> Result<(), String> {
    let info = local_whisper::find_model(&model_id)
        .ok_or_else(|| format!("unknown model id: {model_id}"))?;
    let path = model_path_for(&app, info)?;
    // Drop the loaded model from RAM first when it's the one being deleted,
    // so we're not holding the file. SharedContext keys by path, so it's
    // safe to call unconditionally; worst case the next transcribe reloads
    // a model that didn't actually need to be evicted.
    local_whisper::unload(&state.whisper);
    if path.exists() {
        tokio::fs::remove_file(&path).await.map_err(|e| format!("remove: {e}"))?;
    }
    Ok(())
}
