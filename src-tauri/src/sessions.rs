//! Per-note recording *session* storage.
//!
//! A **session** is one `recording_start` → `recording_stop` cycle (see the
//! PRD in issue #16). Each session's assets live in their own subdirectory,
//! keyed by a UUID, so a second recording on a note never clobbers the first:
//!
//! ```text
//! recordings/<note_id>/
//!   sessions.json                       # ordered manifest (this file)
//!   <session_uuid>/
//!     playback.wav                      # mixed mono 16k WAV for the player
//!     timeline.jsonl                    # per-chunk timeline (offset labels)
//!     mic.wav / sys.wav                 # retained source streams (opt-in / auto)
//!     chunks.json                       # chunk timings for re-diarize
//! ```
//!
//! Ordering lives *only* in the manifest (`index`), never in the path.
//!
//! ## Backward compatibility
//!
//! Notes recorded before this feature stored their assets *flat* in
//! `recordings/<note_id>/` with no manifest. Those are read as a single
//! "legacy" session ([`LEGACY_SESSION_ID`]) via [`resolve_sessions`] — no
//! forced migration on read. A flat note is only migrated into a session
//! subdir when a *second* recording is made on it (see
//! [`migrate_flat_if_needed`]).
//!
//! The functions here are deliberately pure path/FS helpers over an
//! injectable `recordings_dir` so they can be unit-tested against a tempdir
//! without a Tauri `AppHandle`.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Sentinel id for the synthesized single session that represents a
/// pre-feature flat `recordings/<note_id>/` layout. No real UUID collides
/// with this, so callers can round-trip it through the frontend and back
/// (`resolve_session_dir`) and have it map to the flat dir.
pub const LEGACY_SESSION_ID: &str = "__legacy__";

const MANIFEST_FILE: &str = "sessions.json";
const MANIFEST_VERSION: u32 = 1;

/// Per-chunk / per-source asset filenames living inside a session dir (or,
/// for a legacy note, flat in the recordings dir).
pub const ASSET_FILES: [&str; 5] = [
    "playback.wav",
    "timeline.jsonl",
    "mic.wav",
    "sys.wav",
    "chunks.json",
];

/// One recording session's metadata, as persisted in `sessions.json` and
/// surfaced to the frontend carousel.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionEntry {
    /// UUID (or [`LEGACY_SESSION_ID`] for a synthesized legacy session).
    pub id: String,
    /// 1-based order in which this session was recorded. The carousel numbers
    /// pills from this, and re-labelled transcripts derive their per-session
    /// speaker-number offset from prior sessions' order.
    pub index: u32,
    /// RFC3339 UTC timestamp of `recording_start`. Empty for legacy sessions.
    #[serde(default)]
    pub started_at: String,
    /// Wall-clock length in milliseconds (max end_ms across the timeline).
    /// Best-effort; 0 when unknown.
    #[serde(default)]
    pub duration_ms: u64,
    /// Which streams produced content, e.g. `["mic"]` or `["mic","sys"]`.
    #[serde(default)]
    pub streams: Vec<String>,
    /// Whether this take's audio has been through the transcription provider
    /// yet (#146). `true` for every take recorded the ordinary way — the text
    /// landed chunk by chunk as the meeting ran. `false` only for a take
    /// captured with **Transcribe manually** on, which holds its retained
    /// audio and waits for the note's Transcribe action.
    ///
    /// Defaults to `true`, which is what makes it safe to add to a manifest
    /// that already exists: an entry written before this field did *was*
    /// transcribed, and reading it back as untranscribed would offer to
    /// re-transcribe (and so re-diarize, re-number and rewrite) every take on
    /// every note in the library.
    #[serde(default = "default_transcribed")]
    pub transcribed: bool,
}

fn default_transcribed() -> bool {
    true
}

/// Whether a take being appended to the manifest already has its text (#146).
/// A named pair rather than a trailing `bool`, so `append_session(…, No)` reads
/// as what it is at the call site — the rest of this change spends enums
/// (`SinkMode`, `LabelFallback`) on exactly this kind of distinction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transcribed {
    Yes,
    No,
}

impl Transcribed {
    fn as_bool(self) -> bool {
        matches!(self, Transcribed::Yes)
    }
}

/// The ordered manifest persisted at `recordings/<note_id>/sessions.json`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionsManifest {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub sessions: Vec<SessionEntry>,
}

fn default_version() -> u32 {
    MANIFEST_VERSION
}

impl SessionsManifest {
    pub fn empty() -> Self {
        Self {
            version: MANIFEST_VERSION,
            sessions: Vec::new(),
        }
    }
}

/// `<app_data>/recordings/<note_id>`.
pub fn recordings_dir(app_data_dir: &Path, note_id: &str) -> PathBuf {
    app_data_dir.join("recordings").join(note_id)
}

/// `<recordings_dir>/sessions.json`.
pub fn manifest_path(recordings_dir: &Path) -> PathBuf {
    recordings_dir.join(MANIFEST_FILE)
}

/// `<recordings_dir>/<session_id>` — the per-session asset subdir.
pub fn session_dir(recordings_dir: &Path, session_id: &str) -> PathBuf {
    recordings_dir.join(session_id)
}

/// The dir new assets for `session_id` should be **written** into — kept in
/// lockstep with the read path ([`resolve_session_dir`]).
///
/// This is the whole fix for the legacy-note re-diarize no-op: a legacy note's
/// assets are *read* flat from `recordings/<note_id>/`, but a naïve
/// `session_dir(recordings, LEGACY_SESSION_ID)` writes to a
/// `recordings/<note_id>/__legacy__/` **subdir** the reader never looks at — so
/// re-diarize would silently write new labels to an orphan dir while the UI
/// kept showing the stale flat transcript. Special-casing the sentinel here
/// makes every writer (playback assets, keep-audio copies, timeline rewrites)
/// land exactly where [`resolve_session_dir`] / [`latest_session_dir`] read.
pub fn session_write_dir(recordings_dir: &Path, session_id: &str) -> PathBuf {
    if session_id == LEGACY_SESSION_ID {
        recordings_dir.to_path_buf()
    } else {
        session_dir(recordings_dir, session_id)
    }
}

/// True when `id` is safe to use as a filesystem path segment for a session
/// dir: non-empty and only `[A-Za-z0-9_-]`. Our client only ever mints session
/// ids as UUIDs (or the `__legacy__` sentinel, which is all underscores), so
/// anything else — path separators, `..`, dots, whitespace — is a hostile id
/// smuggled in from a remote `note_sessions.client_id` or a tampered
/// `sessions.json`. Rejecting it at every point where manifest/remote data
/// becomes a path stops directory traversal (e.g. `../../../Desktop` reaching
/// `remove_dir_all`). Mirrors `cloud_sync::is_safe_id`, whose remote pull path
/// guards the same way. Note `__legacy__` passes (underscores only).
pub fn is_safe_session_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

/// Read + parse the manifest. `None` when it's absent or unparseable — the
/// caller then falls back to the legacy flat layout.
pub fn read_manifest(recordings_dir: &Path) -> Option<SessionsManifest> {
    let path = manifest_path(recordings_dir);
    let body = std::fs::read_to_string(&path).ok()?;
    match serde_json::from_str::<SessionsManifest>(&body) {
        Ok(m) => Some(m),
        Err(e) => {
            eprintln!("sessions: manifest parse {}: {e}", path.display());
            None
        }
    }
}

/// Write the manifest (pretty-printed), creating the recordings dir if needed.
///
/// **Atomic replace.** The body is written to a sibling temp file on the *same*
/// filesystem, then `rename`d over the target. A concurrent reader therefore
/// only ever observes the old complete file or the new complete file — never a
/// half-written `sessions.json` (which [`read_manifest`] would treat as
/// unparseable and fall back to the legacy/empty layout, blanking the
/// carousel). The temp name is fixed because the process-wide manifest lock in
/// `AppState` serializes read-modify-write across the post-stop and cloud-pull
/// paths, so two writers can never race on the temp file itself.
pub fn write_manifest(recordings_dir: &Path, manifest: &SessionsManifest) -> std::io::Result<()> {
    std::fs::create_dir_all(recordings_dir)?;
    let body = serde_json::to_string_pretty(manifest)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tmp_path = recordings_dir.join(format!("{MANIFEST_FILE}.tmp"));
    std::fs::write(&tmp_path, body)?;
    std::fs::rename(&tmp_path, manifest_path(recordings_dir))
}

/// Whether any flat (pre-feature) asset file sits directly in the recordings
/// dir. Used to decide if a manifest-less note is a real legacy note (as
/// opposed to a brand-new note with nothing recorded yet).
pub fn has_flat_assets(recordings_dir: &Path) -> bool {
    ASSET_FILES
        .iter()
        .any(|f| recordings_dir.join(f).exists())
}

