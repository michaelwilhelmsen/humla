use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::LazyLock;
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

// Diarization runs in the `speaker-diarize` sidecar, a wrapper around the
// FluidAudio Swift package, which downloads and compiles the CoreML models,
// diarizes a WAV and prints its segments as one JSON array on stdout.

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct Segment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub speaker_id: String,
}

/// Maximum gap (ms) between two same-speaker segments to merge them into one
/// turn. An end-to-end engine slices a single turn at its own frame boundaries,
/// 100–200 ms apart.
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

/// Pre-processing pass over raw diarize output. An end-to-end engine emits
/// short, overlapping fragments, and word-level alignment over them produces
/// more speaker changes than `bridge_short_interjections` can absorb.
///
/// Three passes, applied in order:
///   1. Merge adjacent same-speaker segments separated by ≤250 ms gap
///      or overlapping.
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

/// User-tunable thresholds passed through to the sidecar. `None` means the
/// sidecar's own default.
#[derive(Clone, Copy, Debug, Default)]
pub struct Thresholds {
    pub community1_clustering: Option<f64>,
}

/// Which diarization engine the sidecar runs.
///
/// `Nemotron3` is NVIDIA's end-to-end Nemotron 3 Diarization: it counts
/// speakers itself, up to [`NEMOTRON_MAX_SPEAKERS`], and takes no count.
/// `Community1` is pyannote segmentation plus VBx clustering: it takes the
/// note's speaker count, and without one it tends to merge a meeting one person
/// dominates onto a single speaker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    Community1,
    Nemotron3,
}

/// The most speakers Nemotron 3 can tell apart.
pub const NEMOTRON_MAX_SPEAKERS: i64 = 8;

impl Engine {
    pub fn parse(arg: &str) -> Option<Self> {
        match arg {
            "community1" => Some(Engine::Community1),
            "nemotron3" => Some(Engine::Nemotron3),
            _ => None,
        }
    }

    /// A stored value this build doesn't know reads as community-1. Mirrored by
    /// `selectedDiarizeEngine` in `src/lib/diarizeEngine.ts`.
    pub fn from_setting(s: &str) -> Self {
        Self::parse(s).unwrap_or(Engine::Community1)
    }

    pub(crate) fn arg(self) -> &'static str {
        match self {
            Engine::Community1 => "community1",
            Engine::Nemotron3 => "nemotron3",
        }
    }

    pub(crate) fn takes_speaker_hint(self) -> bool {
        self == Engine::Community1
    }

    fn other(self) -> Self {
        match self {
            Engine::Community1 => Engine::Nemotron3,
            Engine::Nemotron3 => Engine::Community1,
        }
    }
}

/// The engines a note may be diarized with, best first: a count above what
/// Nemotron 3 can represent prefers community-1, anything else the selected
/// engine, and the other engine follows as the fallback for a missing model,
/// since labels from either beat none. Mirrored by `enginesToDownload` in
/// `src/lib/diarizeEngine.ts`, which fetches what this falls back to.
pub fn engine_preference(selected: Engine, expected_speakers: Option<i64>) -> Vec<Engine> {
    let preferred = if expected_speakers.is_some_and(|n| n > NEMOTRON_MAX_SPEAKERS) {
        Engine::Community1
    } else {
        selected
    };
    vec![preferred, preferred.other()]
}

/// The engine a note is diarized with, and whether its model is on disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EngineChoice {
    pub engine: Engine,
    pub downloaded: bool,
}

