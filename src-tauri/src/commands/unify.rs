//! Cross-session speaker unification (#17): one clustering pass over a note's
//! joined takes, relabelling every take's timeline from the result.

use super::timeline::{commit_rebuilt_transcript, read_timeline_values, rebuild_note_transcript};
use super::transcript::{single_piece, split_by_labels, LabelledPiece};
use super::{
    assign_speaker, diarize_and_maybe_clean, distinct_speaker_count, emit_error,
    finalise_stream_labels, log_echo_check, note_diarize_settings, parse_chunks_json,
    serialize_timeline, ChainProgress, EchoPass, HybridLabels, NoteDiarizeSettings, StreamSteps,
};
use crate::recording::{ChunkRecord, ChunkSource, Step, StepReport};
use crate::{diarize, sessions, wav, AppState};
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Manager};

// ---- Cross-session speaker unification (#17) -------------------------------
//
// Each recording session is diarized on its own WAV, so clustering can't know
// that session 2's "Speaker 1" is the same voice as session 1's — the offset
// combine just renumbers past the previous max and a 2-person, 3-stop meeting
// shows up to 8 labels. The unify pass concatenates the retained per-session
// source WAVs (per stream, matching the per-source diarize passes), re-runs
// clustering over the combined audio so one voice = one cluster across takes,
// and rebuilds every session's timeline labels from the unified result. User
// renames survive: the old timeline labels are carried onto the new clusters
// by speech-time overlap (custom names beat generated `Speaker N` ones).
//
// The pass recomputes from fresh clustering every run — no stored marker —
// so re-running it is idempotent by construction: the same audio yields the
// same clusters, and re-applying the carried names reproduces the same
// timelines.

/// Which streams a session's chunks cover. Mirrors `diarize_and_apply`'s
/// per-take branch: mic-only diarizes the mic stream, sys-only the system
/// stream, hybrid labels mic "You" by channel attribution and diarizes sys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionMode {
    MicOnly,
    SysOnly,
    Hybrid,
}

/// Classify a session's chunk log. `None` when it recorded nothing.
fn session_mode(chunks: &[ChunkRecord]) -> Option<SessionMode> {
    let mic = chunks.iter().any(|c| c.source == ChunkSource::Mic);
    let sys = chunks.iter().any(|c| c.source == ChunkSource::Sys);
    match (mic, sys) {
        (true, false) => Some(SessionMode::MicOnly),
        (false, true) => Some(SessionMode::SysOnly),
        (true, true) => Some(SessionMode::Hybrid),
        (false, false) => None,
    }
}

/// Whether a session can join the concatenated unify pass, given which source
/// WAVs survive on disk: it needs every WAV whose stream will be diarized.
/// Mic-only needs `mic.wav`, sys-only needs `sys.wav`, and **hybrid needs
/// both** — its mic is diarized now rather than being labelled `You` by
/// channel attribution, and `concat_wavs` fails the whole pass on an
/// unreadable input, which would also misalign every later session's concat
/// offset. Sessions that fail this stay "frozen": their existing labels are
/// kept and the unified numbering is offset past them.
fn session_unifiable(mode: SessionMode, has_mic_wav: bool, has_sys_wav: bool) -> bool {
    match mode {
        SessionMode::MicOnly => has_mic_wav,
        SessionMode::SysOnly => has_sys_wav,
        SessionMode::Hybrid => has_mic_wav && has_sys_wav,
    }
}

/// One session's input to the unified relabel pass.
struct UnifySession {
    session_id: String,
    mode: SessionMode,
    /// Chunk log with session-local times, exactly as recorded.
    chunks: Vec<ChunkRecord>,
    /// This session's start offset (ms) inside the concatenated mic / sys
    /// WAV. Only meaningful for the stream(s) this session contributed.
    mic_offset_ms: u64,
    sys_offset_ms: u64,
    /// The labels currently on disk (timeline.jsonl before the rewrite) —
    /// the only place user renames live (there is no metadata table).
    old_spans: Vec<LabelSpan>,
}

/// One timeline entry's label + time span (+ stream when the entry was
/// written by a build that records it). Used to carry user renames from the
/// pre-unify timeline onto the freshly clustered one by time overlap.
#[derive(Clone, Debug)]
struct LabelSpan {
    start_ms: u64,
    end_ms: u64,
    label: String,
    source: Option<ChunkSource>,
}

/// Millisecond start offset of each input inside a concatenation, from real
/// sample counts (16 kHz mono → 16 samples per ms). Using actual samples
/// rather than the manifest's best-effort `durationMs` keeps the chunk-offset
/// map aligned with the diarizer's own clock over multi-session concats.
fn concat_offsets_ms(sample_counts: &[usize]) -> Vec<u64> {
    let mut out = Vec::with_capacity(sample_counts.len());
    let mut acc_samples: u64 = 0;
    for &n in sample_counts {
        out.push(acc_samples / 16);
        acc_samples += n as u64;
    }
    out
}

