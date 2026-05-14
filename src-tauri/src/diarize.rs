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
// The sidecar uses FluidAudio's `OfflineDiarizerManager` (community-1
// segmentation + VBx clustering with PLDA) — the upgrade from the 3.1-based
// `DiarizerManager` we used initially. Picked because community-1 counts and
// assigns speakers more accurately on dense single-mic captures (e.g.
// in-person meetings where everyone shares one acoustic context). The
// sidecar handles: model download (~30 MB of CoreML files on first run,
// cached after), compile for the Apple Neural Engine, audio resample to
// 16 kHz mono Float32, and the actual diarization. It writes a single JSON
// array of segments to stdout and exits.

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct Segment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub speaker_id: String,
}

/// Maximum gap (ms) between two same-speaker segments to merge them
/// into one continuous turn. Sortformer in particular often slices a
/// single speaker's turn into multiple sub-segments separated by 100-
/// 200 ms of model-internal frame boundaries; merging recovers the
/// natural turn shape.
const SAME_SPEAKER_MERGE_GAP_MS: u64 = 250;

/// Maximum duration (ms) for a segment to be a candidate for "noise"
/// dropping. Anything longer is treated as a real turn regardless of
/// what overlaps it.
const NOISE_CANDIDATE_MAX_MS: u64 = 600;

/// Minimum fraction of a candidate segment's duration that must be
/// covered by a longer different-speaker segment for the candidate to
/// be dropped as noise.
const NOISE_OVERLAP_THRESHOLD: f64 = 0.80;

/// The containing different-speaker segment must be at least this many
/// times longer than the candidate. Avoids dropping a 500 ms segment
/// because a 600 ms different-speaker segment happens to overlap it.
const NOISE_CONTAINER_LENGTH_RATIO: u64 = 2;

/// Hard floor: any segment under this length is dropped unconditionally.
/// Below ~150 ms we're well under the duration of a real speech turn —
/// these are per-frame prediction blips.
const HARD_FLOOR_MS: u64 = 150;

/// Pre-processing pass over raw diarize output. Sortformer in particular
/// produces highly fragmented segments — for example a 60-minute
/// recording yielded 2072 segments with median duration 960 ms, 32%
/// under 500 ms, and frequent overlap between different-speaker
/// segments. Without cleaning, walking word-level alignment over this
/// segment set produces hyper-fragmented `LabelledPiece` sequences
/// that downstream flicker absorption (`bridge_short_interjections`)
/// cannot fully rescue.
///
/// Three passes, applied in order:
///   1. Merge adjacent same-speaker segments separated by ≤250 ms gap
///      or overlapping. Recovers continuous turns Sortformer's
///      per-frame output sliced into pieces.
///   2. Drop short segments (<600 ms) that are ≥80% contained inside
///      a longer (2× or more) different-speaker segment. These are
///      almost always per-frame prediction blips, not real backchannels.
///   3. Drop any remaining segment under 150 ms unconditionally.
///
/// Idempotent: re-running `clean_segments` on its own output is a no-op.
pub fn clean_segments(segments: Vec<Segment>) -> Vec<Segment> {
    let merged = merge_same_speaker(segments);
    let denoised = drop_contained_noise(merged);
    drop_subthreshold(denoised)
}

fn merge_same_speaker(segments: Vec<Segment>) -> Vec<Segment> {
    if segments.is_empty() {
        return segments;
    }
    // Group by speaker, then within each speaker walk in start order
    // and merge gap-adjacent or overlapping pairs.
    let mut by_speaker: std::collections::HashMap<String, Vec<Segment>> =
        std::collections::HashMap::new();
    for s in segments {
        by_speaker.entry(s.speaker_id.clone()).or_default().push(s);
    }
    let mut out: Vec<Segment> = Vec::new();
    for (_, mut segs) in by_speaker {
        segs.sort_by_key(|s| (s.start_ms, s.end_ms));
        let mut merged: Vec<Segment> = Vec::new();
        for s in segs.drain(..) {
            match merged.last_mut() {
                Some(last)
                    if s.start_ms.saturating_sub(last.end_ms) <= SAME_SPEAKER_MERGE_GAP_MS =>
                {
                    last.end_ms = last.end_ms.max(s.end_ms);
                }
                _ => merged.push(s),
            }
        }
        out.extend(merged);
    }
    out.sort_by_key(|s| (s.start_ms, s.end_ms));
    out
}

