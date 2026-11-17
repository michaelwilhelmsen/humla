use crate::db;
use crate::diarize;
use crate::sessions;
use crate::recording::{ChunkRecord, ChunkSource, ErrorPayload, Phase, RecordingStatus, ReplayProgress, Step, StepReport};
use crate::AppState;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};

// Command handlers are split into submodules under `commands/`. Each holds a
// cohesive group of #[tauri::command] fns and is re-exported with `pub use`
// so the `commands::<name>` paths in lib.rs's generate_handler! stay unchanged;
// a module that holds no commands isn't re-exported.
// Shared helpers + constants (err, DEFAULT_*, emit_*, key/config readers) stay
// in this root module and are reached from submodules via `super::` / `crate::`.
mod api_keys;
mod assets;
mod capture;
mod chat;
mod clients;
mod copilot;
mod deferred;
mod diarization;
// `pub(crate)` so `crate::menubar` can resolve the active workspace when it
// creates a note for a headless recording.
pub(crate) mod cloud;
pub(crate) mod cloud_auth;
mod export;
// Live cloud-sync worker glue. Behind the `cloud` feature; `pub` so `run()` can
// reach `cloud_worker::install`. Not a #[tauri::command] group, so no glob
// re-export below.
#[cfg(feature = "cloud")]
pub mod cloud_worker;
mod folders;
mod import;
mod local_llm;
mod mcp;
mod menubar;
mod models;
pub(crate) mod notes;
mod permissions;
mod playback;
mod settings;
mod summary;
mod summary_prompts;
mod telemetry;
mod timeline;
mod title;
mod transcribe;
mod transcript;
mod transcription_config;
mod unify;
mod webhook;
pub use api_keys::*;
pub use assets::*;
pub use capture::*;
pub use chat::*;
pub use clients::*;
pub use copilot::*;
pub use deferred::*;
pub use diarization::*;
pub use cloud::*;
pub use cloud_auth::*;
pub use export::*;
pub use folders::*;
pub use import::*;
pub use local_llm::*;
pub use mcp::*;
pub use menubar::*;
pub use models::*;
pub use notes::*;
pub use permissions::*;
pub use settings::*;
pub use summary::*;
pub use telemetry::*;
pub use timeline::*;
pub use title::*;
pub use summary_prompts::*;
pub use transcription_config::*;
pub use webhook::*;

// Shared helpers that moved into submodules but are still called by the
// code remaining in this root module, or by siblings through `super::`.
use api_keys::read_provider_api_key;
use transcript::{
    absorb_text_continuation_chains, bridge_short_interjections, dedup_mic_against_sys,
    estimated_speech_ms, LabelledPiece,
};
use diarization::offset_speaker_label;

pub(crate) const DEFAULT_LANGUAGE: &str = "no";
// The diarization engine when `diarize_model` is unset. Only a fresh install
// has no row, since `db::migrate_diarize_engine` writes one for every other.
// Mirrored by `DEFAULT_DIARIZE_ENGINE` in `src/lib/diarizeEngine.ts`.
const DEFAULT_DIARIZE_MODEL: &str = "nemotron3";

// Matches the sidecar's own default. Stored as a string and parsed at use.
const DEFAULT_COMMUNITY1_THRESHOLD: &str = "0.5";

// Silent-chunk gate. RMS below this is dropped before transcription so
// Whisper / gpt-4o-transcribe don't hallucinate confident text on silence.
// Was 0.008 originally — too aggressive for users sitting >40 cm from a
// MacBook mic at default gain (their soft speech lands at ~0.007). 0.005
// still rejects pure silence (~0.0001), room tone (~0.001), and mic hiss
// (~0.003) while admitting normal soft speech. Tunable via the
// silence_rms_threshold setting for noisy environments.
const DEFAULT_SILENCE_RMS_THRESHOLD: f32 = 0.005;

// `keep_audio` defaults off. Its value is read through
// `sessions::retain_audio` (which owns the default) via `keep_audio_enabled`;
// off means recordings live in the temp dir for the post-stop pipeline and are
// deleted at the end, with no playback.wav and no source copies left behind.

// Bounded backlog for the import reader loop. An `--import` replay emits chunks
// at full disk speed — far faster than realtime — so without a bound the reader
// would spawn a transcribe task per chunk and a multi-hour file would pile up
// hundreds of parked tasks doing eager pre-gate work (silence RMS read, config
// lookups) while they queue behind `transcribe_gate`. The reader acquires a
// permit before spawning each chunk's task and the task releases it on
// completion, capping the parked backlog. Transcription quality is unchanged —
// the tasks still serialise on `transcribe_gate` exactly as live recording does.
const IMPORT_BACKLOG_PERMITS: usize = 16;