/// Labels the pipeline emits itself: `Speaker N` and the fixed hybrid-mic
/// `You` (plus the empty "no label" marker). Everything else is a user
/// rename. Deviation from a literal "custom = not /^Speaker \d+$/" reading
/// for "You": it's system-generated channel attribution, and treating it as
/// custom would let a remote speaker's cluster inherit "You" through
/// incidental time overlap with the user's own mic entries.
fn is_generated_label(label: &str) -> bool {
    if label.is_empty() || label == "You" {
        return true;
    }
    label
        .strip_prefix("Speaker ")
        .map(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
        .unwrap_or(false)
}

/// Overlap between two spans in ms; 0 when disjoint.
fn spans_overlap_ms(a: &LabelSpan, b: &LabelSpan) -> u64 {
    let start = a.start_ms.max(b.start_ms);
    let end = a.end_ms.min(b.end_ms);
    end.saturating_sub(start)
}

/// Parse timeline JSONL values into label spans. `source` is `None` for
/// entries written before the field existed (pre-#17 builds).
fn spans_from_values(values: &[serde_json::Value]) -> Vec<LabelSpan> {
    values
        .iter()
        .filter_map(|v| {
            let label = v.get("label")?.as_str()?.to_string();
            let start_ms = v.get("start_ms")?.as_u64()?;
            let end_ms = v.get("end_ms")?.as_u64()?;
            let source = v
                .get("source")
                .and_then(|s| s.as_str())
                .and_then(|s| match s {
                    "mic" => Some(ChunkSource::Mic),
                    "sys" => Some(ChunkSource::Sys),
                    _ => None,
                });
            Some(LabelSpan { start_ms, end_ms, label, source })
        })
        .collect()
}

/// Highest exact `Speaker N` label in a session's timeline file. Drives the
/// numbering offset that keeps unified labels from colliding with frozen
/// (non-unifiable) sessions' existing numbers.
fn max_speaker_in_timeline(path: &std::path::Path) -> u32 {
    read_timeline_values(path)
        .iter()
        .filter_map(|v| v.get("label").and_then(|s| s.as_str()).map(str::to_string))
        .filter_map(|l| l.strip_prefix("Speaker ").and_then(|r| r.parse::<u32>().ok()))
        .max()
        .unwrap_or(0)
}

/// First-encounter display numbering over the whole note. Walks sessions in
/// manifest order and chunks in the `(start_ms, source)` order the timeline
/// serialiser uses, resolving each chunk (or word midpoint) against the
/// combined-timeline segments after shifting by the session's concat offset.
/// One shared counter across both streams so a reader meets `Speaker 1`,
/// `Speaker 2`, … in reading order regardless of which stream each voice is
/// on. Hybrid sessions' mic chunks are skipped (fixed "You").
fn build_unified_display_map(
    sessions: &[UnifySession],
    mic_segments: &[diarize::Segment],
    sys_segments: &[diarize::Segment],
) -> (
    std::collections::HashMap<String, u32>,
    std::collections::HashMap<String, u32>,
    u32,
) {
    let mut mic_map: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let mut sys_map: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let mut next: u32 = 1;
    for sess in sessions {
        let mut sorted: Vec<&ChunkRecord> = sess.chunks.iter().collect();
        sorted.sort_by_key(|c| {
            let source_rank = match c.source {
                ChunkSource::Mic => 0,
                ChunkSource::Sys => 1,
            };
            (c.start_ms, source_rank)
        });
        for chunk in sorted {
            // A hybrid take's mic used to be skipped here — it was labelled
            // `You` by channel attribution and never diarized. It now gets
            // numbered like any other stream so several people sharing one
            // mic stay distinct; `finalise_stream_labels` re-applies `You`
            // when the mic really did resolve to a single voice.
            let (segments, offset, map) = match chunk.source {
                ChunkSource::Mic => (mic_segments, sess.mic_offset_ms, &mut mic_map),
                ChunkSource::Sys => (sys_segments, sess.sys_offset_ms, &mut sys_map),
            };
            if segments.is_empty() {
                continue;
            }
            if chunk.words.is_empty() {
                if let Some(sid) = assign_speaker(chunk.start_ms.saturating_add(offset), segments) {
                    if !map.contains_key(sid) {
                        map.insert(sid.to_string(), next);
                        next += 1;
                    }
                }
                continue;
            }
            for word in &chunk.words {
                let half = word.end_ms.saturating_sub(word.start_ms) / 2;
                let mid = word.start_ms.saturating_add(half);
                let abs = chunk
                    .start_ms
                    .saturating_add(mid)
                    .saturating_add(offset);
                if let Some(sid) = assign_speaker(abs, segments) {
                    if !map.contains_key(sid) {
                        map.insert(sid.to_string(), next);
                        next += 1;
                    }
                }
            }
        }
    }
    (mic_map, sys_map, next)
}

/// Result of the pure relabel pass: per-session timeline JSONL (manifest
/// order, session-local times) plus human-readable notices for custom-name
/// collisions the pass had to resolve.
struct UnifyOutcome {
    timelines: Vec<(String, String)>,
    notices: Vec<String>,
}

/// Carry user renames from the old timelines onto the freshly clustered
/// labels. For each new (generated) label, accumulate speech-time overlap
/// against old *custom* labels — matched within the same session, and within
/// the same stream when both sides know their source. Rules (issue #17):
/// a custom name beats a generated `Speaker N`; when two different custom
/// names land in one cluster, the one covering more speech time wins and a
/// notice is emitted so the user can see (and undo, via rename) the merge.
fn custom_name_map(
    new_per_session: &[Vec<LabelSpan>],
    old_per_session: &[Vec<LabelSpan>],
) -> (std::collections::HashMap<String, String>, Vec<String>) {
    let mut acc: std::collections::HashMap<String, std::collections::HashMap<String, u64>> =
        std::collections::HashMap::new();
    for (news, olds) in new_per_session.iter().zip(old_per_session) {
        for n in news {
            // New labels are always pipeline-generated at this point; an
            // empty label marks an unlabelled piece and must never gain a
            // name it didn't have.
            if n.label.is_empty() || !is_generated_label(&n.label) {
                continue;
            }
            for o in olds {
                if is_generated_label(&o.label) {
                    continue;
                }
                if let (Some(a), Some(b)) = (n.source, o.source) {
                    if a != b {
                        continue;
                    }
                }
                let t = spans_overlap_ms(n, o);
                if t > 0 {
                    *acc.entry(n.label.clone())
                        .or_default()
                        .entry(o.label.clone())
                        .or_default() += t;
                }
            }
        }
    }
    let mut map = std::collections::HashMap::new();
    let mut notices = Vec::new();
    let mut keys: Vec<&String> = acc.keys().collect();
    keys.sort();
    for k in keys {
        let mut ranked: Vec<(&String, u64)> = acc[k].iter().map(|(n, &t)| (n, t)).collect();
        // Most speech time wins; name ascending as a deterministic tiebreak.
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        let (winner, _) = ranked[0];
        map.insert(k.clone(), winner.clone());
        if ranked.len() >= 2 {
            let losers: Vec<&str> = ranked[1..].iter().map(|(n, _)| n.as_str()).collect();
            notices.push(format!(
                "Speaker unification detected \"{}\" and \"{}\" as the same voice — kept \"{}\" (more speech time). Rename the speaker to adjust.",
                winner,
                losers.join("\" and \""),
                winner
            ));
        }
    }
    (map, notices)
}

/// The pure core of the unify pass: given every unifiable session's chunks,
/// concat offsets, and old labels, plus the segments from diarizing the
/// concatenated stream(s), rebuild each session's timeline JSONL with
/// unified labels. Times stay session-local — the splitter shifts a chunk
/// into concat time only for the segment lookup, and `serialize_timeline`
/// rebases words off the original chunk. `label_offset` bumps generated
/// numbers past frozen sessions' existing ones.
fn unify_relabel(
    sessions: &[UnifySession],
    mic_segments: &[diarize::Segment],
    sys_segments: &[diarize::Segment],
    label_offset: u32,
) -> UnifyOutcome {
    let (mic_nums, sys_nums, next) =
        build_unified_display_map(sessions, mic_segments, sys_segments);
    // `You` is a channel-attribution artifact of a *remote call*: it only holds
    // when every take that captured mic was one, and the combined mic stream
    // really did resolve to a single voice (see `finalise_stream_labels`). A
    // note that also has an in-person take numbers its voices instead —
    // otherwise unifying a solo remote take with an in-person one would relabel
    // the in-person speaker `You`, which is not what that take recorded.
    let mic_sessions: Vec<&UnifySession> = sessions
        .iter()
        .filter(|s| s.mode != SessionMode::SysOnly)
        .collect();
    let allow_you = !mic_sessions.is_empty()
        && mic_sessions.iter().all(|s| s.mode == SessionMode::Hybrid);
    let HybridLabels { mic: mic_map, sys: sys_map, .. } =
        finalise_stream_labels(mic_nums, sys_nums, allow_you, next);

    let mut values_per_session: Vec<Vec<serde_json::Value>> = Vec::new();
    for sess in sessions {
        let mic_off = sess.mic_offset_ms;
        let sys_off = sess.sys_offset_ms;
        let splitter = |c: &ChunkRecord| -> Vec<LabelledPiece> {
            match c.source {
                // A mic stream with no segments (diarize unavailable for this
                // note's mic side) keeps the old channel-attributed label
                // rather than emitting `None`, which would glue the text onto
                // whatever line came before it.
                ChunkSource::Mic if mic_segments.is_empty() => {
                    single_piece(c, Some("You".to_string()))
                }
                ChunkSource::Mic => {
                    let mut shifted = c.clone();
                    shifted.start_ms = shifted.start_ms.saturating_add(mic_off);
                    split_by_labels(&shifted, mic_segments, &mic_map)
                }
                ChunkSource::Sys => {
                    let mut shifted = c.clone();
                    shifted.start_ms = shifted.start_ms.saturating_add(sys_off);
                    split_by_labels(&shifted, sys_segments, &sys_map)
                }
            }
        };
        let jsonl = serialize_timeline(&sess.chunks, &splitter, label_offset);
        values_per_session.push(
            jsonl
                .lines()
                .filter(|l| !l.trim().is_empty())
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect(),
        );
    }

    let new_spans: Vec<Vec<LabelSpan>> =
        values_per_session.iter().map(|v| spans_from_values(v)).collect();
    let old_spans: Vec<Vec<LabelSpan>> =
        sessions.iter().map(|s| s.old_spans.clone()).collect();
    let (rename_map, notices) = custom_name_map(&new_spans, &old_spans);

    let mut timelines = Vec::new();
    for (sess, mut values) in sessions.iter().zip(values_per_session) {
        for v in values.iter_mut() {
            if let Some(label) = v.get("label").and_then(|s| s.as_str()) {
                if let Some(new_label) = rename_map.get(label) {
                    v["label"] = serde_json::Value::String(new_label.clone());
                }
            }
        }
        let mut out = String::new();
        for v in &values {
            out.push_str(&v.to_string());
            out.push('\n');
        }
        timelines.push((sess.session_id.clone(), out));
    }
    UnifyOutcome { timelines, notices }
}

/// One session that qualified for the concatenated pass, with its chunk log
/// already loaded.
struct UnifyCandidate {
    entry: sessions::SessionEntry,
    dir: PathBuf,
    chunks: Vec<ChunkRecord>,
    mode: SessionMode,
}

/// Concatenate 16 kHz mono WAVs into `out`, returning each input's start
/// offset (ms) inside the result, computed from real sample counts. `None`
/// when `paths` is empty (no file written).
async fn concat_wavs(
    paths: &[PathBuf],
    out: &std::path::Path,
) -> anyhow::Result<Option<Vec<u64>>> {
    if paths.is_empty() {
        return Ok(None);
    }
    let mut combined: Vec<f32> = Vec::new();
    let mut counts = Vec::with_capacity(paths.len());
    for p in paths {
        let samples = wav::read_f32_mono_16k(p).await?;
        counts.push(samples.len());
        combined.extend_from_slice(&samples);
    }
    wav::write_pcm16_mono_16k(out, &combined).await?;
    Ok(Some(concat_offsets_ms(&counts)))
}

/// What the named steps inside one unify pass cost, in ms, beyond the pass as
/// a whole.
pub(crate) struct UnifyCosts {
    /// The echo pass over every hybrid take; `None` when there was none.
    pub(super) echo_ms: Option<u64>,
}

/// A scratch dir for one unify invocation's concat WAVs. Unique per call (a
/// UUID suffix) so two overlapping unify passes for the *same* note — auto-unify
/// in the post-stop chain racing a user `rediarize_note`, or two rapid
/// Re-diarize clicks — never write to and `remove_dir_all` each other's concat
/// files (a mid-read truncation that silently produced wrong unified labels).
fn unify_scratch_dir(note_id: &str) -> PathBuf {
    std::env::temp_dir().join(format!("humla-unify-{note_id}-{}", uuid::Uuid::new_v4()))
}

/// Get (or lazily create) the per-note unify lock. Same note → same `Arc`, so
/// concurrent unify passes for it serialize; different notes get independent
/// locks and stay concurrent. The `parking_lot` guard is dropped before the
/// caller `.await`s on the returned tokio lock.
fn unify_note_lock(
    locks: &parking_lot::Mutex<std::collections::HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    note_id: &str,
) -> Arc<tokio::sync::Mutex<()>> {
    locks.lock().entry(note_id.to_string()).or_default().clone()
}

/// Run the cross-session speaker unification pass on a note: every take's
/// audio joined end to end and diarized once per stream, all takes each time,
/// so one voice keeps one label across them. The joined WAVs live in a temp
/// dir removed when the pass ends.
///
/// Returns what it cost when the pass ran and rewrote labels, and `Ok(None)`
/// when the note doesn't qualify (fewer than two sessions, fewer than two with
/// retained source audio and chunk timings, or no diarize model), in which case
/// callers keep the per-take labels. `Err` leaves the existing labels
/// untouched: the pass writes only after every diarize it needed succeeded.
pub(crate) async fn unify_note_speakers(
    app: &AppHandle,
    note_id: &str,
    // Where the pass reports. Passed in, never chosen here: this one function
    // is reached from the live stop, the user-pressed Re-diarize and a deferred
    // replay, and only the first of those owns `recording_status`.
    progress: &ChainProgress,
) -> anyhow::Result<Option<UnifyCosts>> {
    // Per-note re-entrancy guard: a second unify for this note waits for the
    // first to finish (then re-runs on its up-to-date timelines) rather than
    // racing it. Held for the whole pass; safe because unify is a leaf that
    // never re-enters itself, so this can't deadlock the post-stop chain.
    let note_lock = unify_note_lock(&app.state::<AppState>().unify_locks, note_id);
    let _unify_guard = note_lock.lock().await;

    let app_dir = app.path().app_data_dir()?;
    let recordings = sessions::recordings_dir(&app_dir, note_id);
    let resolved = sessions::resolve_sessions(&recordings);
    if resolved.len() < 2 {
        return Ok(None);
    }

    let settings = note_diarize_settings(app, note_id).await;
    if !settings.choice.downloaded {
        eprintln!("unify: diarize model not downloaded, keeping per-take labels");
        return Ok(None);
    }

    // Partition into sessions that can join the concatenated pass and
    // "frozen" ones that keep their existing labels — a take without
    // chunks.json or its diarized-stream WAV (e.g. a first take recorded
    // before auto-retention kicked in, or a keep-audio-off legacy note)
    // can't be re-clustered, so its labels stay and the unified numbering
    // is offset past them.
    let mut unifiable: Vec<UnifyCandidate> = Vec::new();
    let mut frozen_dirs: Vec<PathBuf> = Vec::new();
    for (entry, dir) in resolved {
        let chunks_path = dir.join("chunks.json");
        let chunks = if chunks_path.exists() {
            parse_chunks_json(&chunks_path).unwrap_or_default()
        } else {
            Vec::new()
        };
        let has_mic = dir.join("mic.wav").exists();
        let has_sys = dir.join("sys.wav").exists();
        match session_mode(&chunks) {
            Some(mode) if session_unifiable(mode, has_mic, has_sys) => {
                unifiable.push(UnifyCandidate { entry, dir, chunks, mode });
            }
            _ => frozen_dirs.push(dir),
        }
    }
    if unifiable.len() < 2 {
        eprintln!(
            "unify: only {} session(s) have retained audio + chunk timings (need 2+), keeping per-take labels",
            unifiable.len()
        );
        return Ok(None);
    }

    // Concat WAVs are scratch files — never inside the session dirs. Unique per
    // invocation so overlapping passes can't clobber each other's concats.
    let tmp = unify_scratch_dir(note_id);
    tokio::fs::create_dir_all(&tmp).await?;
    let result = unify_apply(app, note_id, &tmp, &unifiable, &frozen_dirs, settings, progress).await;
    let _ = tokio::fs::remove_dir_all(&tmp).await;
    result.map(Some)
}

/// The IO half of the unify pass, split out so the caller can always clean
/// up the temp dir. Concatenates + diarizes per stream, runs the pure
/// relabel, writes every unified session's timeline, and rebuilds the DB
/// transcript from all sessions (frozen ones included, untouched).
async fn unify_apply(
    app: &AppHandle,
    note_id: &str,
    tmp: &std::path::Path,
    unifiable: &[UnifyCandidate],
    frozen_dirs: &[PathBuf],
    settings: NoteDiarizeSettings,
    progress: &ChainProgress,
) -> anyhow::Result<UnifyCosts> {
    let NoteDiarizeSettings { choice, thresholds, expected_speakers } = settings;
    let mut engine = choice.engine;
    // Per-stream concatenation in manifest order. Mic concat = every session
    // that captured mic (mic-only *and* hybrid — a hybrid take's mic is
    // diarized now, not assumed to be one person); sys concat = sys-only +
    // hybrid. Matches the per-source passes diarize_and_apply runs on a single
    // take. `session_unifiable` guarantees a hybrid session has both WAVs, so
    // neither concat can hit a missing file and misalign the offsets.
    let mic_takes: Vec<&UnifyCandidate> =
        unifiable.iter().filter(|c| c.mode != SessionMode::SysOnly).collect();
    let sys_paths: Vec<PathBuf> = unifiable
        .iter()
        .filter(|c| c.mode != SessionMode::MicOnly)
        .map(|c| c.dir.join("sys.wav"))
        .collect();
    let mic_concat = tmp.join("mic-concat.wav");
    let sys_concat = tmp.join("sys-concat.wav");
    let mut echoes = cancel_each_takes_echo(app, progress, &mic_takes).await;
    let echo_ms = echoes.iter().filter_map(|e| e.ms).reduce(|a, b| a + b);
    let mic_paths: Vec<PathBuf> = mic_takes
        .iter()
        .zip(&echoes)
        .map(|(take, echo)| echo.mic_input(&take.dir.join("mic.wav")).to_path_buf())
        .collect();
    // Named a stream at a time: each stream's concat and its diarize are
    // announced together, so the label names the work actually running.
    let mut unify_steps = StreamSteps::new(
        app,
        progress,
        Step::MatchingSpeakers,
        &[!mic_paths.is_empty(), !sys_paths.is_empty()],
    );
    // Same speaker-count hint semantics as the per-take pass. With no system
    // stream in play the note's expected_speakers applies to the mic directly
    // (in-person). Once both streams exist the total can't be split a priori,
    // so the mic goes in unhinted and the sys stream is asked for whatever the
    // mic didn't account for. Only community-1 takes the hint; Nemotron 3
    // counts the combined audio itself, as it does a single take.
    if !mic_paths.is_empty() {
        unify_steps.begin();
    }
    let mic_offsets = concat_wavs(&mic_paths, &mic_concat).await?;
    echoes.iter_mut().for_each(EchoPass::release_cancelled_mic);
    let mic_hint = if sys_paths.is_empty() {
        expected_speakers
    } else {
        None
    };
    let mic_segments = match &mic_offsets {
        Some(_) => diarize_and_maybe_clean(app, note_id, &mic_concat, mic_hint, &mut engine, thresholds).await?,
        None => Vec::new(),
    };
    if mic_offsets.is_some() && mic_segments.is_empty() {
        anyhow::bail!("diarize returned no segments for the combined mic stream");
    }
    // Dropped before the sys hint counts the mic's voices, which echo must
    // not add to.
    let (mic_segments, echo_takes) = drop_joined_echo(
        &mic_takes,
        &echoes,
        mic_offsets.as_deref().unwrap_or_default(),
        &mic_segments,
    );
    if !sys_paths.is_empty() {
        unify_steps.begin();
    }
    let sys_offsets = concat_wavs(&sys_paths, &sys_concat).await?;
    let sys_hint = if mic_paths.is_empty() {
        expected_speakers
    } else {
        expected_speakers
            .map(|n| (n - distinct_speaker_count(&mic_segments).max(1) as i64).max(1))
    };
    let sys_segments = match &sys_offsets {
        Some(_) => {
            diarize_and_maybe_clean(app, note_id, &sys_concat, sys_hint, &mut engine, thresholds).await?
        }
        None => Vec::new(),
    };
    if sys_offsets.is_some() && sys_segments.is_empty() {
        anyhow::bail!("diarize returned no segments for the combined system stream");
    }
    if !echo_takes.is_empty() {
        write_unify_dump(app, note_id, engine, &echo_takes).await;
    }

    // Unified generated numbers start past any frozen session's existing
    // ones so the two label spaces never collide in the merged transcript.
    let label_offset = frozen_dirs
        .iter()
        .map(|d| max_speaker_in_timeline(&d.join("timeline.jsonl")))
        .max()
        .unwrap_or(0);

    let mut mic_iter = mic_offsets.unwrap_or_default().into_iter();
    let mut sys_iter = sys_offsets.unwrap_or_default().into_iter();
    let sessions_in: Vec<UnifySession> = unifiable
        .iter()
        .map(|c| {
            // Must mirror `mic_takes` / `sys_paths` above exactly — these
            // offsets are consumed in the same order the concat was built, so a
            // filter that disagrees shifts every later session's segment
            // lookups into the wrong take's audio.
            let mic_offset_ms = if c.mode != SessionMode::SysOnly {
                mic_iter.next().unwrap_or(0)
            } else {
                0
            };
            let sys_offset_ms = if c.mode != SessionMode::MicOnly {
                sys_iter.next().unwrap_or(0)
            } else {
                0
            };
            UnifySession {
                session_id: c.entry.id.clone(),
                mode: c.mode,
                chunks: c.chunks.clone(),
                mic_offset_ms,
                sys_offset_ms,
                old_spans: spans_from_values(&read_timeline_values(
                    &c.dir.join("timeline.jsonl"),
                )),
            }
        })
        .collect();

    let outcome = unify_relabel(&sessions_in, &mic_segments, &sys_segments, label_offset);

    for ((session_id, jsonl), cand) in outcome.timelines.iter().zip(unifiable) {
        debug_assert_eq!(session_id, &cand.entry.id);
        tokio::fs::write(cand.dir.join("timeline.jsonl"), jsonl).await?;
    }

    // Rebuild the DB transcript from every session's timeline (frozen ones
    // keep their old labels) and notify the UI + sync.
    let transcript = rebuild_note_transcript(app, note_id).map_err(|e| anyhow::anyhow!(e))?;
    commit_rebuilt_transcript(app, note_id, transcript).map_err(|e| anyhow::anyhow!(e))?;

    // Surface custom-name merges on the existing transient-toast channel
    // (recording_error doubles as the informational surface — the sidecar's
    // non-fatal Diagnostic notices already ride it).
    for notice in &outcome.notices {
        emit_error(app, Some(note_id), notice);
    }
    eprintln!(
        "unify: relabelled {} session(s) from unified clustering ({} frozen)",
        unifiable.len(),
        frozen_dirs.len()
    );
    Ok(UnifyCosts { echo_ms })
}

/// Each hybrid take's echo pass, a take at a time since every take's lag is
/// its own: one [`EchoPass`] per mic take, holding nothing for a take with no
/// system stream. `session_unifiable` has made sure a hybrid take kept both.
async fn cancel_each_takes_echo(
    app: &AppHandle,
    progress: &ChainProgress,
    mic_takes: &[&UnifyCandidate],
) -> Vec<EchoPass> {
    let hybrid = mic_takes.iter().filter(|c| c.mode == SessionMode::Hybrid).count() as u32;
    let mut nth = 0;
    let mut echoes = Vec::with_capacity(mic_takes.len());
    for take in mic_takes {
        let sys = (take.mode == SessionMode::Hybrid).then(|| take.dir.join("sys.wav"));
        let echo = EchoPass::run(Some(&take.dir.join("mic.wav")), sys.as_deref(), || {
            nth += 1;
            progress.report(app, StepReport::counted(Step::RemovingEcho, nth, hybrid));
        })
        .await;
        echoes.push(echo);
    }
    echoes
}

/// What the unify dump records of one take's echo pass.
#[derive(serde::Serialize)]
struct UnifyTakeEcho<'a> {
    session_id: &'a str,
    index: u32,
    echo: crate::echo::EchoReport,
}