fn flat_streams(recordings_dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if recordings_dir.join("mic.wav").exists() {
        out.push("mic".to_string());
    }
    if recordings_dir.join("sys.wav").exists() {
        out.push("sys".to_string());
    }
    // A legacy note always has a playback.wav even when the raw source WAVs
    // weren't retained. Default to a mic stream so the carousel/tooltip has
    // something sensible to show.
    if out.is_empty() {
        out.push("mic".to_string());
    }
    out
}

/// Ordered `(entry, asset_dir)` pairs for a note, with legacy fallback.
///
/// - Manifest present + non-empty → each session mapped to its subdir,
///   sorted by `index`.
/// - No manifest but flat assets exist → a single synthesized legacy session
///   ([`LEGACY_SESSION_ID`], index 1) pointing at the flat recordings dir.
/// - Nothing recorded → empty.
pub fn resolve_sessions(recordings_dir: &Path) -> Vec<(SessionEntry, PathBuf)> {
    if let Some(manifest) = read_manifest(recordings_dir) {
        if !manifest.sessions.is_empty() {
            let mut pairs: Vec<(SessionEntry, PathBuf)> = manifest
                .sessions
                .into_iter()
                // Trust boundary: a manifest entry's id is joined onto the
                // recordings dir below. Drop any that isn't a safe segment so a
                // tampered `sessions.json` can't traverse out of the tree.
                .filter(|e| is_safe_session_id(&e.id))
                .map(|e| {
                    let dir = session_dir(recordings_dir, &e.id);
                    (e, dir)
                })
                .collect();
            pairs.sort_by_key(|(e, _)| e.index);
            return pairs;
        }
    }
    if has_flat_assets(recordings_dir) {
        let entry = SessionEntry {
            id: LEGACY_SESSION_ID.to_string(),
            index: 1,
            started_at: String::new(),
            duration_ms: 0,
            streams: flat_streams(recordings_dir),
            // A pre-feature flat note predates deferred transcription, so its
            // one take is transcribed by construction.
            transcribed: true,
        };
        return vec![(entry, recordings_dir.to_path_buf())];
    }
    Vec::new()
}

/// Resolve a single session's asset dir by id (honouring the legacy
/// fallback), or `None` if the id isn't known for this note.
pub fn resolve_session_dir(recordings_dir: &Path, session_id: &str) -> Option<PathBuf> {
    resolve_sessions(recordings_dir)
        .into_iter()
        .find(|(e, _)| e.id == session_id)
        .map(|(_, dir)| dir)
}

/// The most-recent session's asset dir, falling back to the flat recordings
/// dir when nothing is recorded yet. Always returns a path (used as the
/// cloud upload source + download target so single-file audio sync keeps
/// pointing at the latest take, exactly as before per-session storage).
pub fn latest_session_dir(recordings_dir: &Path) -> PathBuf {
    resolve_sessions(recordings_dir)
        .last()
        .map(|(_, dir)| dir.clone())
        .unwrap_or_else(|| recordings_dir.to_path_buf())
}

/// Migrate a pre-feature flat note into a session subdir on its second
/// recording. No-op when a manifest already exists or there are no flat
/// assets (a brand-new note). Best-effort: on any FS error it leaves the
/// flat layout in place and returns `Ok(false)` so the caller proceeds
/// (the new session still records; the old take is no worse off than the
/// pre-feature overwrite behaviour).
///
/// `legacy_id` is the UUID to assign the migrated session; the caller
/// generates it so tests stay deterministic.
pub fn migrate_flat_if_needed(recordings_dir: &Path, legacy_id: &str) -> std::io::Result<bool> {
    if read_manifest(recordings_dir).is_some() {
        return Ok(false); // already session-shaped
    }
    if !has_flat_assets(recordings_dir) {
        return Ok(false); // brand-new note, nothing to migrate
    }
    let streams = flat_streams(recordings_dir);
    let dir = session_dir(recordings_dir, legacy_id);
    std::fs::create_dir_all(&dir)?;
    for f in ASSET_FILES {
        let src = recordings_dir.join(f);
        if src.exists() {
            std::fs::rename(&src, dir.join(f))?;
        }
    }
    let manifest = SessionsManifest {
        version: MANIFEST_VERSION,
        sessions: vec![SessionEntry {
            id: legacy_id.to_string(),
            index: 1,
            started_at: String::new(),
            duration_ms: 0,
            streams,
            transcribed: true,
        }],
    };
    write_manifest(recordings_dir, &manifest)?;
    Ok(true)
}

/// Append a freshly-finished session to the manifest and return its assigned
/// `index`. Creates the manifest when absent (a brand-new note's first
/// session).
pub fn append_session(
    recordings_dir: &Path,
    id: &str,
    started_at: &str,
    duration_ms: u64,
    streams: Vec<String>,
    transcribed: Transcribed,
) -> std::io::Result<u32> {
    let mut manifest = read_manifest(recordings_dir).unwrap_or_else(SessionsManifest::empty);
    let index = manifest.sessions.iter().map(|e| e.index).max().unwrap_or(0) + 1;
    manifest.sessions.push(SessionEntry {
        id: id.to_string(),
        index,
        started_at: started_at.to_string(),
        duration_ms,
        streams,
        transcribed: transcribed.as_bool(),
    });
    write_manifest(recordings_dir, &manifest)?;
    Ok(index)
}

/// Flip one session's [`SessionEntry::transcribed`] to `true` (#146). Returns
/// `false` when the note has no manifest entry with that id — the caller has
/// nothing to correct, and writing a fresh entry here would invent a take.
///
/// Deliberately *not* a general "set" with a bool: transcription is one-way.
/// A take whose text is in the note can't be made untranscribed again without
/// also unwinding the transcript, the timeline and the speaker numbering, and
/// nothing in the product asks for that.
pub fn mark_session_transcribed(recordings_dir: &Path, session_id: &str) -> std::io::Result<bool> {
    let Some(mut manifest) = read_manifest(recordings_dir) else {
        return Ok(false);
    };
    let Some(entry) = manifest.sessions.iter_mut().find(|e| e.id == session_id) else {
        return Ok(false);
    };
    if entry.transcribed {
        return Ok(true); // already there; don't rewrite the file for nothing
    }
    entry.transcribed = true;
    write_manifest(recordings_dir, &manifest)?;
    Ok(true)
}

/// A note's takes that still hold untranscribed audio, in recording order
/// (#146). The order is the whole point: each take's timeline is written with a
/// speaker-number offset taken from the takes *before* it, so transcribing them
/// out of order would number the later ones off a still-empty predecessor.
///
/// The legacy flat pseudo-session can never appear here (it reads back
/// `transcribed: true`), so no caller has to special-case a session id that
/// resolves to the note's whole recordings dir.
pub fn untranscribed_sessions(recordings_dir: &Path) -> Vec<(SessionEntry, PathBuf)> {
    resolve_sessions(recordings_dir)
        .into_iter()
        .filter(|(e, _)| !e.transcribed)
        .collect()
}

/// Number of sessions already recorded (manifest entries, or 1 for a legacy
/// flat note). This used to also drive auto-retention ("a note's second take
/// keeps its source WAVs regardless of `keep_audio`"); #24 removed that
/// exception — see [`retain_audio`] — so this is now only a count.
pub fn existing_session_count(recordings_dir: &Path) -> usize {
    resolve_sessions(recordings_dir).len()
}

// ---------------------------------------------------------------------------
// Cloud sync (#16) — pure helpers shared by the asset upload/download path in
// `commands::cloud`. Kept here (over `crate::sessions` FS state) so they can be
// unit-tested without a Tauri `AppHandle` or a live PocketBase.
// ---------------------------------------------------------------------------

/// One per-session asset file, matching the five typed file fields on the
/// server's `note_sessions` collection. The `field` name is both the
/// PocketBase record key and the multipart form-part name; `file` is the local
/// filename inside a session dir.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssetField {
    Playback,
    Mic,
    Sys,
    Timeline,
    Chunks,
}

impl AssetField {
    /// All five fields, in a stable order.
    pub const ALL: [AssetField; 5] = [
        AssetField::Playback,
        AssetField::Mic,
        AssetField::Sys,
        AssetField::Timeline,
        AssetField::Chunks,
    ];