const DEFAULT_SUMMARY_MODEL: &str = "gpt-5.4-mini";
/// The cloud model an automatic title call falls back to when the configured
/// summary model reasons (#90) — which the default above does, so this is the
/// model the default configuration actually titles with.
///
/// Pinned rather than derived: a reasoning model would spend thousands of
/// invisible reasoning tokens on five words, in a call the user never asked
/// for. It has to be a model every OpenAI key can reach, since a failure here
/// is silent by design and would present as titling simply never happening.
pub(crate) const TITLE_FALLBACK_MODEL: &str = "gpt-4o-mini";
// Ollama's default port + OpenAI-compat path. Any user running LM Studio,
// llama-server, or vLLM will override this in Settings.
const DEFAULT_LOCAL_LLM_BASE_URL: &str = "http://localhost:11434/v1";
fn err<E: std::fmt::Display>(e: E) -> String { e.to_string() }

/// A channel one chain's progress goes on.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Channel {
    /// The global `recording_status`.
    RecordingStatus,
    /// The per-note `diarize_status`.
    DiarizeStatus,
    /// The per-note `transcribe_status`.
    TranscribeStatus,
    /// None.
    None,
}

/// Where one chain reports — both the bracket saying it is in flight and the
/// named steps inside it.
///
/// Passed in the way [`StopClock`] is: the diarize pass and the unify pass are
/// shared by the live stop, the user-pressed Re-diarize and a deferred replay,
/// so a callee must never choose the channel. Bracket and steps live on one
/// type because they are one answer — held apart, a caller can bracket on one
/// channel and name steps on another.
///
/// `recording_status` describes the live capture and nothing else (#187): it is
/// a single global slot that also drives the tray icon and its Start/Stop
/// items, so a pass the user can start on an *arbitrary* note must stay off it —
/// its `Idle` would blank a recording running on a different note.
#[derive(Clone)]
pub(crate) enum ChainProgress {
    /// The post-stop chain of the capture that just finished, on the global
    /// phase it already holds.
    PostStop { note_id: String },
    /// The user pressed Re-diarize: bracketed per-note, with no measure.
    Rediarize { note_id: String },
    /// A deferred replay, on the per-note channel where the run's audio
    /// position and take counter already ride. Unbracketed — the command
    /// brackets the whole run.
    Replay {
        note_id: String,
        progress: Arc<parking_lot::Mutex<ReplayProgress>>,
    },
    /// Nothing. A deferred stop dispatched no chunks and lands on idle in a
    /// few hundred milliseconds, so a bar drawn for it appears and vanishes
    /// without having measured anything.
    Silent,
}

impl ChainProgress {
    /// The one channel this chain reports on, bracket and steps alike.
    #[cfg(test)]
    pub(crate) fn channel(&self) -> Channel {
        match self {
            Self::PostStop { .. } => Channel::RecordingStatus,
            Self::Rediarize { .. } => Channel::DiarizeStatus,
            Self::Replay { .. } => Channel::TranscribeStatus,
            Self::Silent => Channel::None,
        }
    }

    /// The pass is in flight.
    fn started(&self, app: &AppHandle) {
        match self {
            Self::PostStop { note_id } => emit_status(app, Some(note_id), Phase::Diarizing),
            Self::Rediarize { note_id } => emit_diarize_status(app, note_id, true),
            Self::Replay { .. } | Self::Silent => {}
        }
    }

    /// Clear it again, on every exit path.
    fn finished(&self, app: &AppHandle) {
        match self {
            Self::PostStop { .. } => emit_status(app, None, Phase::Idle),
            Self::Rediarize { note_id } => emit_diarize_status(app, note_id, false),
            Self::Replay { .. } | Self::Silent => {}
        }
    }

    fn report(&self, app: &AppHandle, r: StepReport) {
        match self {
            Self::PostStop { note_id } => {
                let _ = app.emit(
                    "recording_status",
                    RecordingStatus::chain_step(note_id, r.step, r.counter),
                );
            }
            Self::Replay { note_id, progress } => {
                let snapshot = {
                    let mut p = progress.lock();
                    p.begin_step(r.step, r.counter);
                    p.snapshot()
                };
                emit_transcribe_progress(app, note_id, snapshot);
            }
            Self::Rediarize { .. } | Self::Silent => {}
        }
    }

    fn step(&self, app: &AppHandle, step: Step) {
        self.report(app, StepReport::plain(step));
    }
}

