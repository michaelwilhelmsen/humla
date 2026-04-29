use anyhow::{anyhow, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tauri::{AppHandle, Manager};
use tokio::process::Command;

// fluidaudio-rs v0.1.0 advertises diarization but the Rust bindings are
// stubs — only the underlying FluidAudio Swift package implements it. So
// we wrap that Swift package in a sidecar binary (`speaker-diarize`) and
// IPC over stdout JSON, mirroring our `audio-capture` sidecar pattern.
//
// The sidecar handles: model download (~500 MB on first run, cached after),
// CoreML compile for the Apple Neural Engine, audio resample to 16 kHz mono
// Float32, and the actual diarization. It writes a single JSON array of
// segments to stdout and exits.

#[derive(Debug, Clone, Deserialize)]
pub struct Segment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub speaker_id: String,
}

/// Run speaker diarization on a WAV file by invoking the speaker-diarize
/// sidecar. First call downloads the model (~500 MB) and compiles it for
/// the Apple Neural Engine — that's slow (20-30 s). Subsequent calls reuse
/// the cached model + compilation and run in roughly realtime/30 (i.e.
/// ~1 s per 30 min of audio on M-series).
pub async fn diarize_file(app: &AppHandle, audio_path: &Path) -> Result<Vec<Segment>> {
    let sidecar = sidecar_path(app)?;
    let path_str = audio_path
        .to_str()
        .ok_or_else(|| anyhow!("non-utf8 audio path"))?;

    let output = Command::new(&sidecar)
        .arg(path_str)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| anyhow!("spawn speaker-diarize: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("speaker-diarize exit {}: {}", output.status, stderr));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let segments: Vec<Segment> = serde_json::from_str(stdout.trim())
        .map_err(|e| anyhow!("parse segments JSON: {e} -- {stdout}"))?;
    Ok(segments)
}

/// Mirror of audio-capture sidecar resolution: bundle path in production,
/// `src-tauri/binaries/` in dev.
fn sidecar_path(app: &AppHandle) -> Result<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for name in [
                "speaker-diarize",
                "speaker-diarize-aarch64-apple-darwin",
                "speaker-diarize-x86_64-apple-darwin",
            ] {
                let p = dir.join(name);
                if p.exists() {
                    return Ok(p);
                }
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        for triple in ["aarch64-apple-darwin", "x86_64-apple-darwin"] {
            let p = cwd.join(format!("src-tauri/binaries/speaker-diarize-{triple}"));
            if p.exists() {
                return Ok(p);
            }
            let p = cwd.join(format!("binaries/speaker-diarize-{triple}"));
            if p.exists() {
                return Ok(p);
            }
        }
    }
    // Fallback so tests / dev builds without the sidecar produce a clear
    // error instead of a panic. The caller decides how to handle it
    // (currently: log + skip diarization, leaving the transcript untagged).
    let _ = app;
    Err(anyhow!("speaker-diarize sidecar not found"))
}

/// Best-effort cleanup of the full-recording WAV. Logs and continues on
/// failure — a stale temp file is much less bad than a panic in shutdown.
pub async fn cleanup_full_wav(path: &Path) {
    if let Err(e) = tokio::fs::remove_file(path).await {
        eprintln!("cleanup full.wav: {e}");
    }
}