    /// The `note_sessions` field / multipart part name.
    pub fn field(self) -> &'static str {
        match self {
            AssetField::Playback => "playback",
            AssetField::Mic => "mic",
            AssetField::Sys => "sys",
            AssetField::Timeline => "timeline",
            AssetField::Chunks => "chunks",
        }
    }

    /// The local filename inside a session dir.
    pub fn file_name(self) -> &'static str {
        match self {
            AssetField::Playback => "playback.wav",
            AssetField::Mic => "mic.wav",
            AssetField::Sys => "sys.wav",
            AssetField::Timeline => "timeline.jsonl",
            AssetField::Chunks => "chunks.json",
        }
    }

    /// Whether this asset is *audio* — the thing `keep_audio` governs (#24).
    /// `timeline` and `chunks` are text (word timings / chunk timings) and are
    /// kept regardless: they drive the merged reader and the re-diarize anchors,
    /// and they carry no recording of anyone's voice.
    pub fn is_audio(self) -> bool {
        matches!(
            self,
            AssetField::Playback | AssetField::Mic | AssetField::Sys
        )
    }

    /// The MIME type to stamp on the multipart upload. The JSON-ish assets go
    /// up as octet-stream (the server's timeline/chunks fields have no mime
    /// restriction — jsonl/json content-sniff inconsistently).
    pub fn mime(self) -> &'static str {
        match self {
            AssetField::Playback | AssetField::Mic | AssetField::Sys => "audio/wav",
            AssetField::Timeline | AssetField::Chunks => "application/octet-stream",
        }
    }
}

/// Decide which asset fields to (re-)upload for one session, given which are
/// present on disk locally and which already have a file on the server.
///
/// Rules (the "upload sequencing" decision, unit-tested):
///  - `timeline` is uploaded whenever it exists locally — it's rewritten by
///    re-diarize / cross-session unification and is tiny, so teammates always
///    get the latest speaker labels.
///  - every other asset is uploaded only once (when present locally but not yet
///    on the server), so a long recording's multi-hundred-MB `mic`/`sys`/
///    `playback` WAVs aren't re-sent on every subsequent timeline edit.
pub fn session_upload_plan(
    local_present: &[AssetField],
    remote_present: &[AssetField],
) -> Vec<AssetField> {
    AssetField::ALL
        .into_iter()
        .filter(|f| {
            let local = local_present.contains(f);
            if !local {
                return false;
            }
            matches!(f, AssetField::Timeline) || !remote_present.contains(f)
        })
        .collect()
}

/// The *repair* variant of [`session_upload_plan`]: only the assets the server
/// is missing outright, never a re-send of one it already holds.
///
/// The full plan runs after an event that changed the timeline (a recording
/// finishing, a re-diarize, a speaker rename), so re-sending it is the point.
/// This one runs opportunistically on note open, to close the window where the
/// author's post-recording upload never happened at all — the asset PATCH is a
/// separate fire-and-forget call from the sync worker's metadata push, so
/// quitting the app (or losing the network) in the minute after a recording
/// leaves the session records on the server with no `timeline` file attached,
/// and nothing retries. Teammates then see a note with sessions but no speaker
/// labels. Repair-on-open is that retry, and it stays cheap by never re-sending
/// what is already up.
pub fn session_repair_plan(
    local_present: &[AssetField],
    remote_present: &[AssetField],
) -> Vec<AssetField> {
    AssetField::ALL
        .into_iter()
        .filter(|f| local_present.contains(f) && !remote_present.contains(f))
        .collect()
}

/// Drop the audio assets from an upload plan when this device is set not to
/// sync audio. The setting is about *recordings*, so it must not take the
/// timeline down with them: withholding word timings doesn't protect anything
/// (they carry no voice) and costs teammates every speaker label in the note.
pub fn retain_syncable(plan: Vec<AssetField>, sync_audio: bool) -> Vec<AssetField> {
    if sync_audio {
        return plan;
    }
    plan.into_iter().filter(|f| !f.is_audio()).collect()
}

/// Whether an asset already on disk is worth re-fetching when the server's copy
/// changed. Only the timeline: it is small text that the author *rewrites* after
/// the fact (re-diarize, cross-session unification, speaker rename), so a cached
/// copy silently pins the reader to stale labels. Audio is immutable per take
/// and can be hundreds of MB — fetched once, never again.
pub fn refetch_when_changed(field: AssetField) -> bool {
    matches!(field, AssetField::Timeline)
}

/// Decide whether to fetch one asset, given what's on disk and what the server
/// holds.
///
/// `stamp` is the remote filename recorded when this device last downloaded the
/// asset (see [`remote_stamp_path`]). PocketBase mints a fresh random suffix on
/// every upload (`timeline_x7rwstj5qc.jsonl`), so a differing filename is an
/// exact "the server's copy is not the one I have" signal — no clock comparison,
/// no skew.
pub fn should_fetch_asset(
    field: AssetField,
    dest_exists: bool,
    stamp: Option<&str>,
    remote_filename: &str,
) -> bool {
    if remote_filename.is_empty() {
        return false; // nothing attached to the record
    }
    if !dest_exists {
        return true;
    }
    if !refetch_when_changed(field) {
        return false;
    }
    stamp != Some(remote_filename)
}

/// Where the last-downloaded remote filename for an asset is recorded, beside
/// the asset itself. A dotfile so it never looks like a recording artefact.
pub fn remote_stamp_path(session_dir: &Path, field: AssetField) -> PathBuf {
    session_dir.join(format!(".{}.remote", field.file_name()))
}

// ---------------------------------------------------------------------------
// Honest keep_audio (#24) — the storage decision and the cleanup primitives.
// ---------------------------------------------------------------------------

/// The shipped default for `keep_audio`: off. The single source of truth for
/// the default; mirrored by the frontend's `settings/types.ts` default.
const KEEP_AUDIO_DEFAULT: bool = false;

/// Whether this device should persist audio at all, from the raw `keep_audio`
/// setting value. The single gate for every audio write and fetch (#24).
///
/// **The setting is the outer gate, with nothing above it.** Before #24 it only
/// governed the *raw* mic/sys copies: `playback.wav` was written
/// unconditionally, and #16 force-retained the sources once a note had a second
/// take — so "off" silently kept a full recording of the meeting on disk. Off
/// now means no audio, which costs cross-session unification (#17) on those
/// notes. That trade-off is what the setting buys; it is not a regression to
/// route around with another force-retain exception.
pub fn retain_audio(keep_setting: Option<&str>) -> bool {
    match keep_setting {
        Some(v) => v == "true",
        None => KEEP_AUDIO_DEFAULT,
    }
}

/// The shipped default for `transcribe_manually`: off. Mirrored by the
/// frontend's `settings/types.ts` default.
const TRANSCRIBE_MANUALLY_DEFAULT: bool = false;

/// Whether a recording starting *now* should skip live transcription and wait
/// for the note's Transcribe action (#146).
///
/// **`keep_audio` is the outer gate, and stays so.** Deferring transcription
/// means the meeting exists only as audio until the user asks for it, so on a
/// device that stores no audio (#24) the setting could only ever destroy the
/// recording. Rather than force-retaining behind the user's back — the class of
/// exception #24 exists to remove, and which `retain_audio` names — the whole
/// feature is inert while retention is off. Settings does the same thing
/// visually: the toggle isn't shown until "Keep recorded audio" is on.
pub fn defer_transcription(manual_setting: Option<&str>, keep_audio: bool) -> bool {
    if !keep_audio {
        return false;
    }
    match manual_setting {
        Some(v) => v == "true",
        None => TRANSCRIBE_MANUALLY_DEFAULT,
    }
}

/// Which of a teammate's session assets this device should fetch (#24). The
/// rule is *device-scoped*: a Mac with `keep_audio` off doesn't download
/// someone else's recording either, so the setting describes the machine
/// rather than just its own captures. The timeline comes down either way — it
/// is text, and the reader needs it.
pub fn session_download_plan(keep_audio: bool) -> Vec<AssetField> {
    let mut plan = Vec::new();
    if keep_audio {
        plan.push(AssetField::Playback);
    }
    plan.push(AssetField::Timeline);
    plan
}

/// Every stored audio file under one note's recordings dir: `*.wav` flat in the
/// dir and in every immediate subdir, sorted for stable output.
///
/// Deliberately a filesystem sweep rather than a walk of
/// [`resolve_sessions`] × [`AssetField`]. Once a `sessions.json` exists the
/// manifest wins over flat assets and unlisted dirs are invisible to the
/// reader — but a WAV in one is still a recording of a meeting sitting on
/// disk. A tombstoned session pulled from the cloud, a stray flat file beside
/// a migrated note, and a `mic.wav` in a dir no manifest mentions all have to
/// count here, or "delete stored audio" would leave audio behind while
/// reporting success — the same dishonesty #24 exists to remove.
pub fn stored_audio_files(recordings_dir: &Path) -> Vec<PathBuf> {
    fn wavs_in(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file()
                && path
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("wav"))
            {
                out.push(path);
            }
        }
    }

    let mut out = Vec::new();
    wavs_in(recordings_dir, &mut out);
    if let Ok(entries) = std::fs::read_dir(recordings_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                wavs_in(&path, &mut out);
            }
        }
    }
    out.sort();
    out
}

/// `<app_data>/recordings` — the parent of every note's recordings dir.
pub fn recordings_root(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("recordings")
}