/// Names one step over the per-source streams a pass actually has.
struct StreamSteps<'a> {
    app: &'a AppHandle,
    progress: &'a ChainProgress,
    reports: std::vec::IntoIter<StepReport>,
}

impl<'a> StreamSteps<'a> {
    /// `present` is the streams in the order the caller walks them. The count
    /// comes from what is there, never from a literal — see
    /// [`crate::recording::stream_reports`].
    fn new(
        app: &'a AppHandle,
        progress: &'a ChainProgress,
        step: Step,
        present: &[bool],
    ) -> Self {
        let streams = present.iter().filter(|p| **p).count() as u32;
        Self {
            app,
            progress,
            reports: crate::recording::stream_reports(step, streams).into_iter(),
        }
    }

    /// The next present stream is starting. Runs out rather than counting past
    /// the streams it was given.
    fn begin(&mut self) {
        if let Some(r) = self.reports.next() {
            self.progress.report(self.app, r);
        }
    }
}

// ---- Speaker diarization model management ---------------------------------

/// The diarization engine selected in Settings.
pub(crate) fn selected_diarize_engine(state: &State<AppState>) -> diarize::Engine {
    let conn = state.db.lock();
    let id = db::get_setting(&conn, "diarize_model")
        .ok()
        .flatten()
        .unwrap_or_else(|| DEFAULT_DIARIZE_MODEL.to_string());
    diarize::Engine::from_setting(&id)
}

fn raw_setting(app: &AppHandle, key: &str) -> Option<String> {
    let state: State<AppState> = app.state();
    let conn = state.db.lock();
    db::get_setting(&conn, key).ok().flatten()
}

/// Read the device's `keep_audio` decision (#24). The single gate consulted by
/// every path that would write, upload or download a WAV.
pub(crate) fn keep_audio_enabled(app: &AppHandle) -> bool {
    sessions::retain_audio(raw_setting(app, "keep_audio").as_deref())
}

/// Whether a capture starting now should keep the Mac from idle sleep (#180).
/// Read once at `recording_start` and handed to the sidecar as a launch flag;
/// flipping the setting mid-recording leaves the running capture alone.
pub(crate) fn keep_awake_enabled(app: &AppHandle) -> bool {
    sessions::keep_awake(raw_setting(app, "keep_awake").as_deref())
}

/// Whether a recording starting now should defer transcription (#146). Reads
/// both halves of the decision — see [`sessions::defer_transcription`] for why
/// `keep_audio` gates it.
pub(crate) fn deferred_transcription_enabled(app: &AppHandle) -> bool {
    let manual = raw_setting(app, "transcribe_manually");
    let keep = raw_setting(app, "keep_audio");
    sessions::defer_transcription(manual.as_deref(), sessions::retain_audio(keep.as_deref()))
}

/// How far the run has got, on the same per-note channel. Measured in audio
/// position (see [`crate::recording::ReplayProgress`]) because a replay's chunk
/// count isn't known until it ends. No rate limit: chunks are seconds apart.
fn emit_transcribe_progress(
    app: &AppHandle,
    note_id: &str,
    snapshot: crate::recording::ReplaySnapshot,
) {
    let _ = app.emit(
        "transcribe_status",
        crate::recording::TranscribeStatusPayload::progress(note_id, snapshot),
    );
}

