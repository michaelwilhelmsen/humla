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
) -> std::io::Result<u32> {
    let mut manifest = read_manifest(recordings_dir).unwrap_or_else(SessionsManifest::empty);
    let index = manifest.sessions.iter().map(|e| e.index).max().unwrap_or(0) + 1;
    manifest.sessions.push(SessionEntry {
        id: id.to_string(),
        index,
        started_at: started_at.to_string(),
        duration_ms,
        streams,
    });
    write_manifest(recordings_dir, &manifest)?;
    Ok(index)
}

/// Number of sessions already recorded (manifest entries, or 1 for a legacy
/// flat note). Used to decide auto-retention: once a note gains a *second*
/// session, source WAVs are kept regardless of the `keep_audio` setting.
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
        by_id.insert(
            r.client_id.clone(),
            SessionEntry {
                id: r.client_id.clone(),
                index: r.index,
                started_at: ms_to_started_at(r.started_at_ms),
                duration_ms: r.duration_ms,
                streams: r.streams.clone(),
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
        append_session(&rec, "uuid-a", "", 0, vec!["mic".into()]).unwrap();
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
        append_session(&rec, "uuid-a", "", 0, vec![]).unwrap();
        append_session(&rec, "uuid-b", "", 0, vec![]).unwrap();
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
        let i1 = append_session(&rec, "uuid-a", "2026-07-09T10:00:00Z", 1000, vec!["mic".into()])
            .unwrap();
        let i2 = append_session(
            &rec,
            "uuid-b",
            "2026-07-09T11:00:00Z",
            2000,
            vec!["mic".into(), "sys".into()],
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
                },
                SessionEntry {
                    id: "first".into(),
                    index: 1,
                    started_at: String::new(),
                    duration_ms: 0,
                    streams: vec![],
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
        append_session(&rec, "uuid-a", "", 0, vec!["mic".into()]).unwrap();
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
        append_session(&rec, "uuid-a", "", 0, vec!["mic".into()]).unwrap();
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
        let idx = append_session(&rec, "uuid-2", "2026-07-09T12:00:00Z", 5000, vec!["mic".into()])
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

        append_session(&rec, "uuid-a", "", 0, vec![]).unwrap();
        append_session(&rec, "uuid-b", "", 0, vec![]).unwrap();
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
        });
        manifest.sessions.push(SessionEntry {
            id: "550e8400-e29b-41d4-a716-446655440000".into(),
            index: 2,
            started_at: String::new(),
            duration_ms: 0,
            streams: vec![],
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
        });
        write_manifest(&rec, &manifest).unwrap();
        // note_session_playback_path feeds a frontend id through here; a poisoned
        // manifest must not resolve to a traversal path.
        assert!(resolve_session_dir(&rec, "../../../../Desktop").is_none());
    }
}