/// What a "delete stored audio" sweep would cover, for the Settings confirm.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredAudioTotals {
    /// Notes that hold at least one audio file.
    pub notes: usize,
    /// Audio files across those notes (all sessions).
    pub files: usize,
    /// Total bytes on disk.
    pub bytes: u64,
    /// The note ids holding audio, so the caller can ping the sync observer
    /// for exactly the notes a deletion changed.
    pub note_ids: Vec<String>,
}

/// Tally stored audio across every note under `recordings_root`. Notes with no
/// audio (transcript-only, or already cleaned) don't count toward `notes` — the
/// confirm should say how much a deletion actually removes.
pub fn stored_audio_totals(recordings_root: &Path) -> StoredAudioTotals {
    let mut totals = StoredAudioTotals::default();
    let Ok(entries) = std::fs::read_dir(recordings_root) else {
        return totals; // nothing recorded on this device yet
    };
    let mut ids: Vec<(String, Vec<PathBuf>)> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
        .map(|id| {
            let files = stored_audio_files(&recordings_root.join(&id));
            (id, files)
        })
        .filter(|(_, files)| !files.is_empty())
        .collect();
    // Stable output: read_dir order is filesystem-dependent.
    ids.sort_by(|a, b| a.0.cmp(&b.0));
    for (id, files) in ids {
        totals.notes += 1;
        totals.files += files.len();
        totals.bytes += files
            .iter()
            .filter_map(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .sum::<u64>();
        totals.note_ids.push(id);
    }
    totals
}

/// Whether one session dir holds any audio.
///
/// The tie-breaker for discarding an empty take: a take that transcribed
/// nothing is only worth keeping if there's something to listen to. Deciding on
/// the audio rather than on the chunk count alone is deliberate — every chunk
/// failing (a provider outage, a revoked key) looks identical to silence from
/// here, and deleting a recording the user could still play or re-diarize is
/// not a recoverable mistake.
/// Whether this take still holds the audio a deferred transcription would
/// *replay* (#146): one of the raw per-source streams.
///
/// Deliberately not [`session_has_audio`], which counts any `.wav` — including
/// the mixed `playback.wav`. That is the right question for "is there a
/// recording of this meeting on disk" (what #24's sweep answers) and the wrong
/// one here: `transcribe_pending_takes` replays `mic.wav` / `sys.wav`, so a
/// take reduced to its playback mix would offer a Transcribe button that
/// silently does nothing. A mixdown is also not a substitute — the two streams
/// are kept separate end-to-end precisely so each is diarized on its own.
pub fn session_has_replayable_audio(session_dir: &Path) -> bool {
    session_dir.join("mic.wav").exists() || session_dir.join("sys.wav").exists()
}

pub fn session_has_audio(session_dir: &Path) -> bool {
    !stored_audio_files(session_dir).is_empty()
}

/// Delete every stored audio file for one note, keeping timelines, chunk
/// timings and (of course) the transcript. Returns how many files were
/// removed. Best-effort per file: a failure is skipped rather than aborting
/// the sweep, so one locked file can't strand the rest on disk.
pub fn delete_stored_audio(recordings_dir: &Path) -> usize {
    stored_audio_files(recordings_dir)
        .into_iter()
        .filter(|p| std::fs::remove_file(p).is_ok())
        .count()
}

/// Metadata for one remote `note_sessions` record, used to reconstruct the
/// local `sessions.json` manifest on a receiving (teammate) device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteSessionMeta {
    /// Session UUID (the record's `client_id`).
    pub client_id: String,
    pub index: u32,
    /// `started_at` epoch-ms (server `started_at`), 0 when unknown.
    pub started_at_ms: i64,
    pub duration_ms: u64,
    pub streams: Vec<String>,
    /// Tombstone — a deleted record removes the session from the manifest.
    pub deleted: bool,
}

/// epoch-ms → RFC3339 (manifest `started_at`). 0 / negative → empty string
/// (matching how a legacy session with no known start is represented).
pub fn ms_to_started_at(ms: i64) -> String {
    if ms <= 0 {
        return String::new();
    }
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_default()
}