/// Serialise the chunk log as a per-chunk JSONL — one entry per
/// utterance, not per speaker turn. The saved transcript still groups
/// same-label runs (for summary readability), but the playback view
/// wants finer granularity: each ~5–15 s VAD chunk is the natural
/// click-to-seek and highlight unit, so glueing them by label hides
/// internal sentence boundaries from the player and makes the active-
/// turn highlight feel sluggish on long monologues. Runs dedup first
/// so echoed mic chunks don't generate phantom entries.
fn serialize_timeline(
    chunks: &[ChunkRecord],
    split_chunk: &dyn Fn(&ChunkRecord) -> Vec<LabelledPiece>,
    label_offset: u32,
) -> String {
    let kept = dedup_mic_against_sys(chunks);
    let mut sorted: Vec<&ChunkRecord> = kept.iter().collect();
    sorted.sort_by_key(|c| {
        let source_rank = match c.source {
            ChunkSource::Mic => 0,
            ChunkSource::Sys => 1,
        };
        (c.start_ms, source_rank)
    });

    // Estimate per-piece end_ms before serialising. Three sources, in
    // priority order:
    //   1. Piece's own word timings: max(w.end_ms) — exact, used
    //      whenever the provider returned word-level data.
    //   2. Next entry's start: a piece can't outlast whatever audio
    //      came after it on the same stream. Conservative but always
    //      available.
    //   3. Word-count heuristic at ~350 ms/word (typical conversational
    //      rate), floored at 1 s. Only kicks in for the final piece
    //      on a stream when there are no words.
    //
    // The end_ms drives the player's overlap rendering: while currentTime
    // sits inside a piece's [start_ms, end_ms], that piece stays "active"
    // even after a later piece on the other source has begun. Slight
    // over-estimation is fine — the worst case is one extra row staying
    // lit a beat longer; under-estimation causes a visible skip.

    // First flatten chunks → pieces, tagging each with the source so
    // the next-same-source lookup works the same way it did per-chunk.
    struct Entry {
        source: ChunkSource,
        start_ms: u64,
        text: String,
        label: String,
        words: Vec<serde_json::Value>,
        max_word_end_abs: Option<u64>,
    }
    let mut entries: Vec<Entry> = Vec::new();
    // Parallel pieces vec used solely as input to
    // `bridge_short_interjections`, then drained back into `entries`
    // below. We can't bridge `entries` directly because the bridge's
    // sandwich rule reads `LabelledPiece.words` for duration; entries
    // hold absolute-time word JSON. Keeping a parallel `LabelledPiece`
    // sequence is the minimal way to ensure the timeline emits the
    // same speaker labels `build_labelled_transcript` does — without
    // this, the DB transcript collapses correctly but the playback
    // view (driven by timeline.jsonl) still renders per-chunk
    // speaker flicker.
    let mut bridge_pieces: Vec<LabelledPiece> = Vec::new();
    for chunk in &sorted {
        for piece in split_chunk(chunk) {
            let trimmed = piece.text.trim();
            if trimmed.is_empty() {
                continue;
            }
            // Convert word timings to stream-absolute by adding the
            // chunk's start_ms. The playback view's audio element runs
            // in the merged playback.wav timeline; word timestamps
            // inside a chunk are chunk-relative until we rebase here.
            let words_abs: Vec<serde_json::Value> = piece
                .words
                .iter()
                .map(|w| {
                    serde_json::json!({
                        "text": w.text,
                        "start_ms": chunk.start_ms.saturating_add(w.start_ms),
                        "end_ms": chunk.start_ms.saturating_add(w.end_ms),
                    })
                })
                .collect();
            // Piece start: first word's absolute start when present,
            // else the chunk's start (whole-chunk fallback piece).
            let start_ms = piece
                .words
                .first()
                .map(|w| chunk.start_ms.saturating_add(w.start_ms))
                .unwrap_or(chunk.start_ms);
            let max_word_end_abs = piece
                .words
                .iter()
                .map(|w| chunk.start_ms.saturating_add(w.end_ms))
                .max();
            entries.push(Entry {
                source: chunk.source,
                start_ms,
                text: trimmed.to_string(),
                label: piece.label.clone().unwrap_or_default(),
                words: words_abs,
                max_word_end_abs,
            });
            bridge_pieces.push(piece);
        }
    }
    bridge_short_interjections(&mut bridge_pieces);
    absorb_text_continuation_chains(&mut bridge_pieces);
    for (entry, bridged) in entries.iter_mut().zip(bridge_pieces.iter()) {
        entry.label = bridged.label.clone().unwrap_or_default();
    }

    let next_same_source_start: Vec<u64> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            entries
                .iter()
                .skip(i + 1)
                .find(|n| n.source == e.source)
                .map(|n| n.start_ms)
                .unwrap_or(0)
        })
        .collect();

    let mut out = String::new();
    for (i, entry) in entries.iter().enumerate() {
        let end_ms = if let Some(max_end) = entry.max_word_end_abs {
            max_end
        } else if next_same_source_start[i] > entry.start_ms {
            next_same_source_start[i]
        } else {
            let estimated = estimated_speech_ms(&entry.text);
            entry.start_ms.saturating_add(estimated)
        };
        let json = serde_json::json!({
            "start_ms": entry.start_ms,
            "end_ms": end_ms,
            "label": offset_speaker_label(&entry.label, label_offset),
            "text": entry.text,
            "words": entry.words,
            // Which stream the piece came from. The cross-session speaker
            // unification pass (#17) uses this to carry user renames from the
            // old timeline onto the freshly clustered one without letting a
            // mic-side rename leak onto a time-overlapping sys cluster (or
            // vice versa). Absent on timelines written by older builds —
            // readers treat that as "source unknown, match any".
            "source": match entry.source { ChunkSource::Mic => "mic", ChunkSource::Sys => "sys" },
        });
        out.push_str(&json.to_string());
        out.push('\n');
    }
    out
}