/// The joined mic's voices without the ones that read as echo, each hybrid
/// take judged inside its own span of it, and what the dump records of every
/// take that had an echo pass. `offsets[i]` is where `mic_takes[i]` starts in
/// the joined stream, and `echoes[i]` is its echo pass.
fn drop_joined_echo<'a>(
    mic_takes: &[&'a UnifyCandidate],
    echoes: &[EchoPass],
    offsets: &[u64],
    mic_segments: &[diarize::Segment],
) -> (Vec<diarize::Segment>, Vec<UnifyTakeEcho<'a>>) {
    let takes: Vec<crate::echo::JoinedTake> = echoes
        .iter()
        .zip(offsets)
        .map(|(echo, &start_ms)| crate::echo::JoinedTake {
            start_ms,
            analysis: echo.analysis.as_ref(),
        })
        .collect();
    let (kept, reports) = crate::echo::check_joined(mic_segments, &takes);
    let dump = mic_takes
        .iter()
        .zip(reports)
        .filter_map(|(take, report)| {
            let echo = report?;
            if let Some(check) = &echo.check {
                log_echo_check(check);
            }
            Some(UnifyTakeEcho { session_id: &take.entry.id, index: take.entry.index, echo })
        })
        .collect();
    (kept, dump)
}

/// `diagnostics/<note_id>/<engine>-unify.json`. Holds no `chunks`, so
/// `read_chunks_from_diagnostic` passes over it. Best-effort.
async fn write_unify_dump(
    app: &AppHandle,
    note_id: &str,
    engine: diarize::Engine,
    takes: &[UnifyTakeEcho<'_>],
) {
    let Ok(app_dir) = app.path().app_data_dir() else { return };
    let dir = app_dir.join("diagnostics").join(note_id);
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        eprintln!("diagnostics: mkdir {}: {e}", dir.display());
        return;
    }
    let path = dir.join(format!("{}-unify.json", engine.arg()));
    let payload = serde_json::json!({
        "engine": engine.arg(),
        "source": "unify",
        "takes": takes,
        "created_at": chrono::Utc::now().timestamp_millis(),
    });
    match serde_json::to_string_pretty(&payload) {
        Ok(json) => {
            if let Err(e) = tokio::fs::write(&path, json).await {
                eprintln!("diagnostics: write {}: {e}", path.display());
            }
        }
        Err(e) => eprintln!("diagnostics: serialize: {e}"),
    }
}