/// Reconcile pulled remote session records into a local manifest.
///
///  - Non-deleted remote records become (or overwrite) manifest entries.
///  - A deleted remote record removes its entry (tombstone honoured).
///  - Local-only entries not present in `remote` are preserved — a device may
///    hold a freshly-recorded take that hasn't finished pushing yet, and a
///    pull mustn't drop it. (The synthesized [`LEGACY_SESSION_ID`] is likewise
///    preserved.)
///  - Entries are returned sorted by `index`.
pub fn reconcile_manifest(
    existing: Option<SessionsManifest>,
    remote: &[RemoteSessionMeta],
) -> SessionsManifest {
    let mut by_id: std::collections::BTreeMap<String, SessionEntry> = existing
        .map(|m| m.sessions)
        .unwrap_or_default()
        .into_iter()
        // Sanitize a possibly-tampered on-disk manifest so unsafe ids can never
        // survive a reconcile and be written back / joined onto a path.
        .filter(|e| is_safe_session_id(&e.id))
        .map(|e| (e.id.clone(), e))
        .collect();

    for r in remote {
        // A remote `client_id` is server-controlled and becomes both a local
        // path segment and a persisted manifest id. Never let a hostile one in.
        if !is_safe_session_id(&r.client_id) {
            continue;
        }
        if r.deleted {
            by_id.remove(&r.client_id);
            continue;
        }
        // `transcribed` is a *local* fact — whether this device has run this
        // take's retained audio through a provider (#146) — and the server
        // carries no field for it. Preserve whatever the local entry said, and
        // treat a session arriving for the first time as transcribed: its text
        // came down with the note, and this device holds no audio to replay.
        let transcribed = by_id.get(&r.client_id).map_or(true, |e| e.transcribed);
        by_id.insert(
            r.client_id.clone(),
            SessionEntry {
                id: r.client_id.clone(),
                index: r.index,
                started_at: ms_to_started_at(r.started_at_ms),
                duration_ms: r.duration_ms,
                streams: r.streams.clone(),
                transcribed,
            },
        );
    }

    let mut sessions: Vec<SessionEntry> = by_id.into_values().collect();
    sessions.sort_by_key(|e| e.index);
    SessionsManifest {
        version: MANIFEST_VERSION,
        sessions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn touch(path: &Path) {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).unwrap();
        }
        fs::write(path, b"x").unwrap();
    }

    #[test]
    fn resolve_empty_when_nothing_recorded() {
        let tmp = TempDir::new().unwrap();
        let rec = recordings_dir(tmp.path(), "note1");
        assert!(resolve_sessions(&rec).is_empty());
        assert_eq!(existing_session_count(&rec), 0);
    }

    #[test]
    fn flat_note_resolves_as_single_legacy_session() {
        let tmp = TempDir::new().unwrap();
        let rec = recordings_dir(tmp.path(), "note1");
        touch(&rec.join("playback.wav"));
        touch(&rec.join("timeline.jsonl"));

        let sessions = resolve_sessions(&rec);
        assert_eq!(sessions.len(), 1);
        let (entry, dir) = &sessions[0];
        assert_eq!(entry.id, LEGACY_SESSION_ID);
        assert_eq!(entry.index, 1);
        // Legacy assets live flat in the recordings dir, not a subdir.
        assert_eq!(dir, &rec);
    }

    /// #169's repair writes its synthesized session at index 0, in front of
    /// every recorded take, and migrates a legacy flat note first so the take
    /// it is repairing around doesn't vanish behind the new manifest.
    #[test]
    fn a_synthesized_session_at_index_zero_resolves_before_a_migrated_legacy_take() {
        let tmp = TempDir::new().unwrap();
        let rec = recordings_dir(tmp.path(), "note1");
        touch(&rec.join("playback.wav"));
        touch(&rec.join("timeline.jsonl"));
        assert!(migrate_flat_if_needed(&rec, "take-1").unwrap());

        let mut manifest = read_manifest(&rec).unwrap();
        manifest.sessions.push(SessionEntry {
            id: "repair".to_string(),
            index: 0,
            started_at: String::new(),
            duration_ms: 0,
            streams: Vec::new(),
            transcribed: true,
        });
        write_manifest(&rec, &manifest).unwrap();

        let ids: Vec<String> = resolve_sessions(&rec).into_iter().map(|(e, _)| e.id).collect();
        assert_eq!(ids, vec!["repair", "take-1"]);
        // And the next real recording still numbers itself past both.
        assert_eq!(append_session(&rec, "take-2", "", 0, vec![], Transcribed::Yes).unwrap(), 2);
    }

    #[test]
    fn flat_legacy_streams_reflect_retained_wavs() {
        let tmp = TempDir::new().unwrap();
        let rec = recordings_dir(tmp.path(), "note1");
        touch(&rec.join("playback.wav"));
        touch(&rec.join("mic.wav"));
        touch(&rec.join("sys.wav"));
        let sessions = resolve_sessions(&rec);
        assert_eq!(sessions[0].0.streams, vec!["mic", "sys"]);
    }

    // ---- write/read dir agreement (BUG 1: legacy re-diarize orphan dir) ----

    #[test]
    fn legacy_write_dir_matches_read_dir() {
        let tmp = TempDir::new().unwrap();
        let rec = recordings_dir(tmp.path(), "note1");
        touch(&rec.join("playback.wav")); // flat legacy note
        let read_dir = resolve_session_dir(&rec, LEGACY_SESSION_ID).unwrap();
        let write_dir = session_write_dir(&rec, LEGACY_SESSION_ID);
        // The writer must target exactly the dir the reader reads from — the
        // flat recordings dir, NOT a `__legacy__` subdir.
        assert_eq!(write_dir, read_dir);
        assert_eq!(write_dir, rec);
        assert_ne!(write_dir, session_dir(&rec, LEGACY_SESSION_ID));
    }

    #[test]
    fn uuid_write_dir_matches_read_dir() {
        let tmp = TempDir::new().unwrap();
        let rec = recordings_dir(tmp.path(), "note1");
        append_session(&rec, "uuid-a", "", 0, vec!["mic".into()], Transcribed::Yes).unwrap();
        let read_dir = resolve_session_dir(&rec, "uuid-a").unwrap();
        let write_dir = session_write_dir(&rec, "uuid-a");
        assert_eq!(write_dir, read_dir);
        assert_eq!(write_dir, session_dir(&rec, "uuid-a"));
    }

    #[test]
    fn legacy_rediarize_write_is_visible_to_read() {
        // Round-trip the exact failure: re-diarize writes NEW labels through
        // the write path; the read path (rebuild_note_transcript / note_timeline
        // both go through resolve_session_dir) must then see them, and no orphan
        // `__legacy__` subdir may be created.
        let tmp = TempDir::new().unwrap();
        let rec = recordings_dir(tmp.path(), "note1");
        touch(&rec.join("playback.wav"));
        fs::write(rec.join("timeline.jsonl"), b"{\"label\":\"Speaker 1\",\"text\":\"stale\"}\n")
            .unwrap();

        let wdir = session_write_dir(&rec, LEGACY_SESSION_ID);
        fs::write(wdir.join("timeline.jsonl"), b"{\"label\":\"Alice\",\"text\":\"fresh\"}\n").unwrap();

        let rdir = resolve_session_dir(&rec, LEGACY_SESSION_ID).unwrap();
        let body = fs::read_to_string(rdir.join("timeline.jsonl")).unwrap();
        assert!(body.contains("Alice"), "read path must see the re-diarized labels");
        assert!(!body.contains("stale"));
        assert!(
            !rec.join(LEGACY_SESSION_ID).exists(),
            "no orphan __legacy__ subdir may be created"
        );
    }

    // ---- atomic manifest write (BUG 2a) ----------------------------------

    #[test]
    fn write_manifest_is_atomic_temp_plus_rename() {
        let tmp = TempDir::new().unwrap();
        let rec = recordings_dir(tmp.path(), "note1");
        let mut m = SessionsManifest::empty();
        m.sessions.push(SessionEntry {
            id: "uuid-a".into(),
            index: 1,
            started_at: String::new(),
            duration_ms: 0,
            streams: vec![],
            transcribed: true,
        });
        write_manifest(&rec, &m).unwrap();

        // The final file is complete + parseable.
        let back = read_manifest(&rec).unwrap();
        assert_eq!(back.sessions.len(), 1);
        assert_eq!(back.sessions[0].id, "uuid-a");

        // The temp file used for the rename must not survive (rename consumes
        // it), so a reader never trips over a partial `.tmp` and the dir holds
        // only the real manifest.
        assert!(!rec.join(format!("{MANIFEST_FILE}.tmp")).exists());
        let names: Vec<String> = fs::read_dir(&rec)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec![MANIFEST_FILE.to_string()]);
    }

    #[test]
    fn write_manifest_overwrite_never_leaves_partial() {
        // Rewriting an existing manifest replaces it atomically: a reader always
        // parses a whole file, and no stale temp lingers.
        let tmp = TempDir::new().unwrap();
        let rec = recordings_dir(tmp.path(), "note1");
        append_session(&rec, "uuid-a", "", 0, vec![], Transcribed::Yes).unwrap();
        append_session(&rec, "uuid-b", "", 0, vec![], Transcribed::Yes).unwrap();
        let back = read_manifest(&rec).unwrap();
        assert_eq!(back.sessions.len(), 2);
        assert!(!rec.join(format!("{MANIFEST_FILE}.tmp")).exists());
    }

    #[test]
    fn resolve_session_dir_maps_legacy_id_to_flat_dir() {
        let tmp = TempDir::new().unwrap();
        let rec = recordings_dir(tmp.path(), "note1");
        touch(&rec.join("playback.wav"));
        assert_eq!(
            resolve_session_dir(&rec, LEGACY_SESSION_ID),
            Some(rec.clone())
        );
        assert_eq!(resolve_session_dir(&rec, "nope"), None);
    }

    #[test]
    fn append_creates_manifest_and_indexes_sequentially() {
        let tmp = TempDir::new().unwrap();
        let rec = recordings_dir(tmp.path(), "note1");
        let i1 =
            append_session(&rec, "uuid-a", "2026-07-09T10:00:00Z", 1000, vec!["mic".into()], Transcribed::Yes)
                .unwrap();
        let i2 = append_session(
            &rec,
            "uuid-b",
            "2026-07-09T11:00:00Z",
            2000,
            vec!["mic".into(), "sys".into()],
            Transcribed::Yes,
        )
        .unwrap();
        assert_eq!(i1, 1);
        assert_eq!(i2, 2);

        let sessions = resolve_sessions(&rec);
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].0.id, "uuid-a");
        assert_eq!(sessions[0].1, session_dir(&rec, "uuid-a"));
        assert_eq!(sessions[1].0.id, "uuid-b");
        assert_eq!(sessions[1].0.duration_ms, 2000);
        assert_eq!(existing_session_count(&rec), 2);
    }

    #[test]
    fn resolve_sorts_by_index_not_file_order() {
        let tmp = TempDir::new().unwrap();
        let rec = recordings_dir(tmp.path(), "note1");
        let manifest = SessionsManifest {
            version: 1,
            sessions: vec![
                SessionEntry {
                    id: "second".into(),
                    index: 2,
                    started_at: String::new(),
                    duration_ms: 0,
                    streams: vec![],
                    transcribed: true,
                },
                SessionEntry {
                    id: "first".into(),
                    index: 1,
                    started_at: String::new(),
                    duration_ms: 0,
                    streams: vec![],
                    transcribed: true,
                },
            ],
        };
        write_manifest(&rec, &manifest).unwrap();
        let sessions = resolve_sessions(&rec);
        assert_eq!(sessions[0].0.id, "first");
        assert_eq!(sessions[1].0.id, "second");
    }

    #[test]
    fn manifest_wins_over_flat_assets() {
        // A migrated note has both a manifest AND (in principle) could have
        // stray flat files; the manifest must take precedence.
        let tmp = TempDir::new().unwrap();
        let rec = recordings_dir(tmp.path(), "note1");
        touch(&rec.join("playback.wav")); // stray flat file
        append_session(&rec, "uuid-a", "", 0, vec!["mic".into()], Transcribed::Yes).unwrap();
        let sessions = resolve_sessions(&rec);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].0.id, "uuid-a");
    }

    #[test]
    fn migrate_flat_moves_assets_into_subdir_and_seeds_manifest() {
        let tmp = TempDir::new().unwrap();
        let rec = recordings_dir(tmp.path(), "note1");
        touch(&rec.join("playback.wav"));
        touch(&rec.join("timeline.jsonl"));
        touch(&rec.join("mic.wav"));

        let migrated = migrate_flat_if_needed(&rec, "legacy-uuid").unwrap();
        assert!(migrated);

        // Flat files are gone; they now live under the session subdir.
        assert!(!rec.join("playback.wav").exists());
        let sub = session_dir(&rec, "legacy-uuid");
        assert!(sub.join("playback.wav").exists());
        assert!(sub.join("timeline.jsonl").exists());
        assert!(sub.join("mic.wav").exists());

        // Manifest now lists exactly the migrated session at index 1.
        let sessions = resolve_sessions(&rec);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].0.id, "legacy-uuid");
        assert_eq!(sessions[0].0.index, 1);
        assert_eq!(sessions[0].0.streams, vec!["mic"]);
    }

    #[test]
    fn migrate_is_noop_when_manifest_present() {
        let tmp = TempDir::new().unwrap();
        let rec = recordings_dir(tmp.path(), "note1");
        append_session(&rec, "uuid-a", "", 0, vec!["mic".into()], Transcribed::Yes).unwrap();
        let migrated = migrate_flat_if_needed(&rec, "legacy-uuid").unwrap();
        assert!(!migrated);
        assert!(!session_dir(&rec, "legacy-uuid").exists());
    }

    #[test]
    fn migrate_is_noop_for_brand_new_note() {
        let tmp = TempDir::new().unwrap();
        let rec = recordings_dir(tmp.path(), "note1");
        let migrated = migrate_flat_if_needed(&rec, "legacy-uuid").unwrap();
        assert!(!migrated);
        assert!(read_manifest(&rec).is_none());
    }

    #[test]
    fn migrate_then_append_yields_two_sessions() {
        // The real second-recording flow: migrate the flat take, then append
        // the new one.
        let tmp = TempDir::new().unwrap();
        let rec = recordings_dir(tmp.path(), "note1");
        touch(&rec.join("playback.wav"));
        touch(&rec.join("timeline.jsonl"));

        migrate_flat_if_needed(&rec, "legacy-uuid").unwrap();
        let idx =
            append_session(&rec, "uuid-2", "2026-07-09T12:00:00Z", 5000, vec!["mic".into()], Transcribed::Yes)
                .unwrap();
        assert_eq!(idx, 2);
        let sessions = resolve_sessions(&rec);
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].0.id, "legacy-uuid");
        assert_eq!(sessions[1].0.id, "uuid-2");
    }

    #[test]
    fn latest_session_dir_prefers_last_then_flat_fallback() {
        let tmp = TempDir::new().unwrap();
        let rec = recordings_dir(tmp.path(), "note1");
        // Nothing recorded → flat fallback (matches the pre-feature cloud
        // download target).
        assert_eq!(latest_session_dir(&rec), rec);

        append_session(&rec, "uuid-a", "", 0, vec![], Transcribed::Yes).unwrap();
        append_session(&rec, "uuid-b", "", 0, vec![], Transcribed::Yes).unwrap();
        assert_eq!(latest_session_dir(&rec), session_dir(&rec, "uuid-b"));
    }

    #[test]
    fn unparseable_manifest_falls_back_to_flat() {
        let tmp = TempDir::new().unwrap();
        let rec = recordings_dir(tmp.path(), "note1");
        fs::create_dir_all(&rec).unwrap();
        fs::write(manifest_path(&rec), b"{not json").unwrap();
        touch(&rec.join("playback.wav"));
        let sessions = resolve_sessions(&rec);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].0.id, LEGACY_SESSION_ID);
    }

    // ---- cloud sync (#16) pure helpers -----------------------------------

    fn meta(id: &str, index: u32, deleted: bool) -> RemoteSessionMeta {
        RemoteSessionMeta {
            client_id: id.into(),
            index,
            started_at_ms: 1_719_000_000_000,
            duration_ms: 5000,
            streams: vec!["mic".into()],
            deleted,
        }
    }

    #[test]
    fn ms_to_started_at_formats_and_guards_zero() {
        // A known epoch-ms formats to a parseable RFC3339 carrying the same instant.
        let ms = 1_719_921_600_000; // 2024-07-02T12:00:00Z
        let s = ms_to_started_at(ms);
        assert!(!s.is_empty());
        assert_eq!(
            chrono::DateTime::parse_from_rfc3339(&s).unwrap().timestamp_millis(),
            ms
        );
        // 0 / negative (unknown start) collapses to empty, matching a legacy take.
        assert_eq!(ms_to_started_at(0), "");
        assert_eq!(ms_to_started_at(-5), "");
    }

    #[test]
    fn upload_plan_uploads_missing_once_and_timeline_always() {
        use AssetField::*;
        // First upload: everything present locally, nothing on the server yet.
        let plan = session_upload_plan(&[Playback, Mic, Sys, Timeline, Chunks], &[]);
        assert_eq!(plan, vec![Playback, Mic, Sys, Timeline, Chunks]);

        // A later edit: all five already on the server → only timeline re-uploads.
        let plan = session_upload_plan(
            &[Playback, Mic, Sys, Timeline, Chunks],
            &[Playback, Mic, Sys, Timeline, Chunks],
        );
        assert_eq!(plan, vec![Timeline]);

        // Playback + timeline present locally, playback already remote → timeline only.
        let plan = session_upload_plan(&[Playback, Timeline], &[Playback]);
        assert_eq!(plan, vec![Timeline]);

        // Nothing local → nothing uploaded, even if the server expects it.
        assert!(session_upload_plan(&[], &[Playback]).is_empty());
    }

    #[test]
    fn repair_plan_sends_only_what_the_server_is_missing() {
        use AssetField::*;
        // The case this exists for: the metadata record synced but the asset
        // PATCH never ran, so the server has the session and none of its files.
        let plan = session_repair_plan(&[Playback, Timeline], &[]);
        assert_eq!(plan, vec![Playback, Timeline]);

        // Everything already up → a repair pass is free (no re-send of the
        // timeline, unlike the post-recording plan).
        assert!(session_repair_plan(&[Playback, Timeline], &[Playback, Timeline]).is_empty());

        // Half-finished upload: only the gap is filled.
        assert_eq!(session_repair_plan(&[Playback, Timeline], &[Playback]), vec![Timeline]);
    }

    #[test]
    fn sync_audio_off_withholds_audio_but_never_the_timeline() {
        use AssetField::*;
        let plan = vec![Playback, Mic, Sys, Timeline, Chunks];
        assert_eq!(retain_syncable(plan.clone(), true), plan);
        // The regression this guards: `sync_audio = false` used to skip the whole
        // upload, so teammates got no word timings and therefore no speaker
        // labels. Audio is withheld; text is not.
        assert_eq!(retain_syncable(plan, false), vec![Timeline, Chunks]);
    }

    #[test]
    fn only_a_changed_timeline_is_re_fetched() {
        use AssetField::*;
        // Nothing local yet → fetch whatever the record holds.
        assert!(should_fetch_asset(Timeline, false, None, "timeline_abc.jsonl"));
        assert!(should_fetch_asset(Playback, false, None, "playback_abc.wav"));

        // Record holds no file for the field → nothing to fetch.
        assert!(!should_fetch_asset(Timeline, false, None, ""));

        // Same upload as the copy on disk → skip.
        assert!(!should_fetch_asset(
            Timeline,
            true,
            Some("timeline_abc.jsonl"),
            "timeline_abc.jsonl"
        ));
        // The author re-diarized / renamed a speaker: PocketBase minted a new
        // stored filename, so the local copy's labels are stale → re-fetch.
        assert!(should_fetch_asset(
            Timeline,
            true,
            Some("timeline_abc.jsonl"),
            "timeline_zzz.jsonl"
        ));
        // Downloaded before stamps existed → one redundant fetch, then stamped.
        assert!(should_fetch_asset(Timeline, true, None, "timeline_abc.jsonl"));

        // Audio is immutable per take and huge — fetched once, never again,
        // whatever the server's filename says.
        assert!(!should_fetch_asset(
            Playback,
            true,
            Some("playback_abc.wav"),
            "playback_zzz.wav"
        ));
    }

    #[test]
    fn reconcile_builds_manifest_sorted_by_index() {
        let m = reconcile_manifest(None, &[meta("b", 2, false), meta("a", 1, false)]);
        assert_eq!(m.sessions.len(), 2);
        assert_eq!(m.sessions[0].id, "a");
        assert_eq!(m.sessions[1].id, "b");
        assert!(!m.sessions[0].started_at.is_empty());
    }

    #[test]
    fn reconcile_tombstone_removes_entry() {
        let existing = reconcile_manifest(None, &[meta("a", 1, false), meta("b", 2, false)]);
        let after = reconcile_manifest(Some(existing), &[meta("a", 1, true)]);
        assert_eq!(after.sessions.len(), 1);
        assert_eq!(after.sessions[0].id, "b");
    }

    #[test]
    fn reconcile_preserves_local_only_and_legacy_entries() {
        // A local manifest holds an un-pushed take + a legacy synthesized entry;
        // a pull carrying an unrelated remote session must keep both locals.
        let mut existing = SessionsManifest::empty();
        existing.sessions.push(SessionEntry {
            id: "local-unpushed".into(),
            index: 1,
            started_at: String::new(),
            duration_ms: 0,
            streams: vec![],
            transcribed: true,
        });
        let after = reconcile_manifest(Some(existing), &[meta("remote", 2, false)]);
        let ids: Vec<&str> = after.sessions.iter().map(|e| e.id.as_str()).collect();
        assert!(ids.contains(&"local-unpushed"));
        assert!(ids.contains(&"remote"));
    }

    #[test]
    fn reconcile_remote_overwrites_matching_local_metadata() {
        let mut existing = SessionsManifest::empty();
        existing.sessions.push(SessionEntry {
            id: "a".into(),
            index: 1,
            started_at: String::new(),
            duration_ms: 0,
            streams: vec![],
            transcribed: true,
        });
        let after = reconcile_manifest(Some(existing), &[meta("a", 1, false)]);
        assert_eq!(after.sessions.len(), 1);
        assert_eq!(after.sessions[0].duration_ms, 5000);
        assert_eq!(after.sessions[0].streams, vec!["mic"]);
    }

    // ---- session-id path-traversal guard ---------------------------------

    #[test]
    fn is_safe_session_id_accepts_uuid_and_legacy() {
        assert!(is_safe_session_id("550e8400-e29b-41d4-a716-446655440000"));
        assert!(is_safe_session_id(LEGACY_SESSION_ID)); // "__legacy__"
        assert!(is_safe_session_id("note-123"));
    }

    #[test]
    fn is_safe_session_id_rejects_traversal_and_separators() {
        assert!(!is_safe_session_id(""));
        assert!(!is_safe_session_id(".."));
        assert!(!is_safe_session_id("../../../../Desktop"));
        assert!(!is_safe_session_id("a/b"));
        assert!(!is_safe_session_id("a\\b"));
        assert!(!is_safe_session_id("has space"));
        assert!(!is_safe_session_id("with.dot")); // dots aren't minted, reject to stay strict
    }

    #[test]
    fn reconcile_drops_hostile_remote_client_id() {
        // A malicious workspace member pushes a session whose client_id would
        // traverse out of the recordings tree. It must never enter the manifest.
        let after = reconcile_manifest(
            None,
            &[meta("../../../../Desktop", 1, false), meta("safe-uuid", 2, false)],
        );
        let ids: Vec<&str> = after.sessions.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["safe-uuid"]);
        assert!(!ids.iter().any(|id| id.contains("..")));
    }

    #[test]
    fn reconcile_sanitizes_tampered_existing_manifest() {
        // An on-disk manifest tampered with before this fix carries a hostile
        // id; reconcile must strip it rather than carry it forward.
        let mut existing = SessionsManifest::empty();
        existing.sessions.push(SessionEntry {
            id: "../../evil".into(),
            index: 1,
            started_at: String::new(),
            duration_ms: 0,
            streams: vec![],
            transcribed: true,
        });
        let after = reconcile_manifest(Some(existing), &[meta("safe", 2, false)]);
        let ids: Vec<&str> = after.sessions.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["safe"]);
    }

    #[test]
    fn resolve_sessions_skips_hostile_manifest_entries() {
        let tmp = TempDir::new().unwrap();
        let rec = recordings_dir(tmp.path(), "note1");
        let mut manifest = SessionsManifest::empty();
        manifest.sessions.push(SessionEntry {
            id: "../../../../Desktop".into(),
            index: 1,
            started_at: String::new(),
            duration_ms: 0,
            streams: vec![],
            transcribed: true,
        });
        manifest.sessions.push(SessionEntry {
            id: "550e8400-e29b-41d4-a716-446655440000".into(),
            index: 2,
            started_at: String::new(),
            duration_ms: 0,
            streams: vec![],
            transcribed: true,
        });
        write_manifest(&rec, &manifest).unwrap();

        let resolved = resolve_sessions(&rec);
        assert_eq!(resolved.len(), 1, "hostile manifest entry must be skipped");
        assert_eq!(resolved[0].0.id, "550e8400-e29b-41d4-a716-446655440000");
        // And the returned path stays inside the recordings tree.
        assert!(resolved[0].1.starts_with(&rec));
    }

    #[test]
    fn resolve_session_dir_rejects_hostile_id_via_manifest() {
        let tmp = TempDir::new().unwrap();
        let rec = recordings_dir(tmp.path(), "note1");
        let mut manifest = SessionsManifest::empty();
        manifest.sessions.push(SessionEntry {
            id: "../../../../Desktop".into(),
            index: 1,
            started_at: String::new(),
            duration_ms: 0,
            streams: vec![],
            transcribed: true,
        });
        write_manifest(&rec, &manifest).unwrap();
        // note_session_playback_path feeds a frontend id through here; a poisoned
        // manifest must not resolve to a traversal path.
        assert!(resolve_session_dir(&rec, "../../../../Desktop").is_none());
    }

    // ---- honest keep_audio (#24) ------------------------------------------

    #[test]
    fn audio_assets_are_the_three_wavs() {
        let audio: Vec<AssetField> =
            AssetField::ALL.into_iter().filter(|f| f.is_audio()).collect();
        assert_eq!(
            audio,
            vec![AssetField::Playback, AssetField::Mic, AssetField::Sys]
        );
        // The text assets are word timings and chunk timings — never audio.
        assert!(!AssetField::Timeline.is_audio());
        assert!(!AssetField::Chunks.is_audio());
    }

    #[test]
    fn retain_audio_follows_the_setting() {
        assert!(retain_audio(Some("true")));
        assert!(!retain_audio(Some("false")));
        // Unset falls back to the shipped default (off).
        assert!(!retain_audio(None));
        // Anything that isn't the literal "true" is off — the setting is stored
        // as a string and a garbled value must fail closed, not open.
        assert!(!retain_audio(Some("")));
        assert!(!retain_audio(Some("1")));
    }

    #[test]
    fn download_plan_drops_audio_when_not_storing() {
        // A device that stores audio pulls a teammate's playback + timeline.
        assert_eq!(
            session_download_plan(true),
            vec![AssetField::Playback, AssetField::Timeline]
        );
        // A device with keep_audio off pulls only the text timeline, so the
        // reader and session dividers still work with no WAV on disk.
        assert_eq!(session_download_plan(false), vec![AssetField::Timeline]);
    }

    #[test]
    fn stored_audio_lists_only_wavs_across_sessions() {
        let tmp = TempDir::new().unwrap();
        let rec = recordings_dir(tmp.path(), "note1");
        let a = rec.join("aaaaaaaa-0000-0000-0000-000000000001");
        let b = rec.join("aaaaaaaa-0000-0000-0000-000000000002");
        for dir in [&a, &b] {
            touch(&dir.join("playback.wav"));
            touch(&dir.join("mic.wav"));
            touch(&dir.join("timeline.jsonl"));
            touch(&dir.join("chunks.json"));
        }
        let mut manifest = SessionsManifest::empty();
        for (i, dir) in [&a, &b].into_iter().enumerate() {
            manifest.sessions.push(SessionEntry {
                id: dir.file_name().unwrap().to_str().unwrap().to_string(),
                index: i as u32 + 1,
                started_at: String::new(),
                duration_ms: 0,
                streams: vec![],
                transcribed: true,
            });
        }
        write_manifest(&rec, &manifest).unwrap();

        let found = stored_audio_files(&rec);
        assert_eq!(found.len(), 4, "two sessions × playback + mic: {found:?}");
        assert!(found.iter().all(|p| p.extension().unwrap() == "wav"));
        assert!(found.iter().any(|p| p.ends_with("playback.wav")));
        assert!(found.iter().any(|p| p.ends_with("mic.wav")));
    }

    #[test]
    fn stored_audio_finds_wavs_no_manifest_mentions() {
        // The manifest wins over flat assets for *reading* (see
        // manifest_wins_over_flat_assets), so a WAV in an unlisted dir — a
        // tombstoned session pulled from the cloud, or a stray flat file left
        // beside a migrated note — is invisible to the player. A privacy sweep
        // must still see it: it is audio on disk.
        let tmp = TempDir::new().unwrap();
        let rec = recordings_dir(tmp.path(), "note1");
        let listed = rec.join("aaaaaaaa-0000-0000-0000-000000000001");
        touch(&listed.join("playback.wav"));
        let mut manifest = SessionsManifest::empty();
        manifest.sessions.push(SessionEntry {
            id: listed.file_name().unwrap().to_str().unwrap().to_string(),
            index: 1,
            started_at: String::new(),
            duration_ms: 0,
            streams: vec![],
            transcribed: true,
        });
        write_manifest(&rec, &manifest).unwrap();
        // Neither of these is reachable through resolve_sessions.
        touch(&rec.join("playback.wav")); // stray flat file
        touch(&rec.join("bbbbbbbb-0000-0000-0000-000000000002").join("mic.wav"));

        let found = stored_audio_files(&rec);
        assert_eq!(found.len(), 3, "{found:?}");
        assert_eq!(delete_stored_audio(&rec), 3);
        assert!(stored_audio_files(&rec).is_empty());
    }

    #[test]
    fn session_has_audio_ignores_the_text_assets() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("s1");
        touch(&dir.join("chunks.json"));
        touch(&dir.join("timeline.jsonl"));
        // A take whose chunks all failed to transcribe still wrote its WAVs;
        // that is the case that must NOT be discarded.
        assert!(!session_has_audio(&dir));
        touch(&dir.join("mic.wav"));
        assert!(session_has_audio(&dir));
    }

    #[test]
    fn stored_audio_covers_a_flat_legacy_note() {
        let tmp = TempDir::new().unwrap();
        let rec = recordings_dir(tmp.path(), "note1");
        touch(&rec.join("playback.wav"));
        touch(&rec.join("timeline.jsonl"));
        let found = stored_audio_files(&rec);
        assert_eq!(found, vec![rec.join("playback.wav")]);
    }

    #[test]
    fn audio_totals_count_notes_with_audio_only() {
        let tmp = TempDir::new().unwrap();
        // note1: has audio. note2: transcript-only (timeline but no WAV).
        touch(&recordings_dir(tmp.path(), "note1").join("playback.wav"));
        touch(&recordings_dir(tmp.path(), "note1").join("mic.wav"));
        touch(&recordings_dir(tmp.path(), "note2").join("timeline.jsonl"));

        let stats = stored_audio_totals(&recordings_root(tmp.path()));
        assert_eq!(stats.notes, 1, "note2 has no audio to delete");
        assert_eq!(stats.files, 2);
        assert_eq!(stats.bytes, 2, "touch() writes one byte per file");
        assert_eq!(stats.note_ids, vec!["note1".to_string()]);
    }

    #[test]
    fn audio_totals_are_empty_before_any_recording() {
        let tmp = TempDir::new().unwrap();
        let stats = stored_audio_totals(&recordings_root(tmp.path()));
        assert_eq!(stats.notes, 0);
        assert_eq!(stats.bytes, 0);
        assert!(stats.note_ids.is_empty());
    }

    #[test]
    fn delete_stored_audio_keeps_the_text_assets() {
        let tmp = TempDir::new().unwrap();
        let rec = recordings_dir(tmp.path(), "note1");
        touch(&rec.join("playback.wav"));
        touch(&rec.join("mic.wav"));
        touch(&rec.join("timeline.jsonl"));
        touch(&rec.join("chunks.json"));

        let removed = delete_stored_audio(&rec);
        assert_eq!(removed, 2);
        assert!(!rec.join("playback.wav").exists());
        assert!(!rec.join("mic.wav").exists());
        // Timelines and chunk timings are text; they survive so the reader,
        // session dividers and re-diarize anchors aren't destroyed with the audio.
        assert!(rec.join("timeline.jsonl").exists());
        assert!(rec.join("chunks.json").exists());
        // Idempotent: a second pass has nothing left to remove.
        assert_eq!(delete_stored_audio(&rec), 0);
    }

    // -----------------------------------------------------------------------
    // Deferred transcription (#146)
    // -----------------------------------------------------------------------

    #[test]
    fn manifest_written_before_the_field_reads_back_as_transcribed() {
        // The upgrade case, and the one that matters most: every take in every
        // existing library predates `transcribed`. Reading them back as
        // untranscribed would offer to re-transcribe (and so re-diarize and
        // re-number) the whole library.
        let tmp = TempDir::new().unwrap();
        let rec = recordings_dir(tmp.path(), "note1");
        fs::create_dir_all(&rec).unwrap();
        fs::write(
            manifest_path(&rec),
            r#"{"version":1,"sessions":[{"id":"uuid-a","index":1,"startedAt":"","durationMs":0,"streams":["mic"]}]}"#,
        )
        .unwrap();
        let sessions = resolve_sessions(&rec);
        assert_eq!(sessions.len(), 1);
        assert!(sessions[0].0.transcribed);
        assert!(untranscribed_sessions(&rec).is_empty());
    }

    #[test]
    fn append_records_an_untranscribed_take_and_resolve_round_trips_it() {
        let tmp = TempDir::new().unwrap();
        let rec = recordings_dir(tmp.path(), "note1");
        append_session(&rec, "uuid-a", "", 0, vec!["mic".into()], Transcribed::No).unwrap();
        // Survives the JSON round-trip, not just the in-memory struct.
        let pending = untranscribed_sessions(&rec);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].0.id, "uuid-a");
        assert_eq!(pending[0].1, session_dir(&rec, "uuid-a"));
    }

    #[test]
    fn untranscribed_sessions_come_back_in_recording_order() {
        // Each take's timeline is numbered off the takes before it, so the
        // deferred pass has to walk them oldest-first.
        let tmp = TempDir::new().unwrap();
        let rec = recordings_dir(tmp.path(), "note1");
        append_session(&rec, "take-1", "", 0, vec![], Transcribed::No).unwrap();
        append_session(&rec, "take-2", "", 0, vec![], Transcribed::Yes).unwrap();
        append_session(&rec, "take-3", "", 0, vec![], Transcribed::No).unwrap();
        let ids: Vec<String> = untranscribed_sessions(&rec)
            .into_iter()
            .map(|(e, _)| e.id)
            .collect();
        assert_eq!(ids, vec!["take-1".to_string(), "take-3".to_string()]);
    }

    #[test]
    fn mark_transcribed_clears_one_take_and_leaves_the_others() {
        let tmp = TempDir::new().unwrap();
        let rec = recordings_dir(tmp.path(), "note1");
        append_session(&rec, "take-1", "", 0, vec![], Transcribed::No).unwrap();
        append_session(&rec, "take-2", "", 0, vec![], Transcribed::No).unwrap();
        assert!(mark_session_transcribed(&rec, "take-1").unwrap());
        let ids: Vec<String> = untranscribed_sessions(&rec)
            .into_iter()
            .map(|(e, _)| e.id)
            .collect();
        assert_eq!(ids, vec!["take-2".to_string()]);
        // Idempotent, and honest about an id the manifest doesn't hold.
        assert!(mark_session_transcribed(&rec, "take-1").unwrap());
        assert!(!mark_session_transcribed(&rec, "nope").unwrap());
    }

    #[test]
    fn mark_transcribed_reports_false_with_no_manifest() {
        // A legacy flat note has no manifest to correct. Inventing an entry
        // here would hide the flat take behind a manifest that doesn't
        // describe it.
        let tmp = TempDir::new().unwrap();
        let rec = recordings_dir(tmp.path(), "note1");
        touch(&rec.join("playback.wav"));
        assert!(!mark_session_transcribed(&rec, LEGACY_SESSION_ID).unwrap());
        assert!(read_manifest(&rec).is_none());
    }

    #[test]
    fn legacy_flat_session_is_never_offered_for_transcription() {
        let tmp = TempDir::new().unwrap();
        let rec = recordings_dir(tmp.path(), "note1");
        touch(&rec.join("playback.wav"));
        touch(&rec.join("mic.wav"));
        assert_eq!(resolve_sessions(&rec).len(), 1);
        assert!(untranscribed_sessions(&rec).is_empty());
    }

    #[test]
    fn reconcile_keeps_a_local_takes_untranscribed_flag() {
        // The server has no field for it, so a cloud pull that rewrites the
        // entry must not silently declare the take transcribed — the audio
        // would stay on disk with no way left to ask for its text.
        let mut existing = SessionsManifest::empty();
        existing.sessions.push(SessionEntry {
            id: "a".into(),
            index: 1,
            started_at: String::new(),
            duration_ms: 0,
            streams: vec![],
            transcribed: false,
        });
        let after = reconcile_manifest(Some(existing), &[meta("a", 1, false)]);
        assert_eq!(after.sessions.len(), 1);
        assert!(!after.sessions[0].transcribed);
    }

    #[test]
    fn reconcile_treats_a_newly_arriving_remote_session_as_transcribed() {
        // A teammate's take: its text arrived with the note, and this device
        // has no audio to replay even if it wanted to.
        let after = reconcile_manifest(None, &[meta("remote", 1, false)]);
        assert!(after.sessions[0].transcribed);
    }

    #[test]
    fn defer_transcription_needs_both_the_setting_and_retention() {
        // The feature is inert while `keep_audio` is off — deferring there
        // would mean discarding the meeting (#24 stays the outer gate).
        assert!(defer_transcription(Some("true"), true));
        assert!(!defer_transcription(Some("true"), false));
        assert!(!defer_transcription(Some("false"), true));
        // Unset defaults off, whatever retention says.
        assert!(!defer_transcription(None, true));
        // Anything that isn't the literal "true" is off, matching how every
        // other boolean setting is read.
        assert!(!defer_transcription(Some("yes"), true));
    }

    #[test]
    fn only_the_raw_streams_count_as_replayable() {
        // #146: a take reduced to its mixed playback.wav can't be replayed —
        // the deferred pass reads mic.wav / sys.wav, and the two streams are
        // kept separate end-to-end so each is diarized on its own. Offering
        // Transcribe for it would be a button that silently does nothing.
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("s1");
        touch(&dir.join("playback.wav"));
        touch(&dir.join("timeline.jsonl"));
        assert!(session_has_audio(&dir), "playback.wav is still audio on disk");
        assert!(!session_has_replayable_audio(&dir));

        touch(&dir.join("sys.wav"));
        assert!(session_has_replayable_audio(&dir));
    }
}