pub(crate) fn emit_status(app: &AppHandle, note_id: Option<&str>, phase: Phase) {
    emit_phase(app, RecordingStatus::plain(note_id, phase));
}

/// A stop phase of a capture that never transcribed as it ran (#146). Marked
/// rather than withheld: with no chunks to drain and no text to diarize the
/// chain lands on Idle in a few hundred milliseconds, and the Idle is what
/// makes the pending take visible. No drain counts either — there is nothing
/// pending to count.
fn emit_deferred_phase(app: &AppHandle, note_id: &str, phase: Phase) {
    emit_phase(app, RecordingStatus::deferred_phase(note_id, phase));
}

/// `Stopping` with the drain's progress attached (#182): `pending` is how many
/// transcribes were in flight, `done` how many have landed. Same channel and
/// same phase as before — the counts are additive, so a listener that reads
/// only `{noteId, phase}` sees exactly what it saw before.
fn emit_stopping_progress(app: &AppHandle, note_id: &str, pending: u32, done: u32) {
    emit_phase(app, RecordingStatus::stopping_progress(note_id, pending, done));
}

fn emit_phase(app: &AppHandle, status: RecordingStatus) {
    let phase = status.phase;
    let _ = app.emit("recording_status", status);
    // The tray's icon and its Start/Stop items are driven from here rather than
    // from each call site: this is the one funnel every phase change already
    // goes through, so the menu bar can't drift out of step with the pipeline.
    crate::menubar::on_phase(app, phase);
}

/// Per-note (re)diarize lifecycle (#187), on its own channel for the same
/// reason `transcribe_status` has one: re-diarize and unify are user actions on
/// an arbitrary note, so they must not reach `recording_status`, which describes
/// the live capture and nothing else.
fn emit_diarize_status(app: &AppHandle, note_id: &str, active: bool) {
    let _ = app.emit(
        "diarize_status",
        crate::recording::DiarizeStatusPayload {
            note_id: note_id.to_string(),
            active,
        },
    );
}

fn emit_error(app: &AppHandle, note_id: Option<&str>, message: &str) {
    let _ = app.emit("recording_error", ErrorPayload {
        note_id: note_id.map(|s| s.to_string()),
        message: message.to_string(),
    });
}

/// Tell the sync observer a note changed via a backend-driven write — recording
/// transcript, re-diarize, or a timeline edit — that bypasses the `notes_*`
/// commands (those ping the observer themselves; the manual transcript/speaker
/// edits in the UI go through `notes_update`, so they're already covered). This
/// enqueues a push so the generated transcript/summary actually replicate.
/// No-op under the open-source `NoopSync`.
pub(crate) fn note_changed_for_sync(app: &AppHandle, note_id: &str) {
    app.state::<AppState>().sync.note_upserted(note_id);
}

/// Tell the sync observer a recording *session* was created or changed (#16) —
/// a take finished recording/importing, or was re-diarized. Enqueues the
/// `note_sessions` metadata push (the binary assets are uploaded separately,
/// frontend-triggered). Without this explicit ping the session would never
/// sync, since the pipeline writes it to disk outside the `notes_*` commands.
/// No-op under the open-source `NoopSync`.
fn session_changed_for_sync(app: &AppHandle, note_id: &str, session_id: &str) {
    if session_id == sessions::LEGACY_SESSION_ID {
        return; // synthesized legacy take — synced via the notes.audio path
    }
    app.state::<AppState>().sync.session_upserted(note_id, session_id);
}

fn sidecar_path(_app: &AppHandle) -> Result<PathBuf, String> {
    // 1) Production / `tauri build`: Tauri copies external binaries next to
    //    the main executable inside the .app bundle's MacOS folder, with the
    //    triple suffix stripped. So look for ../MacOS/audio-capture first.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidates = [
                dir.join("audio-capture"),
                dir.join("audio-capture-aarch64-apple-darwin"),
                dir.join("audio-capture-x86_64-apple-darwin"),
            ];
            for c in candidates {
                if c.exists() {
                    return Ok(c);
                }
            }
        }
    }

    // 2) Dev (`tauri dev`): the binary lives under src-tauri/binaries/.
    if let Ok(cwd) = std::env::current_dir() {
        for triple in ["aarch64-apple-darwin", "x86_64-apple-darwin"] {
            let p = cwd.join(format!("src-tauri/binaries/audio-capture-{triple}"));
            if p.exists() { return Ok(p); }
            let p = cwd.join(format!("binaries/audio-capture-{triple}"));
            if p.exists() { return Ok(p); }
        }
    }

    Err("audio-capture sidecar not found".into())
}