fn drop_contained_noise(segments: Vec<Segment>) -> Vec<Segment> {
    let n = segments.len();
    let mut keep = vec![true; n];
    for i in 0..n {
        let dur = segments[i].end_ms.saturating_sub(segments[i].start_ms);
        if dur >= NOISE_CANDIDATE_MAX_MS || dur == 0 {
            continue;
        }
        for j in 0..n {
            if i == j || segments[j].speaker_id == segments[i].speaker_id {
                continue;
            }
            let other_dur = segments[j].end_ms.saturating_sub(segments[j].start_ms);
            if other_dur < dur.saturating_mul(NOISE_CONTAINER_LENGTH_RATIO) {
                continue;
            }
            let overlap_start = segments[i].start_ms.max(segments[j].start_ms);
            let overlap_end = segments[i].end_ms.min(segments[j].end_ms);
            if overlap_end <= overlap_start {
                continue;
            }
            let overlap = overlap_end - overlap_start;
            if (overlap as f64 / dur as f64) >= NOISE_OVERLAP_THRESHOLD {
                keep[i] = false;
                break;
            }
        }
    }
    segments
        .into_iter()
        .zip(keep)
        .filter_map(|(s, k)| if k { Some(s) } else { None })
        .collect()
}

fn drop_subthreshold(segments: Vec<Segment>) -> Vec<Segment> {
    segments
        .into_iter()
        .filter(|s| s.end_ms.saturating_sub(s.start_ms) >= HARD_FLOOR_MS)
        .collect()
}

/// Run speaker diarization on a WAV file by invoking the speaker-diarize
/// sidecar. First call downloads the offline diarizer models (~30 MB of
/// CoreML files) and compiles them for the Apple Neural Engine — that's
/// slow (20–30 s). Subsequent calls reuse the cached + compiled models and
/// run substantially faster than realtime on M-series.
///
/// `num_speakers` is an optional caller-supplied hint. When provided, the
/// sidecar pins the cluster count via `OfflineDiarizerConfig.withSpeakers
/// (exactly:)`, which is the most reliable fix for dominant-speaker
/// recordings where VBx auto-detection collapses to one cluster. `None`
/// leaves auto-detection on.
/// User-tunable thresholds passed through to the sidecar. `None` for any
/// field means "use the sidecar's built-in default" — the values that
/// match what the project shipped before these became settings.
#[derive(Clone, Copy, Debug, Default)]
pub struct Thresholds {
    pub community1_clustering: Option<f64>,
    pub sortformer_silence: Option<f32>,
    pub sortformer_pred: Option<f32>,
}

/// Which diarization engine the sidecar should run.
///
/// `Community1` is FluidAudio's `OfflineDiarizerManager` (community-1
/// segmentation + VBx clustering with PLDA). Strong baseline, but
/// clustering-based approaches plateau on rapid within-channel speaker
/// turns — the architectural ceiling that drove the Sortformer addition.
///
/// `Sortformer` is NVIDIA's Streaming Sortformer (4-speaker end-to-end
/// transformer) running in batch via `SortformerDiarizer.processComplete`.
/// We use the `highContextV2_1` variant (chunkRightContext=40 frames,
/// ~4s of right-side lookahead) for offline accuracy, not the streaming
/// latency the default `fastV2_1` is tuned for. Trade-off: 4-speaker hard
/// cap (vs auto-detect on community-1), no num_speakers hint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    Community1,
    Sortformer,
}

impl Engine {
    pub fn from_setting(s: &str) -> Self {
        match s {
            "sortformer" => Engine::Sortformer,
            _ => Engine::Community1,
        }
    }

    fn arg(self) -> &'static str {
        match self {
            Engine::Community1 => "community1",
            Engine::Sortformer => "sortformer",
        }
    }
}