/// The first engine in [`engine_preference`] whose model is downloaded. When
/// none is, the note's preferred engine with `downloaded: false`.
pub async fn choose_engine(
    app: &AppHandle,
    selected: Engine,
    expected_speakers: Option<i64>,
) -> EngineChoice {
    let preference = engine_preference(selected, expected_speakers);
    for &engine in &preference {
        if matches!(status(app, engine).await, Ok(s) if s.downloaded) {
            if engine != preference[0] {
                eprintln!(
                    "diarize: {} model not downloaded, using {}",
                    preference[0].arg(),
                    engine.arg()
                );
            }
            return EngineChoice { engine, downloaded: true };
        }
    }
    EngineChoice { engine: preference[0], downloaded: false }
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
    if engine.takes_speaker_hint() {
        if let Some(n) = num_speakers.filter(|n| *n > 0) {
            cmd.arg("--num-speakers").arg(n.to_string());
        }
        if let Some(t) = thresholds.community1_clustering {
            cmd.arg("--threshold").arg(format!("{t}"));
        }
    }
    let output = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| anyhow!("spawn speaker-diarize: {e}"))?;

    if !output.status.success() {
        let clean = sidecar_error(&String::from_utf8_lossy(&output.stderr), output.status);
        return Err(anyhow!("{clean}"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let segments = parse_segment_payload(&stdout)
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

/// The sidecar's own error line out of its stderr, which FluidAudio fills with
/// logging, so the sidecar tags that line `humla-error:`. An untagged failure
/// (a crash) gives its last line and how the process exited.
fn sidecar_error(stderr: &str, exit: impl std::fmt::Display) -> String {
    const TAG: &str = "humla-error:";
    let tagged = stderr
        .lines()
        .filter_map(|l| l.strip_prefix(TAG))
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .last();
    if let Some(message) = tagged {
        return message.to_string();
    }
    let last = stderr
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with(TAG));
    match last {
        Some(line) => format!("{line} (speaker-diarize {exit})"),
        None => format!("speaker-diarize {exit}"),
    }
}

/// Extract the segment array from the sidecar's stdout.
///
/// We can't assume stdout is exactly one JSON value. The sidecar writes the
/// payload as a single compact JSON line through `FileHandle.standardOutput`
/// (an unbuffered write straight to fd 1), but the vendored FluidAudio package
/// can emit unrelated text to stdout around it. The subtle part: Swift's
/// `print()` is stdio-buffered, so a stray `print()` elsewhere in FluidAudio
/// flushes only at process exit — *after* our unbuffered payload write — and
/// lands as a trailing line. `serde_json::from_str` over the whole buffer then
/// fails with "trailing characters at line 2 column 1" and diarization dies,
/// surfacing the raw parse error where the transcript should be.
///
/// So scan for the line that actually parses as a segment array rather than
/// trusting the whole buffer. The payload is always a single line
/// (JSONSerialization emits compact, newline-free JSON), so the real array
/// never spans lines and stray log lines never parse as `Vec<Segment>`.
/// Mirrors `sidecar_error`'s filtering of stderr. Falls back to the
/// whole trimmed buffer so a genuinely malformed payload still surfaces
/// serde's diagnostic to the caller.
fn parse_segment_payload(stdout: &str) -> serde_json::Result<Vec<Segment>> {
    for line in stdout.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            if let Ok(segments) = serde_json::from_str::<Vec<Segment>>(line) {
                return Ok(segments);
            }
        }
    }
    serde_json::from_str::<Vec<Segment>>(stdout.trim())
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

/// Model directories under FluidAudio's `Models` root that Humla no longer
/// uses — the old streaming diarizer's files and Sortformer's — each with the
/// flag that removes it once per install.
const RETIRED_MODEL_DIRS: &[(&str, &[&str])] = &[
    (
        "legacy_streaming_models_purged_v1",
        &[
            "speaker-diarization/pyannote_segmentation.mlmodelc",
            "speaker-diarization/wespeaker_v2.mlmodelc",
        ],
    ),
    ("sortformer_models_purged_v1", &["sortformer"]),
];

/// FluidAudio's shared model cache, a sibling of Humla's own app-data directory.
fn fluidaudio_models_root(app: &AppHandle) -> Option<PathBuf> {
    let app_data = match app.path().app_data_dir() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("FluidAudio models: no app_data_dir: {e}");
            return None;
        }
    };
    let Some(application_support) = app_data.parent() else {
        eprintln!("FluidAudio models: app_data_dir has no parent");
        return None;
    };
    Some(application_support.join("FluidAudio").join("Models"))
}

/// Remove the retired model directories left in FluidAudio's shared model cache.
pub fn remove_retired_models(app: &AppHandle, conn: &rusqlite::Connection) {
    if let Some(models) = fluidaudio_models_root(app) {
        remove_retired_model_dirs(&models, conn);
    }
}

/// Nemotron 3's bundle from before FluidAudio re-exported it into
/// `monolithic/v2`. Nothing loads it now.
const OLD_NEMOTRON_BUNDLE: &str = "nemotron-3-diarization/monolithic/Nemotron3Diarizer_fast128.mlmodelc";

#[derive(Debug, PartialEq, Eq)]
enum OldBundle {
    Absent,
    Remove,
    Replace,
}