#[cfg(test)]
mod chain_progress_tests {
    use super::*;

    fn every_chain() -> Vec<ChainProgress> {
        vec![
            ChainProgress::PostStop { note_id: "n".into() },
            ChainProgress::Rediarize { note_id: "n".into() },
            ChainProgress::Replay {
                note_id: "n".into(),
                progress: Arc::new(parking_lot::Mutex::new(ReplayProgress::new(Vec::new()))),
            },
            ChainProgress::Silent,
        ]
    }

    #[test]
    fn only_the_live_captures_chain_reports_on_recording_status() {
        // `recording_status` is a single global slot that also drives the tray,
        // so anything a user can start on an arbitrary note must stay off it —
        // its Idle would blank a recording running on a different note (#187).
        let on_global: Vec<Channel> = every_chain()
            .iter()
            .map(ChainProgress::channel)
            .filter(|c| *c == Channel::RecordingStatus)
            .collect();
        assert_eq!(on_global.len(), 1);
        assert_eq!(
            ChainProgress::PostStop { note_id: "n".into() }.channel(),
            Channel::RecordingStatus
        );
    }

    #[test]
    fn each_chain_reports_on_exactly_one_channel() {
        // One type owns the channel, so no chain can bracket on one and name
        // its steps on another.
        assert_eq!(
            ChainProgress::Rediarize { note_id: "n".into() }.channel(),
            Channel::DiarizeStatus
        );
        assert_eq!(
            ChainProgress::Replay {
                note_id: "n".into(),
                progress: Arc::new(parking_lot::Mutex::new(ReplayProgress::new(Vec::new()))),
            }
            .channel(),
            Channel::TranscribeStatus
        );
        assert_eq!(ChainProgress::Silent.channel(), Channel::None);
    }

    #[test]
    fn a_stream_step_counts_off_the_streams_it_has() {
        let counted = |present: &[bool]| -> Vec<(u32, u32)> {
            let streams = present.iter().filter(|p| **p).count() as u32;
            crate::recording::stream_reports(Step::Diarizing, streams)
                .iter()
                .filter_map(|r| r.counter.map(|c| (c.index, c.count)))
                .collect()
        };
        // Mic-only and sys-only are one unit: nothing to count, so no counter.
        assert!(counted(&[true]).is_empty());
        assert!(counted(&[true, false]).is_empty());
        assert!(counted(&[false, true]).is_empty());
        assert_eq!(counted(&[true, true]), vec![(1, 2), (2, 2)]);
        // A stream that isn't there is not counted, so a third source is one
        // more flag rather than a rewrite of every literal.
        assert_eq!(counted(&[true, true, true]), vec![(1, 3), (2, 3), (3, 3)]);
    }

    #[test]
    fn a_stream_step_never_counts_past_its_own_total() {
        let reports = crate::recording::stream_reports(Step::SavingAudio, 2);
        assert_eq!(reports.len(), 2);
        for r in &reports {
            let c = r.counter.expect("two streams carry a counter");
            assert!(c.index <= c.count, "{c:?}");
        }
        // `begin` runs out rather than emitting a third report over two
        // streams, so an extra call cannot render "3 of 2".
        let mut iter = crate::recording::stream_reports(Step::SavingAudio, 2).into_iter();
        assert!(iter.next().is_some());
        assert!(iter.next().is_some());
        assert!(iter.next().is_none());
    }
}

#[cfg(test)]
mod diarize_tests {
    use super::*;
    use super::diarization::{combine_with_snapshot, session_speaker_offset};
    use super::timeline::{comparable_words, orphaned_prefix, projection_covers, synthesize_orphan_timeline};
    use super::transcript::{build_labelled_transcript, single_piece};

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

    /// Build a sys chunk with explicit word timings. Each `(text,
    /// start_ms, end_ms)` is chunk-relative, matching how the
    /// transcribe path stores them.
    fn sys_with_words(
        start_ms: u64,
        words: Vec<(&str, u64, u64)>,
    ) -> ChunkRecord {
        let text = words.iter().map(|(t, _, _)| *t).collect::<Vec<_>>().join(" ");
        ChunkRecord {
            source: ChunkSource::Sys,
            start_ms,
            text,
            words: words
                .into_iter()
                .map(|(t, s, e)| crate::recording::ChunkWord {
                    text: t.to_string(),
                    start_ms: s,
                    end_ms: e,
                })
                .collect(),
            detected_language: None,
        }
    }