#[cfg(test)]
mod unify_concurrency_tests {
    use super::*;

    #[test]
    fn scratch_dir_is_unique_per_invocation() {
        // Two unify passes for the SAME note must get distinct scratch dirs, or
        // one's remove_dir_all truncates the other's concat WAVs mid-read →
        // wrong unified labels written to every session.
        let a = unify_scratch_dir("note1");
        let b = unify_scratch_dir("note1");
        assert_ne!(a, b);
        assert!(a.file_name().unwrap().to_string_lossy().contains("note1"));
        assert!(a.starts_with(std::env::temp_dir()));
    }

    #[test]
    fn note_lock_shared_per_note_distinct_across_notes() {
        let locks = parking_lot::Mutex::new(std::collections::HashMap::new());
        let a1 = unify_note_lock(&locks, "n1");
        let a2 = unify_note_lock(&locks, "n1");
        let b = unify_note_lock(&locks, "n2");
        // Same note → same lock (so passes serialize); different note → its own.
        assert!(Arc::ptr_eq(&a1, &a2));
        assert!(!Arc::ptr_eq(&a1, &b));
    }

    #[test]
    fn note_lock_serializes_same_note() {
        // A held lock forces a second unify for the same note to wait; a
        // different note is never blocked.
        let locks = parking_lot::Mutex::new(std::collections::HashMap::new());
        let held = unify_note_lock(&locks, "n1").try_lock_owned().unwrap();
        let same = unify_note_lock(&locks, "n1");
        assert!(same.try_lock().is_err(), "second unify for the note must wait");
        assert!(
            unify_note_lock(&locks, "n2").try_lock().is_ok(),
            "a different note stays concurrent"
        );
        drop(held);
        assert!(same.try_lock().is_ok(), "lock frees once the first pass ends");
    }
}