/// An install on Nemotron 3 diarizes with community-1 until the current bundle
/// is ready, so it gets that bundle before the old one goes; any other install
/// only loses the old one.
fn old_bundle_plan(present: bool, selected: Engine) -> OldBundle {
    match (present, selected) {
        (false, _) => OldBundle::Absent,
        (true, Engine::Nemotron3) => OldBundle::Replace,
        (true, Engine::Community1) => OldBundle::Remove,
    }
}

/// Readies Nemotron 3 in the background at launch, so neither a download nor a
/// Neural Engine compile lands in a post-stop chain. An install that fetched the
/// old bundle is moved onto the current one, and the old bundle removed only
/// once that succeeds; otherwise model files warmed up under another macOS
/// build or sidecar are warmed up again. A failed attempt is made again at the
/// next launch, and until one succeeds `choose_engine` uses community-1.
pub fn prepare_nemotron_at_launch(app: &AppHandle, selected: Engine) {
    let Some(models) = fluidaudio_models_root(app) else { return };
    let old = models.join(OLD_NEMOTRON_BUNDLE);
    let app = app.clone();
    match old_bundle_plan(old.exists(), selected) {
        OldBundle::Replace => {
            tauri::async_runtime::spawn(async move {
                match download(&app, Engine::Nemotron3).await {
                    Ok(()) => remove_old_bundle(&old),
                    Err(e) => eprintln!("nemotron bundle: preparing the current one failed: {e}"),
                }
            });
        }
        plan => {
            if plan == OldBundle::Remove {
                remove_old_bundle(&old);
            }
            tauri::async_runtime::spawn(async move { rewarm_if_stale(&app).await });
        }
    }
}

/// A `download` of files already on disk fetches nothing and runs the warm-up.
async fn rewarm_if_stale(app: &AppHandle) {
    match status(app, Engine::Nemotron3).await {
        Ok(s) if s.needs_warm_up => {
            eprintln!("nemotron: warming up again for this macOS build and sidecar");
            if let Err(e) = download(app, Engine::Nemotron3).await {
                eprintln!("nemotron: warm-up failed: {e}");
            }
        }
        Ok(_) => {}
        Err(e) => eprintln!("nemotron: status failed: {e}"),
    }
}

fn remove_old_bundle(path: &Path) {
    match std::fs::remove_dir_all(path) {
        Ok(_) => eprintln!("nemotron bundle: removed {}", path.display()),
        Err(e) => eprintln!("nemotron bundle: remove {} failed: {e}", path.display()),
    }
}

