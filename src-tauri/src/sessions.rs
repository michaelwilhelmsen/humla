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
pub fn write_manifest(recordings_dir: &Path, manifest: &SessionsManifest) -> std::io::Result<()> {
    std::fs::create_dir_all(recordings_dir)?;
    let body = serde_json::to_string_pretty(manifest)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(manifest_path(recordings_dir), body)
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
}