#[cfg(test)]
mod unify_tests {
    use super::*;
    use crate::diarize::Segment;

    fn seg(start_ms: u64, end_ms: u64, sid: &str) -> Segment {
        Segment { start_ms, end_ms, speaker_id: sid.to_string() }
    }

    fn mic(start_ms: u64, text: &str) -> ChunkRecord {
        ChunkRecord {
            source: ChunkSource::Mic,
            start_ms,
            text: text.to_string(),
            words: Vec::new(),
            detected_language: None,
        }
    }

    fn sys(start_ms: u64, text: &str) -> ChunkRecord {
        ChunkRecord {
            source: ChunkSource::Sys,
            start_ms,
            text: text.to_string(),
            words: Vec::new(),
            detected_language: None,
        }
    }

    fn span(start_ms: u64, end_ms: u64, label: &str, source: Option<ChunkSource>) -> LabelSpan {
        LabelSpan { start_ms, end_ms, label: label.to_string(), source }
    }

    fn usess(
        id: &str,
        mode: SessionMode,
        chunks: Vec<ChunkRecord>,
        mic_offset_ms: u64,
        sys_offset_ms: u64,
        old_spans: Vec<LabelSpan>,
    ) -> UnifySession {
        UnifySession {
            session_id: id.to_string(),
            mode,
            chunks,
            mic_offset_ms,
            sys_offset_ms,
            old_spans,
        }
    }