    /// Wrap a simple chunk-level labeller as a piece producer that
    /// emits one whole-chunk piece. Lets the existing tests keep their
    /// `Fn(&ChunkRecord) -> Option<String>` shape while
    /// `build_labelled_transcript` consumes the new
    /// `Fn(&ChunkRecord) -> Vec<LabelledPiece>` signature.
    fn whole_chunk_pieces<F: Fn(&ChunkRecord) -> Option<String>>(
        labeller: F,
    ) -> impl Fn(&ChunkRecord) -> Vec<LabelledPiece> {
        move |c: &ChunkRecord| single_piece(c, labeller(c))
    }

    /// Issue #169: the transcript and the timeline are written from the same
    /// post-stop pass, but only the transcript gets the prior-take snapshot
    /// prepended — `serialize_timeline` sees this session's chunks alone. So a
    /// snapshot that no *earlier session's* timeline accounts for is text that
    /// exists in `note.transcript` and in no timeline at all. The styled reader
    /// renders from the merged timelines, so that text is invisible while the
    /// edit textarea (bound to the raw string) shows it — and any later
    /// `rebuild_note_transcript` (cycle a chunk label, delete a chunk,
    /// re-diarize, unify) rewrites the transcript to the timeline projection
    /// and deletes it outright.
    ///
    /// A note reaches that state when a recording lands text but writes no
    /// session assets — the diarize-model-missing early return is the route we
    /// have evidence for — and a later take on the same note becomes the only
    /// session with a timeline.
    ///
    /// Closed by repair-on-open: `orphaned_prefix` spots the snapshot the
    /// projection doesn't cover and `synthesize_orphan_timeline` gives it a
    /// session, so the merged timeline accounts for the whole transcript.
    #[test]
    fn orphaned_snapshot_text_survives_in_the_timeline() {
        // Take 1 left this behind: live-appended text, unlabelled, because the
        // pass that would have labelled it (and written its timeline) returned
        // early when the diarize model was missing.
        let snapshot = "we kicked off by agreeing the deadline slips a week";

        // Take 2, diarized normally: its own chunks, its own timeline.
        let chunks = vec![
            sys(30, "so where did we land on the freeze."),
            mic(4_000, "pushing it to the following Friday."),
        ];
        let labeller = |c: &ChunkRecord| match c.source {
            ChunkSource::Mic => Some("You".to_string()),
            ChunkSource::Sys => Some("Speaker 1".to_string()),
        };
        let split = whole_chunk_pieces(labeller);

        let combined = combine_with_snapshot(snapshot, &build_labelled_transcript(&chunks, &split));
        let session_two = serialize_timeline(&chunks, &split, session_speaker_offset(snapshot));

        // What opening the note does: project the sessions that exist, find
        // the text the projection can't account for, and give it a session of
        // its own in front of them.
        let projection = group_values_to_transcript(&jsonl_values(&session_two));
        let orphans =
            orphaned_prefix(&combined, &projection).expect("the snapshot is orphaned today");
        let timeline = format!("{}{}", synthesize_orphan_timeline(&orphans), session_two);

        // What the reader can actually show: every text the merged timeline
        // carries. Anything in the transcript but absent here is unreachable.
        let shown: String = timeline
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| {
                serde_json::from_str::<serde_json::Value>(l)
                    .ok()?
                    .get("text")?
                    .as_str()
                    .map(|s| s.to_string())
            })
            .collect::<Vec<_>>()
            .join(" ");

        for line in combined.lines().filter(|l| !l.trim().is_empty()) {
            // Strip any "Label: " prefix — the reader draws identity as a dot
            // rather than text, so only the words themselves must be reachable.
            let words = line.split_once(": ").map_or(line, |(_, rest)| rest);
            assert!(
                shown.contains(words),
                "transcript line is in no timeline entry, so the reader cannot \
                 show it and a rebuild would delete it: {words:?}",
            );
        }

