use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncBufReadExt, BufReader};
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
    // Echo to stderr for live debugging — visible in `pnpm tauri dev`'s
    // terminal. Cheap to leave on; segments are typically small.
    eprintln!(
        "diarize: {} segment(s): {:?}",
        segments.len(),
        segments
            .iter()
            .map(|s| format!("{}({}–{}ms)", s.speaker_id, s.start_ms, s.end_ms))
            .collect::<Vec<_>>()
            .join(" ")
    );
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelStatus {
    pub downloaded: bool,
    pub size_bytes: Option<u64>,
    pub path: Option<String>,
}

/// Ask the sidecar whether the FluidAudio model files are present on disk.
/// Returns Ok(downloaded=false) when the sidecar binary itself isn't
/// installed — that lets the rest of the app behave as "diarization not
/// available" rather than erroring out the user.
pub async fn status(app: &AppHandle) -> Result<ModelStatus> {
    let sidecar = match sidecar_path(app) {
        Ok(p) => p,
        Err(_) => {
            return Ok(ModelStatus {
                downloaded: false,
                size_bytes: None,
                path: None,
            });
        }
    };
    let output = Command::new(&sidecar)
        .arg("status")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| anyhow!("spawn speaker-diarize status: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("speaker-diarize status: {stderr}"));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(stdout.trim())
        .map_err(|e| anyhow!("parse status JSON: {e} -- {stdout}"))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProgress {
    pub fraction: f64,
    pub phase: String,
}

/// Trigger the model download via the sidecar, emitting Tauri events for
/// each progress line so the UI can show a progress bar. The sidecar handles
/// FluidAudio's three-phase flow (listing → downloading → compiling).
pub async fn download(app: &AppHandle) -> Result<()> {
    let sidecar = sidecar_path(app)?;
    let mut child = Command::new(&sidecar)
        .arg("download")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| anyhow!("spawn speaker-diarize download: {e}"))?;

    let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
    let stderr = child.stderr.take().ok_or_else(|| anyhow!("no stderr"))?;

    // Drain stderr concurrently so the pipe never blocks.
    let stderr_handle = tokio::spawn(async move {
        let mut reader = BufReader::new(stderr).lines();
        let mut buf = String::new();
        while let Ok(Some(line)) = reader.next_line().await {
            buf.push_str(&line);
            buf.push('\n');
        }
        buf
    });

    let mut reader = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = reader.next_line().await {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        match v.get("event").and_then(|e| e.as_str()) {
            Some("progress") => {
                let progress = DownloadProgress {
                    fraction: v.get("fraction").and_then(|f| f.as_f64()).unwrap_or(0.0),
                    phase: v
                        .get("phase")
                        .and_then(|p| p.as_str())
                        .unwrap_or("downloading")
                        .to_string(),
                };
                let _ = app.emit("diarize_download_progress", progress);
            }
            Some("done") => {
                // Final marker; loop will end when sidecar closes pipe.
            }
            _ => {}
        }
    }

    let exit = child.wait().await.map_err(|e| anyhow!("wait: {e}"))?;
    if !exit.success() {
        let stderr_text = stderr_handle.await.unwrap_or_default();
        return Err(anyhow!("download failed: {stderr_text}"));
    }
    Ok(())
}

pub async fn delete(app: &AppHandle) -> Result<()> {
    let sidecar = sidecar_path(app)?;
    let output = Command::new(&sidecar)
        .arg("delete")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| anyhow!("spawn speaker-diarize delete: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("speaker-diarize delete: {stderr}"));
    }
    Ok(())
}