    /// (label, text) per entry of a timeline JSONL string.
    fn entries_of(jsonl: &str) -> Vec<(String, String)> {
        jsonl
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                let v: serde_json::Value = serde_json::from_str(l).unwrap();
                (
                    v.get("label").and_then(|s| s.as_str()).unwrap().to_string(),
                    v.get("text").and_then(|s| s.as_str()).unwrap().to_string(),
                )
            })
            .collect()
    }

    /// Parse a timeline JSONL back into label spans, mirroring what the
    /// orchestrator reads off disk on the next run.
    fn spans_of(jsonl: &str) -> Vec<LabelSpan> {
        let values: Vec<serde_json::Value> = jsonl
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        spans_from_values(&values)
    }

    #[test]
    fn concat_offsets_accumulate_from_sample_counts() {
        // 16 kHz mono → 16 samples/ms. 16000 samples = 1000 ms.
        assert_eq!(concat_offsets_ms(&[16_000, 8_000, 4_000]), vec![0, 1_000, 1_500]);
        assert_eq!(concat_offsets_ms(&[]), Vec::<u64>::new());
        assert_eq!(concat_offsets_ms(&[500]), vec![0]);
    }

    #[test]
    fn session_mode_reflects_chunk_sources() {
        assert_eq!(session_mode(&[mic(0, "a")]), Some(SessionMode::MicOnly));
        assert_eq!(session_mode(&[sys(0, "a")]), Some(SessionMode::SysOnly));
        assert_eq!(
            session_mode(&[mic(0, "a"), sys(1, "b")]),
            Some(SessionMode::Hybrid)
        );
        assert_eq!(session_mode(&[]), None);
    }

    #[test]
    fn session_unifiable_requires_every_diarized_stream() {
        // Mic-only diarizes mic.wav; sys-only diarizes sys.wav; hybrid now
        // diarizes BOTH, so it needs both on disk. A hybrid session missing one
        // can't join the concat — `concat_wavs` fails the whole pass on an
        // unreadable input, and skipping it would misalign later sessions'
        // offsets — so it stays frozen with its existing labels.
        assert!(session_unifiable(SessionMode::MicOnly, true, false));
        assert!(!session_unifiable(SessionMode::MicOnly, false, true));
        assert!(session_unifiable(SessionMode::SysOnly, false, true));
        assert!(!session_unifiable(SessionMode::SysOnly, true, false));
        assert!(session_unifiable(SessionMode::Hybrid, true, true));
        assert!(!session_unifiable(SessionMode::Hybrid, false, true));
        assert!(!session_unifiable(SessionMode::Hybrid, true, false));
    }

    #[test]
    fn generated_label_detection() {
        assert!(is_generated_label("Speaker 1"));
        assert!(is_generated_label("Speaker 12"));
        assert!(is_generated_label("You"));
        assert!(is_generated_label(""));
        assert!(!is_generated_label("Michael"));
        assert!(!is_generated_label("Speaker"));
        assert!(!is_generated_label("Speaker x"));
        assert!(!is_generated_label("Speaker 1 (guest)"));
    }

    /// Two mic-only takes of the same 2-person conversation. The combined
    /// clustering sees voice A in both takes → ONE label across sessions
    /// (the bug this issue fixes: per-take clustering gave take 2's voice A
    /// a fresh number).
    #[test]
    fn same_voice_across_sessions_gets_one_label() {
        // Take 1 is 10 s (offset 0), take 2 starts at 10 000 in concat time.
        let s1 = usess(
            "a",
            SessionMode::MicOnly,
            vec![mic(0, "hello from voice one."), mic(6_000, "reply from voice two.")],
            0,
            0,
            Vec::new(),
        );
        let s2 = usess(
            "b",
            SessionMode::MicOnly,
            vec![mic(1_000, "voice one again in take two.")],
            10_000,
            0,
            Vec::new(),
        );
        // Combined-timeline segments: spk_a talks 0–5 s and 10.5–15 s,
        // spk_b talks 6–10 s.
        let segs = vec![
            seg(0, 5_000, "spk_a"),
            seg(6_000, 10_000, "spk_b"),
            seg(10_500, 15_000, "spk_a"),
        ];
        let out = unify_relabel(&[s1, s2], &segs, &[], 0);
        assert_eq!(out.timelines.len(), 2);
        assert!(out.notices.is_empty());

        let t1 = entries_of(&out.timelines[0].1);
        let t2 = entries_of(&out.timelines[1].1);
        assert_eq!(t1[0].0, "Speaker 1");
        assert_eq!(t1[1].0, "Speaker 2");
        // Take 2's chunk is voice A → same "Speaker 1", NOT "Speaker 3".
        assert_eq!(t2[0].0, "Speaker 1");

        // Times stay session-local: take 2's entry starts at 1000, not 11000.
        let v: serde_json::Value = serde_json::from_str(
            out.timelines[1].1.lines().next().unwrap(),
        )
        .unwrap();
        assert_eq!(v.get("start_ms").and_then(|s| s.as_u64()), Some(1_000));
    }

    #[test]
    fn hybrid_mic_is_diarized_and_unified_with_the_in_person_take() {
        // Take 1 in-person (mic-only), take 2 a remote call (hybrid). The
        // hybrid take's mic used to be hard-labelled `You`, which hid the fact
        // that it's the same person who spoke in take 1. Diarizing it means the
        // unify pass can recognise the voice and give both takes one label.
        // Numbering stays one sequence across streams in reading order.
        let s1 = usess(
            "a",
            SessionMode::MicOnly,
            vec![mic(0, "in person voice.")],
            0,
            0,
            Vec::new(),
        );
        let s2 = usess(
            "b",
            SessionMode::Hybrid,
            vec![mic(0, "me on the call."), sys(2_000, "remote person answering.")],
            0,
            0,
            Vec::new(),
        );
        let mic_segs = vec![seg(0, 5_000, "spk_m")];
        let sys_segs = vec![seg(1_500, 6_000, "spk_r")];
        let out = unify_relabel(&[s1, s2], &mic_segs, &sys_segs, 0);

        let t1 = entries_of(&out.timelines[0].1);
        let t2 = entries_of(&out.timelines[1].1);
        assert_eq!(t1[0].0, "Speaker 1");
        assert_eq!(
            t2[0].0, "Speaker 1",
            "one voice across both takes — not `You` on a note with an in-person take"
        );
        assert_eq!(t2[1].0, "Speaker 2");
    }

    #[test]
    fn all_hybrid_takes_with_a_lone_mic_voice_keep_you() {
        // Every take is a remote call and the combined mic stream is one voice:
        // that's the shape `You` was built for, so it survives the unify pass
        // and the remote side still numbers from 1.
        let s1 = usess(
            "a",
            SessionMode::Hybrid,
            vec![mic(0, "me first."), sys(2_000, "them first.")],
            0,
            0,
            Vec::new(),
        );
        let s2 = usess(
            "b",
            SessionMode::Hybrid,
            vec![mic(0, "me again."), sys(2_000, "them again.")],
            0,
            0,
            Vec::new(),
        );
        let mic_segs = vec![seg(0, 5_000, "spk_me")];
        let sys_segs = vec![seg(1_500, 6_000, "spk_them")];
        let out = unify_relabel(&[s1, s2], &mic_segs, &sys_segs, 0);

        let t1 = entries_of(&out.timelines[0].1);
        let t2 = entries_of(&out.timelines[1].1);
        assert_eq!(t1[0].0, "You");
        assert_eq!(t1[1].0, "Speaker 1", "`You` doesn't consume a number");
        assert_eq!(t2[0].0, "You");
        assert_eq!(t2[1].0, "Speaker 1");
    }

    #[test]
    fn hybrid_takes_with_several_mic_voices_number_the_room() {
        // The multi-session form of the in-person regression: a hybrid take
        // whose mic holds more than one person must not collapse them onto
        // `You`, even though every take is hybrid.
        let s1 = usess(
            "a",
            SessionMode::Hybrid,
            vec![
                mic(0, "first colleague."),
                mic(6_000, "second colleague."),
                sys(12_000, "remote voice."),
            ],
            0,
            0,
            Vec::new(),
        );
        let mic_segs = vec![seg(0, 4_000, "spk_a"), seg(5_000, 9_000, "spk_b")];
        let sys_segs = vec![seg(11_000, 15_000, "spk_r")];
        let out = unify_relabel(&[s1], &mic_segs, &sys_segs, 0);

        let t = entries_of(&out.timelines[0].1);
        assert_eq!(t[0].0, "Speaker 1");
        assert_eq!(t[1].0, "Speaker 2");
        assert_eq!(t[2].0, "Speaker 3");
        assert!(
            !t.iter().any(|e| e.0 == "You"),
            "two people sharing the mic are not one `You`"
        );
    }

    #[test]
    fn timeline_entries_carry_source() {
        let s = usess(
            "a",
            SessionMode::Hybrid,
            vec![mic(0, "me talking."), sys(3_000, "remote talking.")],
            0,
            0,
            Vec::new(),
        );
        let sys_segs = vec![seg(2_500, 6_000, "spk_r")];
        let out = unify_relabel(&[s], &[], &sys_segs, 0);
        let spans = spans_of(&out.timelines[0].1);
        assert_eq!(spans[0].source, Some(ChunkSource::Mic));
        assert_eq!(spans[1].source, Some(ChunkSource::Sys));
    }

    /// A user rename ("Michael") on take 1's cluster survives unification and
    /// spreads to take 2's chunks of the same voice.
    #[test]
    fn custom_rename_carries_onto_unified_cluster() {
        let s1 = usess(
            "a",
            SessionMode::MicOnly,
            vec![mic(0, "hello from michael.")],
            0,
            0,
            // Old timeline: the user renamed this span to "Michael".
            vec![span(0, 1_200, "Michael", Some(ChunkSource::Mic))],
        );
        let s2 = usess(
            "b",
            SessionMode::MicOnly,
            vec![mic(500, "michael again in take two.")],
            8_000,
            0,
            // Take 2 previously carried a generated (offset) label.
            vec![span(500, 2_000, "Speaker 2", Some(ChunkSource::Mic))],
        );
        let segs = vec![seg(0, 5_000, "spk_a"), seg(8_000, 12_000, "spk_a")];
        let out = unify_relabel(&[s1, s2], &segs, &[], 0);
        assert!(out.notices.is_empty());
        assert_eq!(entries_of(&out.timelines[0].1)[0].0, "Michael");
        assert_eq!(entries_of(&out.timelines[1].1)[0].0, "Michael");
    }

    /// Two different custom names collapse into one cluster: the one with
    /// more speech time wins and the merge is surfaced as a notice.
    #[test]
    fn custom_name_collision_keeps_longer_and_notices() {
        let s1 = usess(
            "a",
            SessionMode::MicOnly,
            vec![mic(0, "a long stretch of alice talking here.")],
            0,
            0,
            // ~2.4 s of "Alice".
            vec![span(0, 2_400, "Alice", Some(ChunkSource::Mic))],
        );
        let s2 = usess(
            "b",
            SessionMode::MicOnly,
            vec![mic(0, "short bob bit.")],
            10_000,
            0,
            // ~1.0 s of "Bob".
            vec![span(0, 1_000, "Bob", Some(ChunkSource::Mic))],
        );
        // One combined cluster spans both takes.
        let segs = vec![seg(0, 15_000, "spk_a")];
        let out = unify_relabel(&[s1, s2], &segs, &[], 0);
        assert_eq!(entries_of(&out.timelines[0].1)[0].0, "Alice");
        assert_eq!(entries_of(&out.timelines[1].1)[0].0, "Alice");
        assert_eq!(out.notices.len(), 1);
        assert!(out.notices[0].contains("Alice"), "{}", out.notices[0]);
        assert!(out.notices[0].contains("Bob"), "{}", out.notices[0]);
    }

    /// "You" is pipeline-generated, not a user rename — a hybrid take's old
    /// "You" entries must not rename a time-overlapping remote cluster.
    #[test]
    fn you_label_does_not_rename_sys_clusters() {
        let s = usess(
            "a",
            SessionMode::Hybrid,
            vec![mic(0, "me talking over them."), sys(200, "remote person talking.")],
            0,
            0,
            vec![
                span(0, 2_000, "You", Some(ChunkSource::Mic)),
                span(200, 2_200, "Speaker 1", Some(ChunkSource::Sys)),
            ],
        );
        let sys_segs = vec![seg(0, 6_000, "spk_r")];
        let out = unify_relabel(&[s], &[], &sys_segs, 0);
        let t = entries_of(&out.timelines[0].1);
        assert_eq!(t[0].0, "You");
        // Remote cluster keeps a generated number — not "You".
        assert_eq!(t[1].0, "Speaker 1");
    }

    /// A rename on the user's own line ("You" → "Michael") must follow the
    /// mic stream only; the time-overlapping remote cluster keeps its number.
    #[test]
    fn same_source_guard_stops_cross_stream_rename_leak() {
        let s = usess(
            "a",
            SessionMode::Hybrid,
            vec![mic(0, "me the renamed user."), sys(100, "remote person here.")],
            0,
            0,
            vec![
                // User renamed their own mic line to "Michael"; sys entry
                // overlaps it in wall time (crosstalk) but is another stream.
                span(0, 2_000, "Michael", Some(ChunkSource::Mic)),
                span(100, 2_100, "Speaker 1", Some(ChunkSource::Sys)),
            ],
        );
        let sys_segs = vec![seg(0, 6_000, "spk_r")];
        let out = unify_relabel(&[s], &[], &sys_segs, 0);
        let t = entries_of(&out.timelines[0].1);
        // Mic side: generated "You" inherits the user's rename.
        assert_eq!(t[0].0, "Michael");
        // Sys side: unaffected by the mic-stream rename.
        assert_eq!(t[1].0, "Speaker 1");
    }

    /// Old timelines written before the `source` field existed still carry
    /// renames (source unknown → match any stream).
    #[test]
    fn sourceless_old_spans_still_carry_renames() {
        let s1 = usess(
            "a",
            SessionMode::MicOnly,
            vec![mic(0, "hello from wilma.")],
            0,
            0,
            vec![span(0, 1_200, "Wilma", None)],
        );
        let s2 = usess(
            "b",
            SessionMode::MicOnly,
            vec![mic(0, "wilma again.")],
            5_000,
            0,
            Vec::new(),
        );
        let segs = vec![seg(0, 10_000, "spk_a")];
        let out = unify_relabel(&[s1, s2], &segs, &[], 0);
        assert_eq!(entries_of(&out.timelines[0].1)[0].0, "Wilma");
        assert_eq!(entries_of(&out.timelines[1].1)[0].0, "Wilma");
    }

    /// Frozen (non-unifiable) sessions keep their numbers; unified generated
    /// labels are offset past them. Custom names are never offset.
    #[test]
    fn label_offset_bumps_generated_numbers_past_frozen() {
        let s1 = usess(
            "a",
            SessionMode::MicOnly,
            vec![mic(0, "voice one talking."), mic(6_000, "voice two talking.")],
            0,
            0,
            Vec::new(),
        );
        let s2 = usess(
            "b",
            SessionMode::MicOnly,
            vec![mic(0, "voice one again.")],
            10_000,
            0,
            Vec::new(),
        );
        let segs = vec![
            seg(0, 5_000, "spk_a"),
            seg(6_000, 10_000, "spk_b"),
            seg(10_000, 14_000, "spk_a"),
        ];
        // A frozen first take already used Speaker 1 + Speaker 2.
        let out = unify_relabel(&[s1, s2], &segs, &[], 2);
        let t1 = entries_of(&out.timelines[0].1);
        let t2 = entries_of(&out.timelines[1].1);
        assert_eq!(t1[0].0, "Speaker 3");
        assert_eq!(t1[1].0, "Speaker 4");
        assert_eq!(t2[0].0, "Speaker 3");
    }

    /// Re-running the pass with its own output as the "old" timelines must
    /// reproduce byte-identical timelines (and stop re-noticing an already-
    /// resolved collision) — idempotency by construction.
    #[test]
    fn rerun_with_own_output_is_identical() {
        let chunks1 = vec![mic(0, "a long stretch of alice talking here.")];
        let chunks2 = vec![mic(0, "short bob bit."), mic(4_000, "second voice appears.")];
        let segs = vec![seg(0, 12_000, "spk_a"), seg(14_000, 20_000, "spk_b")];
        let make = |old1: Vec<LabelSpan>, old2: Vec<LabelSpan>| {
            unify_relabel(
                &[
                    usess("a", SessionMode::MicOnly, chunks1.clone(), 0, 0, old1),
                    usess("b", SessionMode::MicOnly, chunks2.clone(), 10_000, 0, old2),
                ],
                &segs,
                &[],
                0,
            )
        };
        // Run 1: collision between two customs → "Alice" wins, notice fires.
        let run1 = make(
            vec![span(0, 2_400, "Alice", Some(ChunkSource::Mic))],
            vec![span(0, 1_000, "Bob", Some(ChunkSource::Mic))],
        );
        assert_eq!(run1.notices.len(), 1);

        // Run 2: old = run 1's output. Same clustering → identical bytes,
        // and the collision is already resolved so no notice repeats.
        let run2 = make(spans_of(&run1.timelines[0].1), spans_of(&run1.timelines[1].1));
        assert_eq!(run1.timelines, run2.timelines);
        assert!(run2.notices.is_empty());

        // Run 3 keeps the fixpoint.
        let run3 = make(spans_of(&run2.timelines[0].1), spans_of(&run2.timelines[1].1));
        assert_eq!(run2.timelines, run3.timelines);
    }

    /// No-custom-names path is also a fixpoint from the first run.
    #[test]
    fn rerun_without_customs_is_identical() {
        let chunks1 = vec![mic(0, "voice one talking."), mic(6_000, "voice two talking.")];
        let chunks2 = vec![mic(1_000, "voice one again.")];
        let segs = vec![
            seg(0, 5_000, "spk_a"),
            seg(6_000, 10_000, "spk_b"),
            seg(10_500, 15_000, "spk_a"),
        ];
        let make = |old1: Vec<LabelSpan>, old2: Vec<LabelSpan>| {
            unify_relabel(
                &[
                    usess("a", SessionMode::MicOnly, chunks1.clone(), 0, 0, old1),
                    usess("b", SessionMode::MicOnly, chunks2.clone(), 10_000, 0, old2),
                ],
                &segs,
                &[],
                0,
            )
        };
        let run1 = make(Vec::new(), Vec::new());
        let run2 = make(spans_of(&run1.timelines[0].1), spans_of(&run1.timelines[1].1));
        assert_eq!(run1.timelines, run2.timelines);
        assert!(run2.notices.is_empty());
    }

    /// An unlabelled piece (empty label) must never gain a name through
    /// overlap with a custom span.
    #[test]
    fn empty_labels_never_gain_names() {
        let news = vec![vec![span(0, 2_000, "", Some(ChunkSource::Mic))]];
        let olds = vec![vec![span(0, 2_000, "Michael", Some(ChunkSource::Mic))]];
        let (map, notices) = custom_name_map(&news, &olds);
        assert!(map.is_empty());
        assert!(notices.is_empty());
    }

    #[test]
    fn max_speaker_in_timeline_reads_exact_labels() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("timeline.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"start_ms":0,"end_ms":1,"label":"Speaker 2","text":"a"}"#,
                "\n",
                r#"{"start_ms":1,"end_ms":2,"label":"Michael","text":"b"}"#,
                "\n",
                r#"{"start_ms":2,"end_ms":3,"label":"You","text":"c"}"#,
                "\n",
            ),
        )
        .unwrap();
        assert_eq!(max_speaker_in_timeline(&path), 2);
        assert_eq!(max_speaker_in_timeline(&tmp.path().join("absent.jsonl")), 0);
    }

    fn candidate(id: &str, index: u32, mode: SessionMode) -> UnifyCandidate {
        UnifyCandidate {
            entry: sessions::SessionEntry {
                id: id.to_string(),
                index,
                started_at: String::new(),
                duration_ms: 60_000,
                streams: Vec::new(),
                transcribed: true,
            },
            dir: PathBuf::new(),
            chunks: Vec::new(),
            mode,
        }
    }

    #[test]
    fn the_unify_pass_judges_each_hybrid_take_inside_its_own_span_and_never_a_mic_only_one() {
        // Take 1 (0–60 s) was in person, with Hege at the laptop. Take 2
        // (60–120 s) was a call on the speakers with Hege remote, so the mic
        // heard her only while sys sounded.
        let in_person = candidate("s1", 1, SessionMode::MicOnly);
        let call = candidate("s2", 2, SessionMode::Hybrid);
        let echoes = [
            EchoPass { analysis: None, cancelled: None, ms: None },
            EchoPass {
                analysis: Some(crate::echo::tests::echoed_take(60.0, &[(0.0, 20.0), (30.0, 50.0)])),
                cancelled: None,
                ms: Some(0),
            },
        ];
        let segments = vec![
            seg(1_000, 15_000, "HEGE"),
            seg(21_000, 29_000, "ME"),
            seg(61_000, 75_000, "HEGE"),
            seg(81_000, 89_000, "ME"),
            seg(92_000, 108_000, "HEGE"),
            seg(112_000, 118_000, "ME"),
        ];

        let (kept, dump) =
            drop_joined_echo(&[&in_person, &call], &echoes, &[0, 60_000], &segments);

        let hege: Vec<(u64, u64)> = kept
            .iter()
            .filter(|s| s.speaker_id == "HEGE")
            .map(|s| (s.start_ms, s.end_ms))
            .collect();
        assert_eq!(hege, vec![(1_000, 15_000)], "her echo goes, and only her echo");
        assert_eq!(kept.iter().filter(|s| s.speaker_id == "ME").count(), 3);
        assert_eq!(dump.len(), 1, "a mic-only take has no echo pass to record");
        assert_eq!((dump[0].session_id, dump[0].index), ("s2", 2));
        let check = dump[0].echo.check.as_ref().expect("the call was judged");
        assert!(check.voices.iter().any(|v| v.speaker_id == "HEGE" && v.dropped), "{check:?}");
    }
}