        // The rebuild paths are the other half of the bug: `rebuild_note_transcript`
        // derives the transcript from the timelines alone, so before the repair
        // cycling one speaker pill deleted the snapshot outright. After it, a
        // rebuild reproduces the same words.
        let repaired_projection = group_values_to_transcript(&jsonl_values(&timeline));
        assert_eq!(
            comparable_words(&repaired_projection),
            comparable_words(&combined),
            "a rebuild after the repair must not change the transcript's words",
        );
        // ...which is also what makes the repair a fixed point: a second open
        // finds nothing left to account for, so it adds no second session.
        assert!(
            projection_covers(&combined, &repaired_projection)
                && orphaned_prefix(&combined, &repaired_projection).is_none(),
            "repair must be idempotent",
        );
    }

    /// Parse a serialised timeline back into the values the projection and
    /// per-chunk edit paths work on. `read_timeline_values` does this from a
    /// path; tests hold the JSONL in memory.
    fn jsonl_values(jsonl: &str) -> Vec<serde_json::Value> {
        jsonl
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    // ---- Orphaned transcript text (#169) --------------------------------

    /// The diarize-skipped path still has to leave a timeline behind, or the
    /// text it live-appended becomes the next orphan. No model means nothing
    /// can be said about who spoke — so the label is empty, and *only* the
    /// label: word timings describe when each word landed, which diarization
    /// has no bearing on, so they are kept and the note keeps per-word
    /// highlighting.
    #[test]
    fn diarize_skipped_timeline_carries_text_and_timings_with_no_label() {
        let chunks = vec![
            sys_with_words(0, vec![("where", 0, 400), ("now", 400, 900)]),
            sys_with_words(4_000, vec![("friday", 0, 500)]),
        ];
        let split = |c: &ChunkRecord| single_piece(c, None);
        let values = jsonl_values(&serialize_timeline(&chunks, &split, 0));
        assert_eq!(values.len(), 2);
        for v in &values {
            assert_eq!(v["label"], "");
        }
        assert_eq!(values[0]["text"], "where now");
        assert_eq!(values[1]["text"], "friday");
        // The provider's word timings survive a missing diarize model, rebased
        // to stream-absolute as usual...
        assert_eq!(values[0]["words"][1]["text"], "now");
        assert_eq!(values[0]["words"][1]["start_ms"], 400);
        assert_eq!(values[1]["words"][0]["start_ms"], 4_000);
        // ...which also gives the entry the tighter bounds `serialize_timeline`
        // derives from words rather than the coarse chunk fallback.
        assert_eq!(values[0]["end_ms"], 900);
        // And it projects back to the text the live append had already saved.
        assert_eq!(group_values_to_transcript(&values), "where now friday");
    }

    // ---- Per-session (#16) label offset + timeline plumbing -------------

    #[test]
    fn serialize_timeline_applies_label_offset() {
        // A resumed take's timeline `Speaker N` labels get bumped past the
        // prior take's, matching the DB transcript combine_with_snapshot
        // produces. Two same-speaker sys chunks (terminated so the
        // continuation absorber leaves them be).
        let chunks = vec![sys(0, "alpha beta gamma."), sys(5_000, "delta epsilon zeta.")];
        let labeller = |c: &ChunkRecord| single_piece(c, Some("Speaker 1".to_string()));
        let out = serialize_timeline(&chunks, &labeller, 2);
        let labels: Vec<String> = out
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                serde_json::from_str::<serde_json::Value>(l)
                    .unwrap()
                    .get("label")
                    .and_then(|v| v.as_str())
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert!(!labels.is_empty());
        assert!(labels.iter().all(|l| l == "Speaker 3"), "labels = {labels:?}");
    }

    #[test]
    fn serialize_timeline_leaves_you_and_zero_offset_untouched() {
        // "You" is never offset; offset 0 is a no-op.
        let chunks = vec![mic(0, "just me talking.")];
        let labeller = |c: &ChunkRecord| single_piece(c, Some("You".to_string()));
        let out = serialize_timeline(&chunks, &labeller, 4);
        assert!(out.contains("\"label\":\"You\""));
    }
}

#[cfg(test)]
mod import_tests {
    use super::*;
    use tokio::sync::Semaphore;

    #[tokio::test]
    async fn import_backlog_semaphore_bounds_concurrency() {
        // The import reader acquires a permit before spawning each chunk's
        // transcribe task; with IMPORT_BACKLOG_PERMITS outstanding the next
        // acquire must block. This pins the invariant the bounded-backlog
        // backpressure relies on (a full-speed replay can't pile up unboundedly).
        let sem = Arc::new(Semaphore::new(IMPORT_BACKLOG_PERMITS));
        let mut held = Vec::new();
        for _ in 0..IMPORT_BACKLOG_PERMITS {
            held.push(sem.clone().acquire_owned().await.unwrap());
        }
        assert!(
            sem.clone().try_acquire_owned().is_err(),
            "backlog should be saturated at IMPORT_BACKLOG_PERMITS"
        );
        // Releasing one permit (a chunk finished) frees a slot again.
        held.pop();
        assert!(
            sem.try_acquire_owned().is_ok(),
            "a freed permit should be acquirable"
        );
    }
}