/// Once per flag rather than every launch: the folders sit in a cache other
/// FluidAudio clients share, and one of them may use a folder after we left it.
fn remove_retired_model_dirs(models: &Path, conn: &rusqlite::Connection) {
    for (flag, dirs) in RETIRED_MODEL_DIRS {
        match crate::db::get_setting(conn, flag) {
            Ok(Some(_)) => continue,
            Ok(None) => {}
            Err(e) => {
                // Without a readable flag the write below would fail too, and
                // the removal would repeat on every launch.
                eprintln!("retired models: read {flag} failed: {e}");
                continue;
            }
        }
        for dir in *dirs {
            let p = models.join(dir);
            if p.exists() {
                match std::fs::remove_dir_all(&p) {
                    Ok(_) => eprintln!("retired models: removed {}", p.display()),
                    Err(e) => eprintln!("retired models: remove {} failed: {e}", p.display()),
                }
            }
        }
        if let Err(e) = crate::db::set_setting(conn, flag, "1") {
            eprintln!("retired models: write {flag} failed: {e}");
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelStatus {
    pub downloaded: bool,
    /// The files are on disk but their warm-up predates this macOS build or
    /// sidecar. Nemotron 3 only.
    #[serde(default)]
    pub needs_warm_up: bool,
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
                needs_warm_up: false,
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
    /// Which engine this progress event belongs to. Every engine shares the
    /// diarize_download_progress channel, and the frontend filters on this so
    /// simultaneous downloads don't cross into each other's progress bars.
    pub engine: String,
}

/// Which half of a model download a failure belongs to: fetching the files, or
/// preparing them once they are on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DownloadStep {
    Fetch,
    Prepare,
}

impl DownloadStep {
    /// The step after one line of the sidecar's stdout. A failure the sidecar
    /// catches names its own step; otherwise the last progress phase stands in,
    /// which is all a crash leaves. FluidAudio compiles only once every file is
    /// on disk, and starts over from `listing` when a compile fails.
    fn after(self, line: &serde_json::Value) -> Self {
        let field = |key: &str| line.get(key).and_then(serde_json::Value::as_str);
        match field("event") {
            Some("failed") if field("step") == Some("prepare") => Self::Prepare,
            Some("failed") => Self::Fetch,
            Some("progress") => match field("phase") {
                Some("compiling" | "warming") => Self::Prepare,
                _ => Self::Fetch,
            },
            _ => self,
        }
    }

    fn failure(self, reason: &str) -> String {
        match self {
            Self::Fetch => format!("The download didn’t finish: {reason}"),
            Self::Prepare => format!("Preparing the model for this Mac failed: {reason}"),
        }
    }
}

/// Trigger the model download via the sidecar, emitting Tauri events for
/// each progress line so the UI can show a progress bar. Phases are FluidAudio's
/// `listing` → `downloading` → `compiling`, then `warming` for an engine that
/// compiles for the Neural Engine before it reports done. Every failure's
/// message names which of the two steps failed.
pub async fn download(app: &AppHandle, engine: Engine) -> Result<()> {
    // Two sidecars fetching one model would write the same files.
    static ONE_AT_A_TIME: LazyLock<tokio::sync::Mutex<()>> =
        LazyLock::new(|| tokio::sync::Mutex::new(()));
    let _turn = ONE_AT_A_TIME.lock().await;
    let mut step = DownloadStep::Fetch;
    let outcome = run_download(app, engine, &mut step).await;
    outcome.map_err(|e| anyhow!("{}", step.failure(&e.to_string())))
}

async fn run_download(app: &AppHandle, engine: Engine, step: &mut DownloadStep) -> Result<()> {
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
        *step = step.after(&v);
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
        eprintln!("speaker-diarize download --engine {} failed:\n{stderr_text}", engine.arg());
        return Err(anyhow!("{}", sidecar_error(&stderr_text, exit)));
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
    fn drops_contained_noise_end_to_end_pattern() {
        // An end-to-end engine's typical artifact: an 81 ms S1 sliver fully
        // inside an 800 ms S0 segment. Drop the sliver.
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

    #[test]
    fn payload_parses_clean_single_line() {
        let stdout = r#"[{"start_ms":0,"end_ms":1000,"speaker_id":"S0"}]"#;
        assert_eq!(
            parse_segment_payload(stdout).unwrap(),
            vec![seg(0, 1000, "S0")]
        );
    }

    #[test]
    fn payload_ignores_trailing_noise_line() {
        // The regression: FluidAudio's stdio-buffered `print()` flushes at
        // process exit, *after* the sidecar's unbuffered payload write, so a
        // stray line trails the JSON array. The old `from_str(stdout.trim())`
        // died here with "trailing characters at line 2 column 1".
        let stdout = "[{\"start_ms\":0,\"end_ms\":1000,\"speaker_id\":\"S0\"},\
            {\"start_ms\":1000,\"end_ms\":2000,\"speaker_id\":\"S1\"}]\n\
            [DEBUG] Phase 2 complete: diarizerChunks=42, totalProbs=1680, totalFrames=420\n";
        assert_eq!(
            parse_segment_payload(stdout).unwrap(),
            vec![seg(0, 1000, "S0"), seg(1000, 2000, "S1")]
        );
    }

    #[test]
    fn payload_ignores_leading_noise_line() {
        // A `[`-prefixed log line ahead of the array must not false-match —
        // it doesn't parse as a segment array, so the scan moves on.
        let stdout = "[DEBUG] Phase 2 complete: diarizerChunks=42\n\
            [{\"start_ms\":0,\"end_ms\":1000,\"speaker_id\":\"S0\"}]\n";
        assert_eq!(
            parse_segment_payload(stdout).unwrap(),
            vec![seg(0, 1000, "S0")]
        );
    }

    #[test]
    fn payload_parses_empty_array() {
        // noSpeechDetected emits a bare `[]` — must round-trip to no segments,
        // not an error.
        assert!(parse_segment_payload("[]\n").unwrap().is_empty());
    }

    #[test]
    fn payload_errors_when_no_array_present() {
        assert!(parse_segment_payload("humla-error: something went wrong\n").is_err());
    }

    #[test]
    fn sidecar_error_is_the_tagged_line_not_fluidaudios_logging() {
        let stderr = "[Profiling] load 0.8 s\n\
            download error: Error Domain=com.apple.CoreML Code=1 \"Output backing …\"\n\
            humla-error: Output backing for feature named 'speaker_preds' is not compatible\n\
            [Profiling] teardown\n";
        assert_eq!(
            sidecar_error(stderr, "exit status: 1"),
            "Output backing for feature named 'speaker_preds' is not compatible"
        );
    }

    #[test]
    fn an_untagged_failure_gives_its_last_line_and_how_the_sidecar_exited() {
        assert_eq!(
            sidecar_error("first\n[WARN] last words  \n\n", "signal: 11 (SIGSEGV)"),
            "[WARN] last words (speaker-diarize signal: 11 (SIGSEGV))"
        );
        assert_eq!(sidecar_error(" \n", "exit status: 1"), "speaker-diarize exit status: 1");
    }

    #[test]
    fn an_empty_tagged_line_is_not_a_message() {
        assert_eq!(
            sidecar_error("loading\nhumla-error: \n", "exit status: 1"),
            "loading (speaker-diarize exit status: 1)"
        );
    }

    fn step_after(lines: &[&str]) -> DownloadStep {
        lines.iter().fold(DownloadStep::Fetch, |step, line| {
            step.after(&serde_json::from_str(line).unwrap())
        })
    }

    fn progress(phase: &str) -> String {
        format!(r#"{{"event":"progress","fraction":0.5,"phase":"{phase}"}}"#)
    }

    #[test]
    fn a_failure_the_sidecar_catches_names_its_own_step() {
        let warming = progress("warming");
        let downloading = progress("downloading");
        assert_eq!(
            step_after(&[&downloading, r#"{"event":"failed","step":"prepare"}"#]),
            DownloadStep::Prepare
        );
        assert_eq!(
            step_after(&[&warming, r#"{"event":"failed","step":"download"}"#]),
            DownloadStep::Fetch
        );
    }

    #[test]
    fn a_crash_is_placed_by_the_last_progress_phase() {
        for (phases, expected) in [
            (vec!["listing", "downloading"], DownloadStep::Fetch),
            (vec!["downloading", "compiling"], DownloadStep::Prepare),
            // A failed compile starts FluidAudio over from the download.
            (vec!["compiling", "listing", "downloading"], DownloadStep::Fetch),
            (vec!["downloading", "warming"], DownloadStep::Prepare),
        ] {
            let lines: Vec<String> = phases.iter().map(|p| progress(p)).collect();
            let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
            assert_eq!(step_after(&lines), expected, "{phases:?}");
        }
        assert_eq!(step_after(&[r#"{"event":"done"}"#]), DownloadStep::Fetch);
    }

    #[test]
    fn the_old_nemotron_bundle_is_replaced_only_for_an_install_on_nemotron() {
        assert_eq!(old_bundle_plan(true, Engine::Nemotron3), OldBundle::Replace);
        assert_eq!(old_bundle_plan(true, Engine::Community1), OldBundle::Remove);
        assert_eq!(old_bundle_plan(false, Engine::Nemotron3), OldBundle::Absent);
        assert_eq!(old_bundle_plan(false, Engine::Community1), OldBundle::Absent);
    }

    #[test]
    fn the_old_nemotron_bundle_is_not_the_one_the_sidecar_loads() {
        let sidecar = include_str!("../../speaker-diarize/Sources/speaker-diarize/main.swift");
        assert!(sidecar.contains("Nemotron3Config.fast128"));
        let package = include_str!("../../speaker-diarize/Package.swift");
        assert!(
            package.contains(r#"exact: "0.17.4""#),
            "a FluidAudio bump must re-check which Nemotron bundle {OLD_NEMOTRON_BUNDLE} names"
        );
    }

    #[test]
    fn status_reads_the_sidecars_warm_up_flag_and_defaults_it_off() {
        let sidecar = include_str!("../../speaker-diarize/Sources/speaker-diarize/main.swift");
        assert!(sidecar.contains(r#""needsWarmUp": needsWarmUp"#));
        let stale: ModelStatus = serde_json::from_str(
            r#"{"downloaded":false,"needsWarmUp":true,"path":"/m","sizeBytes":1}"#,
        )
        .unwrap();
        assert!(stale.needs_warm_up && !stale.downloaded);
        let older: ModelStatus =
            serde_json::from_str(r#"{"downloaded":true,"path":"/m","sizeBytes":1}"#).unwrap();
        assert!(!older.needs_warm_up);
    }

    #[test]
    fn a_download_failure_is_worded_by_its_step() {
        assert_eq!(
            DownloadStep::Fetch.failure("The Internet connection appears to be offline."),
            "The download didn’t finish: The Internet connection appears to be offline."
        );
        assert_eq!(
            DownloadStep::Prepare.failure(
                "Output backing for feature named 'speaker_preds' is not compatible with the model's output feature description."
            ),
            "Preparing the model for this Mac failed: Output backing for feature named 'speaker_preds' is not compatible with the model's output feature description."
        );
    }

    #[test]
    fn engine_setting_round_trips_and_anything_else_reads_as_community1() {
        for engine in [Engine::Community1, Engine::Nemotron3] {
            assert_eq!(Engine::from_setting(engine.arg()), engine);
        }
        assert_eq!(Engine::from_setting("sortformer"), Engine::Community1);
        assert_eq!(Engine::from_setting(""), Engine::Community1);
    }

    #[test]
    fn parsing_an_engine_argument_refuses_an_unknown_one() {
        assert_eq!(Engine::parse("nemotron3"), Some(Engine::Nemotron3));
        assert_eq!(Engine::parse("community1"), Some(Engine::Community1));
        assert_eq!(Engine::parse("sortformer"), None);
    }

    #[test]
    fn a_note_up_to_eight_speakers_prefers_the_selected_engine() {
        for hint in [None, Some(1), Some(3), Some(8)] {
            assert_eq!(
                engine_preference(Engine::Nemotron3, hint),
                vec![Engine::Nemotron3, Engine::Community1],
                "hint {hint:?}"
            );
        }
    }

    #[test]
    fn a_note_above_eight_speakers_prefers_community1_whichever_is_selected() {
        assert_eq!(
            engine_preference(Engine::Community1, Some(9)),
            vec![Engine::Community1, Engine::Nemotron3]
        );
    }

    #[test]
    fn a_note_above_eight_speakers_prefers_community1() {
        assert_eq!(
            engine_preference(Engine::Nemotron3, Some(9)),
            vec![Engine::Community1, Engine::Nemotron3]
        );
    }

    #[test]
    fn community1_selected_falls_back_to_nemotron_only_when_its_model_is_missing() {
        // A Sortformer install moved to community-1 may never have fetched it.
        for hint in [None, Some(4)] {
            assert_eq!(
                engine_preference(Engine::Community1, hint),
                vec![Engine::Community1, Engine::Nemotron3]
            );
        }
        assert_eq!(
            engine_preference(Engine::Community1, Some(12)),
            vec![Engine::Community1, Engine::Nemotron3]
        );
    }

    #[test]
    fn only_community1_takes_the_speaker_hint() {
        assert!(Engine::Community1.takes_speaker_hint());
        assert!(!Engine::Nemotron3.takes_speaker_hint());
    }

    #[test]
    fn retired_model_dirs_go_and_the_models_in_use_stay() {
        let root = tempfile::tempdir().unwrap();
        let models = root.path();
        for dir in [
            "sortformer/SortformerNvidiaHigh_v2.1.mlmodelc",
            "speaker-diarization/pyannote_segmentation.mlmodelc",
            "speaker-diarization/Segmentation.mlmodelc",
            "nemotron-3-diarization/monolithic",
        ] {
            std::fs::create_dir_all(models.join(dir)).unwrap();
        }
        let conn = crate::db::settings_only_conn();

        remove_retired_model_dirs(models, &conn);

        assert!(!models.join("sortformer").exists());
        assert!(!models.join("speaker-diarization/pyannote_segmentation.mlmodelc").exists());
        assert!(models.join("speaker-diarization/Segmentation.mlmodelc").exists());
        assert!(models.join("nemotron-3-diarization/monolithic").exists());
    }

    #[test]
    fn retired_model_dirs_are_removed_once_per_install() {
        let root = tempfile::tempdir().unwrap();
        let conn = crate::db::settings_only_conn();
        remove_retired_model_dirs(root.path(), &conn);

        // Another FluidAudio client may use the folder later; that is its own.
        std::fs::create_dir_all(root.path().join("sortformer")).unwrap();
        remove_retired_model_dirs(root.path(), &conn);

        assert!(root.path().join("sortformer").exists());
    }
}