pub async fn diarize_file(
    app: &AppHandle,
    audio_path: &Path,
    num_speakers: Option<i64>,
    engine: Engine,
    thresholds: Thresholds,
) -> Result<Vec<Segment>> {
    let sidecar = sidecar_path(app)?;
    let path_str = audio_path
        .to_str()
        .ok_or_else(|| anyhow!("non-utf8 audio path"))?;

    let mut cmd = Command::new(&sidecar);
    cmd.arg(path_str);
    cmd.arg("--engine").arg(engine.arg());
    // Sortformer has a fixed 4-speaker output cap and ignores hints —
    // only forward the flag on the community-1 path.
    if engine == Engine::Community1 {
        if let Some(n) = num_speakers.filter(|n| *n > 0) {
            cmd.arg("--num-speakers").arg(n.to_string());
        }
        if let Some(t) = thresholds.community1_clustering {
            cmd.arg("--threshold").arg(format!("{t}"));
        }
    } else if engine == Engine::Sortformer {
        if let Some(t) = thresholds.sortformer_silence {
            cmd.arg("--silence-threshold").arg(format!("{t}"));
        }
        if let Some(t) = thresholds.sortformer_pred {
            cmd.arg("--pred-threshold").arg(format!("{t}"));
        }
    }
    let output = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| anyhow!("spawn speaker-diarize: {e}"))?;

    if !output.status.success() {
        // FluidAudio prints a wall of `[Profiling]` logs to stderr on every
        // invocation; without filtering, the entire dump would land in the
        // user's recording-error toast. The sidecar tags its own final error
        // line with `humla-error:` so we can pluck it out cleanly. Fall back
        // to the last non-empty line if no tag is present (older sidecars,
        // unexpected crashes).
        let stderr = String::from_utf8_lossy(&output.stderr);
        let clean = stderr
            .lines()
            .filter_map(|l| l.strip_prefix("humla-error: "))
            .last()
            .map(str::to_string)
            .or_else(|| {
                stderr
                    .lines()
                    .rev()
                    .map(str::trim)
                    .find(|l| !l.is_empty())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| format!("speaker-diarize exit {}", output.status));
        return Err(anyhow!("{clean}"));
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

/// One-shot purge of the old streaming diarization model files left behind
/// by pre-v0.8.0 installs. The community-1 (offline) pipeline lives in the
/// same FluidAudio directory but uses different filenames, so leftover
/// `pyannote_segmentation.mlmodelc` + `wespeaker_v2.mlmodelc` directories
/// stick around as ~14 MB of dead weight after upgrade.
///
/// Gated on a settings flag so it runs exactly once per install. Running on
/// every launch would be technically idempotent today (the names we wipe
/// are no longer produced by FluidAudio), but it would silently delete any
/// future upstream model file that happens to reuse those names — a hard-
/// to-debug failure mode. The flag pins the cleanup to "once, right after
/// the upgrade" and makes the function inert thereafter.
///
/// Resolves the FluidAudio dir from `app_data_dir().parent()` rather than
/// hardcoding `~/Library/...` so the function survives a future Tauri path
/// reshuffle. FluidAudio writes to `~/Library/Application Support/FluidAudio/`,
/// a sibling of our own `~/Library/Application Support/no.humla.app/`.
pub fn cleanup_legacy_streaming_models(app: &AppHandle, conn: &rusqlite::Connection) {
    const FLAG_KEY: &str = "legacy_streaming_models_purged_v1";
    match crate::db::get_setting(conn, FLAG_KEY) {
        Ok(Some(_)) => return, // already purged on a prior launch
        Ok(None) => {}
        Err(e) => {
            eprintln!("cleanup_legacy: read flag failed: {e}");
            // Don't proceed without a working DB — the flag write below
            // would also fail and we'd loop on every launch.
            return;
        }
    }

    let app_data = match app.path().app_data_dir() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("cleanup_legacy: no app_data_dir: {e}");
            return;
        }
    };
    let Some(application_support) = app_data.parent() else {
        eprintln!("cleanup_legacy: app_data_dir has no parent");
        return;
    };
    let fluid_dir = application_support
        .join("FluidAudio")
        .join("Models")
        .join("speaker-diarization");
    for legacy in ["pyannote_segmentation.mlmodelc", "wespeaker_v2.mlmodelc"] {
        let p = fluid_dir.join(legacy);
        if p.exists() {
            match std::fs::remove_dir_all(&p) {
                Ok(_) => eprintln!("cleanup_legacy: removed {}", p.display()),
                Err(e) => eprintln!("cleanup_legacy: remove {} failed: {e}", p.display()),
            }
        }
    }
    if let Err(e) = crate::db::set_setting(conn, FLAG_KEY, "1") {
        eprintln!("cleanup_legacy: write flag failed: {e}");
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
pub async fn status(app: &AppHandle, engine: Engine) -> Result<ModelStatus> {
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
        .arg("--engine")
        .arg(engine.arg())
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
    /// Which engine this progress event belongs to ("community1" or
    /// "sortformer"). Both engines share the diarize_download_progress
    /// event channel; the frontend filters by this field so simultaneous
    /// downloads don't cross-pollute each other's progress bars.
    pub engine: String,
}

/// Trigger the model download via the sidecar, emitting Tauri events for
/// each progress line so the UI can show a progress bar. The sidecar handles
/// FluidAudio's three-phase flow (listing → downloading → compiling).
pub async fn download(app: &AppHandle, engine: Engine) -> Result<()> {
    let sidecar = sidecar_path(app)?;
    let mut child = Command::new(&sidecar)
        .arg("download")
        .arg("--engine")
        .arg(engine.arg())
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
                    engine: engine.arg().to_string(),
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

pub async fn delete(app: &AppHandle, engine: Engine) -> Result<()> {
    let sidecar = sidecar_path(app)?;
    let output = Command::new(&sidecar)
        .arg("delete")
        .arg("--engine")
        .arg(engine.arg())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(start: u64, end: u64, spk: &str) -> Segment {
        Segment {
            start_ms: start,
            end_ms: end,
            speaker_id: spk.to_string(),
        }
    }

    #[test]
    fn empty_input_stays_empty() {
        assert!(clean_segments(vec![]).is_empty());
    }

    #[test]
    fn merges_same_speaker_within_gap() {
        let input = vec![
            seg(0, 1000, "S0"),
            seg(1100, 2000, "S0"), // gap 100ms → merge
            seg(3000, 4000, "S0"), // gap 1000ms → don't merge
        ];
        assert_eq!(
            clean_segments(input),
            vec![seg(0, 2000, "S0"), seg(3000, 4000, "S0")]
        );
    }

    #[test]
    fn merges_overlapping_same_speaker() {
        let input = vec![seg(0, 1000, "S0"), seg(500, 1500, "S0")];
        assert_eq!(clean_segments(input), vec![seg(0, 1500, "S0")]);
    }

    #[test]
    fn does_not_merge_across_speakers() {
        let input = vec![seg(0, 1000, "S0"), seg(1100, 2000, "S1")];
        assert_eq!(
            clean_segments(input),
            vec![seg(0, 1000, "S0"), seg(1100, 2000, "S1")]
        );
    }

    #[test]
    fn drops_contained_noise_sortformer_pattern() {
        // Classic Sortformer artifact: 81ms S1 sliver fully inside an
        // 800ms S0 segment. Drop the sliver.
        let input = vec![seg(480, 1280, "S0"), seg(799, 880, "S1")];
        assert_eq!(clean_segments(input), vec![seg(480, 1280, "S0")]);
    }

    #[test]
    fn keeps_short_segment_when_not_contained_by_other_speaker() {
        // S1 says something brief BETWEEN two S0 turns — no S0 segment
        // surrounds it, so it survives as a real turn.
        let input = vec![
            seg(0, 1000, "S0"),
            seg(2000, 2500, "S1"),
            seg(3500, 4500, "S0"),
        ];
        assert_eq!(clean_segments(input.clone()), input);
    }

    #[test]
    fn keeps_short_segment_when_container_isnt_twice_as_long() {
        // 400ms S1 vs 500ms S0 overlap — S0 is only 1.25x longer, not
        // 2x. Don't drop — could be a genuine short turn.
        let input = vec![seg(0, 500, "S0"), seg(100, 500, "S1")];
        // After merge_same_speaker (no-op, different speakers) and
        // drop_contained_noise (rejected by length ratio), both remain.
        let out = clean_segments(input);
        assert_eq!(out.len(), 2);
        assert!(out.contains(&seg(0, 500, "S0")));
        assert!(out.contains(&seg(100, 500, "S1")));
    }

    #[test]
    fn drops_subthreshold_segments() {
        let input = vec![
            seg(0, 100, "S0"),   // 100ms — under 150ms floor → drop
            seg(200, 500, "S1"), // 300ms — keep
        ];
        assert_eq!(clean_segments(input), vec![seg(200, 500, "S1")]);
    }

    #[test]
    fn idempotent() {
        let input = vec![
            seg(0, 1000, "S0"),
            seg(1100, 2000, "S0"),
            seg(2500, 3000, "S1"),
            seg(799, 880, "S1"), // noise inside S0[0..1000] after merge → drop
        ];
        let once = clean_segments(input);
        let twice = clean_segments(once.clone());
        assert_eq!(once, twice);
    }
}
