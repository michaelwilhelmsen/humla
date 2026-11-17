//! Note export (issue #18). Assembles a note's selected content — summary,
//! transcript, typed notes — into a single Markdown, plain-text or PDF
//! document and writes it to a user-chosen path. Also saves a note's stored
//! audio as one file (#224), every take's `playback.wav` joined in order.
//!
//! The path is chosen on the frontend via the dialog plugin's native save
//! panel and passed in as `spec.path`. Unlike `open_in_finder`, the write is
//! intentionally NOT constrained to the app data directory — the whole point
//! is to let the user save anywhere they picked in the panel.

use super::err;
use crate::db;
use crate::export_doc::{self, Block};
use crate::html_text::html_to_text;
use crate::AppState;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExportFormat {
    Markdown,
    Txt,
    Pdf,
}

/// What to export and how. Mirrors the frontend `ExportSpec` type. `path` is
/// the destination chosen in the native save panel.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportSpec {
    pub path: String,
    pub format: ExportFormat,
    pub include_summary: bool,
    pub include_transcript: bool,
    pub include_notes: bool,
    /// When false, the leading `Label: ` prefix is stripped from each
    /// transcript line so the export reads as plain prose.
    pub include_speaker_labels: bool,
}

/// A section's content, before it is rendered for a format.
enum Content {
    /// The summary, which the model writes as Markdown.
    Markdown(String),
    Transcript(String),
    /// The typed notes, already reduced to plain text.
    Notes(String),
}

impl Content {
    fn raw(&self) -> &str {
        match self {
            Content::Markdown(s) | Content::Transcript(s) | Content::Notes(s) => s,
        }
    }

    fn blocks(&self, labelled: bool) -> Vec<Block> {
        match self {
            Content::Markdown(s) => export_doc::parse_markdown(s),
            Content::Transcript(s) => export_doc::transcript_turns(s, labelled),
            Content::Notes(s) => export_doc::plain_lines(s),
        }
    }
}

fn display_title(title: &str) -> &str {
    if title.trim().is_empty() { "Untitled note" } else { title.trim() }
}

fn sections(summary: &str, transcript: &str, body_html: &str, spec: &ExportSpec) -> Vec<(&'static str, Content)> {
    let mut out = Vec::new();
    if spec.include_summary {
        out.push(("Summary", Content::Markdown(summary.trim().to_string())));
    }
    if spec.include_transcript {
        let body = if spec.include_speaker_labels {
            transcript.trim().to_string()
        } else {
            transcript
                .lines()
                .map(|l| export_doc::split_label(l).map_or(l, |(_, text)| text))
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_string()
        };
        out.push(("Transcript", Content::Transcript(body)));
    }
    if spec.include_notes {
        out.push(("Notes", Content::Notes(html_to_text(body_html).trim().to_string())));
    }
    out
}

/// Markdown uses ATX `#`/`##`; plain text underlines the text with `=`
/// (title) or `-` (section) so the structure survives without markup.
fn heading(markdown: bool, level: u8, text: &str) -> String {
    if markdown {
        return format!("{} {}", "#".repeat(level as usize), text);
    }
    let bar = if level <= 1 { '=' } else { '-' };
    let underline: String = std::iter::repeat_n(bar, text.chars().count().max(1)).collect();
    format!("{text}\n{underline}")
}

/// Assemble a Markdown or plain-text export. Pure — no I/O, so the whole
/// content contract is unit-testable without a database or a filesystem.
/// `body_html` is the raw Tiptap HTML. A checked-but-empty section still
/// emits its heading with an explicit empty marker, so the user sees it was
/// included and had no content.
pub(crate) fn build_export_document(
    title: &str,
    summary: &str,
    transcript: &str,
    body_html: &str,
    spec: &ExportSpec,
) -> String {
    let markdown = spec.format == ExportFormat::Markdown;
    let rendered: Vec<String> = sections(summary, transcript, body_html, spec)
        .into_iter()
        .map(|(name, content)| {
            let body = if content.raw().is_empty() {
                if markdown { "_(empty)_" } else { "(empty)" }.to_string()
            } else if markdown || matches!(content, Content::Transcript(_)) {
                content.raw().to_string()
            } else {
                export_doc::render_text(&content.blocks(false))
            };
            format!("{}\n\n{body}", heading(markdown, 2, name))
        })
        .collect();

    let mut doc = heading(markdown, 1, display_title(title));
    if !rendered.is_empty() {
        doc.push_str("\n\n");
        doc.push_str(&rendered.join("\n\n"));
    }
    doc.push('\n');
    doc
}

/// Lay out a PDF export from the same sections the text formats use.
pub(crate) fn build_export_pdf(
    title: &str,
    summary: &str,
    transcript: &str,
    body_html: &str,
    spec: &ExportSpec,
) -> Result<Vec<u8>, String> {
    let sections: Vec<export_doc::Section> = sections(summary, transcript, body_html, spec)
        .into_iter()
        .map(|(name, content)| export_doc::Section {
            name: name.to_string(),
            blocks: content.blocks(spec.include_speaker_labels),
        })
        .collect();
    export_doc::render_pdf(display_title(title), &sections)
}

/// Export a note to the path chosen in the native save panel.
#[tauri::command]
pub fn export_note(app: AppHandle, note_id: String, spec: ExportSpec) -> Result<(), String> {
    let state: tauri::State<AppState> = app.state();
    let note = {
        let conn = state.db.lock();
        db::get_note(&conn, &note_id).map_err(err)?
    };
    let bytes = match spec.format {
        ExportFormat::Pdf => {
            build_export_pdf(&note.title, &note.summary, &note.transcript, &note.body, &spec)?
        }
        ExportFormat::Markdown | ExportFormat::Txt => {
            build_export_document(&note.title, &note.summary, &note.transcript, &note.body, &spec)
                .into_bytes()
        }
    };
    std::fs::write(&spec.path, bytes).map_err(|e| format!("write export file: {e}"))?;
    Ok(())
}

/// The file a note's audio is saved as. Mirrors `AudioFormat` in `ipc.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioFormat {
    Wav,
    /// AAC, through the `afconvert` every Mac ships: an hour of the 16 kHz
    /// mono WAV is ~115 MB, which is awkward to mail or upload.
    M4a,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioExportSpec {
    pub path: String,
    pub format: AudioFormat,
}

/// One take as ⋯ → Save audio… lists it before it asks where: its number, and
/// its length while its audio is still on this device. `None` is a take whose
/// audio is gone, which the file leaves out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TakeAudio {
    pub index: u32,
    pub duration_ms: Option<u64>,
}

/// A note's takes in order, each with the playback audio it still has and that
/// file's length. Index 0 is the "Earlier transcript" repair session, which was
/// never a take and has no audio, so it isn't listed. The playback audio is the
/// file the player plays: both streams mixed, and the one a take keeps
/// whichever way it was captured, imported or synced. A file the join couldn't
/// copy through counts as missing, so the list and the saved file always agree.
fn note_audio_takes(recordings: &Path) -> Vec<(u32, Option<(PathBuf, u64)>)> {
    crate::sessions::resolve_sessions(recordings)
        .into_iter()
        .filter(|(entry, _)| entry.index > 0)
        .map(|(entry, dir)| {
            let audio = crate::sessions::playback_file(&dir).and_then(|path| {
                let ms = if is_m4a(&path) {
                    m4a_duration_ms(&path)
                } else {
                    crate::wav::pcm16_mono_16k_data(&path).ok().map(|(_, bytes)| bytes / 32)
                }?;
                Some((path, ms))
            });
            (entry.index, audio)
        })
        .collect()
}

fn is_m4a(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("m4a"))
}

/// A take's AAC playback length, from `afinfo`. Only 16 kHz mono counts, the
/// format the join decodes it to.
fn m4a_duration_ms(path: &Path) -> Option<u64> {
    let out = std::process::Command::new("/usr/bin/afinfo").arg(path).output().ok()?;
    if !out.status.success() {
        return None;
    }
    afinfo_duration_ms(&String::from_utf8_lossy(&out.stdout))
}

/// `afinfo` reports the frames left once the encoder's priming and padding are
/// trimmed, which is what a decode gives back.
fn afinfo_duration_ms(text: &str) -> Option<u64> {
    let format = text.lines().find(|l| l.trim_start().starts_with("Data format:"))?;
    if !(format.contains(" 1 ch,") && format.contains(" 16000 Hz")) {
        return None;
    }
    let frames: u64 = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("audio ")?.split_once(" valid frames"))?
        .0
        .trim()
        .parse()
        .ok()?;
    Some(frames / 16)
}

pub(crate) fn note_audio_info(recordings: &Path) -> Vec<TakeAudio> {
    note_audio_takes(recordings)
        .into_iter()
        .map(|(index, audio)| TakeAudio { index, duration_ms: audio.map(|(_, ms)| ms) })
        .collect()
}

/// The files a save joins, in take order.
pub(crate) fn note_audio_sources(recordings: &Path) -> Vec<PathBuf> {
    note_audio_takes(recordings)
        .into_iter()
        .filter_map(|(_, audio)| audio.map(|(path, _)| path))
        .collect()
}

/// A directory under the system temp dir, removed with everything in it when
/// dropped, so a failed save leaves nothing behind.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Join `sources` into one file at `dest`. It is built in a scratch dir and
/// only copied over `dest` once it is whole, so a failure never leaves a
/// half-written file where the user asked for one.
pub(crate) fn write_note_audio(sources: &[PathBuf], format: AudioFormat, dest: &Path) -> Result<(), String> {
    if sources.is_empty() {
        return Err("This note has no audio stored on this device.".into());
    }
    let scratch = Scratch(std::env::temp_dir().join(format!("humla-audio-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir_all(&scratch.0).map_err(|e| format!("prepare audio: {e}"))?;
    let mut pcm = Vec::with_capacity(sources.len());
    for (i, source) in sources.iter().enumerate() {
        if !is_m4a(source) {
            pcm.push(source.clone());
            continue;
        }
        let decoded = scratch.0.join(format!("take-{i}.wav"));
        let result = std::process::Command::new("/usr/bin/afconvert")
            .args(["-f", "WAVE", "-d", "LEI16@16000"])
            .arg(source)
            .arg(&decoded)
            .output()
            .map_err(|e| format!("decode audio: {e}"))?;
        if !result.status.success() {
            let stderr = String::from_utf8_lossy(&result.stderr);
            return Err(format!("decode audio: {}", stderr.trim()));
        }
        pcm.push(decoded);
    }
    let wav = scratch.0.join("audio.wav");
    crate::wav::concat_pcm16_mono_16k(&pcm, &wav).map_err(|e| format!("join audio: {e}"))?;
    let out = match format {
        AudioFormat::Wav => wav,
        AudioFormat::M4a => {
            let m4a = scratch.0.join("audio.m4a");
            let result = std::process::Command::new("/usr/bin/afconvert")
                .args(["-f", "m4af", "-d", "aac"])
                .arg(&wav)
                .arg(&m4a)
                .output()
                .map_err(|e| format!("convert audio to M4A: {e}"))?;
            if !result.status.success() {
                let stderr = String::from_utf8_lossy(&result.stderr);
                return Err(format!("convert audio to M4A: {}", stderr.trim()));
            }
            m4a
        }
    };
    std::fs::copy(&out, dest).map_err(|e| format!("write audio file: {e}"))?;
    Ok(())
}

fn note_recordings(app: &AppHandle, note_id: &str) -> Result<PathBuf, String> {
    if !crate::sessions::is_safe_session_id(note_id) {
        return Err("unsafe note id".into());
    }
    let app_dir = app.path().app_data_dir().map_err(err)?;
    Ok(crate::sessions::recordings_dir(&app_dir, note_id))
}

/// What ⋯ → Save audio… would save. `async` + `spawn_blocking` for the same
/// reason as the save: a synchronous command runs on the main thread.
#[tauri::command]
pub async fn note_audio_export_info(app: AppHandle, note_id: String) -> Result<Vec<TakeAudio>, String> {
    let recordings = note_recordings(&app, &note_id)?;
    let forbidden = super::audio_forbidden(&app);
    tauri::async_runtime::spawn_blocking(move || without_audio_if(forbidden, note_audio_info(&recordings)))
        .await
        .map_err(|e| format!("audio info task failed: {e}"))
}

/// Under `audio_forbidden`, every take reads as having no audio to save.
fn without_audio_if(forbidden: bool, takes: Vec<TakeAudio>) -> Vec<TakeAudio> {
    if !forbidden {
        return takes;
    }
    takes.into_iter().map(|t| TakeAudio { duration_ms: None, ..t }).collect()
}

/// Save a note's audio to the path chosen in the native save panel. Like
/// `export_note`, the destination is not constrained to the app data dir.
///
/// This writes no audio Humla keeps: it copies out what `keep_audio` already
/// let this device store, at the user's request, to a place the user chose.
#[tauri::command]
pub async fn export_note_audio(app: AppHandle, note_id: String, spec: AudioExportSpec) -> Result<(), String> {
    if super::audio_forbidden(&app) {
        return Err("Your organisation doesn't allow saving audio.".into());
    }
    let recordings = note_recordings(&app, &note_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let sources = note_audio_sources(&recordings);
        write_note_audio(&sources, spec.format, Path::new(&spec.path))
    })
    .await
    .map_err(|e| format!("save audio task failed: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(format: ExportFormat) -> ExportSpec {
        ExportSpec {
            path: "/tmp/x".into(),
            format,
            include_summary: true,
            include_transcript: true,
            include_notes: false,
            include_speaker_labels: true,
        }
    }

    /// A note's recordings dir holding the repair session (index 0, no audio),
    /// three takes listed out of order, and the second of them with no audio.
    async fn recorded_note(dir: &Path) -> PathBuf {
        let recordings = dir.join("recordings").join("n1");
        let session = |id: &str, index: u32| crate::sessions::SessionEntry {
            id: id.into(),
            index,
            started_at: String::new(),
            duration_ms: 0,
            streams: vec!["mic".into()],
            transcribed: true,
        };
        let manifest = crate::sessions::SessionsManifest {
            version: 1,
            sessions: vec![session("third", 3), session("earlier", 0), session("first", 1), session("second", 2)],
        };
        crate::sessions::write_manifest(&recordings, &manifest).unwrap();
        for (id, samples) in [("first", vec![1i16; 16_000]), ("third", vec![3i16; 8_000])] {
            std::fs::create_dir_all(recordings.join(id)).unwrap();
            crate::wav::write_i16_mono_16k(&recordings.join(id).join("playback.wav"), &samples)
                .await
                .unwrap();
        }
        std::fs::create_dir_all(recordings.join("earlier")).unwrap();
        recordings
    }

    #[tokio::test]
    async fn audio_lists_every_take_and_joins_the_ones_still_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let recordings = recorded_note(dir.path()).await;
        let take = |index, duration_ms| TakeAudio { index, duration_ms };
        assert_eq!(
            note_audio_info(&recordings),
            vec![take(1, Some(1_000)), take(2, None), take(3, Some(500))],
            "in take order, the repair session left out, the take with no audio missing"
        );
        assert_eq!(
            note_audio_sources(&recordings),
            vec![recordings.join("first/playback.wav"), recordings.join("third/playback.wav")]
        );
        // A file the join would refuse is missing too, so the list can't
        // promise a take the save then fails on.
        std::fs::write(recordings.join("third/playback.wav"), b"not audio").unwrap();
        assert_eq!(note_audio_info(&recordings)[2], take(3, None));
        assert_eq!(note_audio_sources(&recordings), vec![recordings.join("first/playback.wav")]);
        assert!(note_audio_info(&dir.path().join("recordings/never-recorded")).is_empty());
    }

    #[tokio::test]
    async fn audio_saves_every_take_as_one_wav() {
        let dir = tempfile::tempdir().unwrap();
        let recordings = recorded_note(dir.path()).await;
        let sources = note_audio_sources(&recordings);
        let dest = dir.path().join("Weekly sync.wav");
        // Saving over a file the panel already confirmed replacing.
        std::fs::write(&dest, b"old").unwrap();
        write_note_audio(&sources, AudioFormat::Wav, &dest).unwrap();
        let saved = crate::wav::read_i16_mono_16k(&dest).await.unwrap();
        assert!(saved == [vec![1i16; 16_000], vec![3i16; 8_000]].concat());
    }

    // Runs `/usr/bin/afconvert`, which only macOS has.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn audio_joins_aac_takes_beside_wav_ones() {
        let dir = tempfile::tempdir().unwrap();
        let recordings = recorded_note(dir.path()).await;
        // Take 3 as a take since #226 holds it: AAC, with no WAV beside it.
        let third = recordings.join("third");
        let tone: Vec<i16> = (0..8_000).map(|i| ((i as f32 * 0.07).sin() * 8_000.0) as i16).collect();
        crate::wav::write_i16_mono_16k(&third.join("playback.wav"), &tone).await.unwrap();
        let status = std::process::Command::new("/usr/bin/afconvert")
            .args(["-f", "m4af", "-d", "aac", "-b", "32000"])
            .arg(third.join("playback.wav"))
            .arg(third.join("playback.m4a"))
            .status()
            .unwrap();
        assert!(status.success());
        std::fs::remove_file(third.join("playback.wav")).unwrap();

        assert_eq!(note_audio_info(&recordings)[2], TakeAudio { index: 3, duration_ms: Some(500) });
        let sources = note_audio_sources(&recordings);
        assert_eq!(sources[1], third.join("playback.m4a"));
        let dest = dir.path().join("out.wav");
        write_note_audio(&sources, AudioFormat::Wav, &dest).unwrap();
        let saved = crate::wav::read_i16_mono_16k(&dest).await.unwrap();
        assert_eq!(saved.len(), 16_000 + 8_000, "the AAC take decodes to its own length");
        assert!(saved[..16_000].iter().all(|&s| s == 1));
    }

    #[test]
    fn afinfo_length_is_its_valid_frames() {
        let text = "File type ID:   m4af\nData format:     1 ch,  16000 Hz, aac  (0x00000000) 0 bits/channel\n\
                    estimated duration: 2153.983313 sec\n\
                    audio 34463733 valid frames + 2112 priming + 971 remainder = 34466816\n";
        assert_eq!(afinfo_duration_ms(text), Some(2_153_983));
        let stereo = text.replace(" 1 ch,", " 2 ch,");
        assert_eq!(afinfo_duration_ms(&stereo), None, "the join only takes mono");
        assert_eq!(afinfo_duration_ms("Data format:     1 ch,  16000 Hz, aac\n"), None);
    }

    #[test]
    fn audio_save_with_nothing_stored_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.wav");
        let e = write_note_audio(&[], AudioFormat::Wav, &dest).unwrap_err();
        assert!(e.contains("no audio"), "{e}");
        assert!(!dest.exists());
        // A source that isn't the format the join copies through fails before
        // anything reaches the destination.
        let bad = dir.path().join("bad.wav");
        std::fs::write(&bad, b"not audio").unwrap();
        assert!(write_note_audio(&[bad], AudioFormat::Wav, &dest).is_err());
        assert!(!dest.exists());
    }

    #[test]
    fn markdown_has_title_and_section_headers() {
        let s = spec(ExportFormat::Markdown);
        let out = build_export_document(
            "Weekly sync",
            "- Ship v1",
            "You: hello\nSpeaker 1: hi",
            "",
            &s,
        );
        assert!(out.starts_with("# Weekly sync\n"), "title heading: {out:?}");
        assert!(out.contains("## Summary\n\n- Ship v1"));
        assert!(out.contains("## Transcript\n\nYou: hello\nSpeaker 1: hi"));
        assert!(out.ends_with('\n'));
    }

    #[test]
    fn plain_text_underlines_headings() {
        let s = spec(ExportFormat::Txt);
        let out = build_export_document("Sync", "Done", "You: hey", "", &s);
        assert!(out.starts_with("Sync\n====\n"), "title underline: {out:?}");
        assert!(out.contains("Summary\n-------\n\nDone"));
    }

    #[test]
    fn plain_text_carries_no_markdown() {
        let mut s = spec(ExportFormat::Txt);
        s.include_notes = true;
        let out = build_export_document(
            "Sync",
            "## Beslutninger\n\n- **Eier:** Hege\n- Frist *fredag*",
            "You: hey",
            "<ul><li>typed</li></ul>",
            &s,
        );
        assert!(
            out.contains("Summary\n-------\n\nBeslutninger\n\n• Eier: Hege\n• Frist fredag\n\n"),
            "{out:?}"
        );
        assert!(out.contains("Transcript\n----------\n\nYou: hey"), "{out:?}");
        assert!(out.contains("Notes\n-----\n\n• typed\n"), "{out:?}");
        assert!(!out.contains(['#', '*']), "{out:?}");
    }

    #[test]
    fn pdf_export_is_a_pdf() {
        let mut s = spec(ExportFormat::Pdf);
        s.include_notes = true;
        let pdf = build_export_pdf("Sync", "- **Ship** v1", "You: hey", "<p>typed</p>", &s).unwrap();
        assert!(pdf.starts_with(b"%PDF-"));
    }

    #[test]
    fn labels_stripped_when_toggle_off() {
        let mut s = spec(ExportFormat::Markdown);
        s.include_summary = false;
        s.include_speaker_labels = false;
        let out = build_export_document(
            "T",
            "",
            "Speaker 1: hello there\nMichael: how are you",
            "",
            &s,
        );
        assert!(out.contains("## Transcript\n\nhello there\nhow are you"), "{out:?}");
        assert!(!out.contains("Speaker 1:"));
        assert!(!out.contains("Michael:"));
    }

    #[test]
    fn labels_kept_when_toggle_on() {
        let mut s = spec(ExportFormat::Markdown);
        s.include_summary = false;
        s.include_speaker_labels = true;
        let out = build_export_document("T", "", "Speaker 1: hello", "", &s);
        assert!(out.contains("Speaker 1: hello"));
    }

    #[test]
    fn mid_sentence_colon_is_preserved_when_stripping() {
        // A spoken colon far into the line is not a label — leave the line intact
        // when it has no short leading `Label: ` segment... but our heuristic
        // strips the first short prefix. Guard the realistic case: a genuine
        // label followed by text containing a colon keeps the tail's colon.
        let mut s = spec(ExportFormat::Markdown);
        s.include_summary = false;
        s.include_speaker_labels = false;
        let out = build_export_document("T", "", "You: the plan is: ship it", "", &s);
        assert!(out.contains("the plan is: ship it"), "{out:?}");
    }

    #[test]
    fn notes_converted_from_html() {
        let mut s = spec(ExportFormat::Markdown);
        s.include_summary = false;
        s.include_transcript = false;
        s.include_notes = true;
        let out = build_export_document(
            "T",
            "",
            "",
            "<p>First para</p><ul><li>one</li><li>two</li></ul>",
            &s,
        );
        assert!(out.contains("## Notes"));
        assert!(out.contains("First para"));
        assert!(out.contains("- one"));
        assert!(out.contains("- two"));
        assert!(!out.contains("<p>"), "html should be stripped: {out:?}");
    }

    #[test]
    fn empty_checked_section_shows_placeholder() {
        let mut s = spec(ExportFormat::Markdown);
        s.include_transcript = false;
        // summary included but empty
        let out = build_export_document("T", "   ", "", "", &s);
        assert!(out.contains("## Summary\n\n_(empty)_"), "{out:?}");
    }

    #[test]
    fn empty_title_falls_back() {
        let s = spec(ExportFormat::Markdown);
        let out = build_export_document("", "s", "t", "", &s);
        assert!(out.starts_with("# Untitled note"));
    }

    #[test]
    fn nothing_selected_yields_just_the_title() {
        let mut s = spec(ExportFormat::Markdown);
        s.include_summary = false;
        s.include_transcript = false;
        s.include_notes = false;
        let out = build_export_document("Solo", "s", "t", "b", &s);
        assert_eq!(out, "# Solo\n");
    }

    #[test]
    fn format_deserializes_from_lowercase() {
        let f: ExportFormat = serde_json::from_str("\"markdown\"").unwrap();
        assert_eq!(f, ExportFormat::Markdown);
        let f: ExportFormat = serde_json::from_str("\"txt\"").unwrap();
        assert_eq!(f, ExportFormat::Txt);
    }

    #[test]
    fn audio_forbidden_leaves_no_take_with_audio_to_save() {
        let takes = vec![TakeAudio { index: 1, duration_ms: Some(1000) }, TakeAudio { index: 2, duration_ms: None }];
        let kept = without_audio_if(false, takes.clone());
        assert_eq!(kept.iter().map(|t| t.duration_ms).collect::<Vec<_>>(), [Some(1000), None]);
        let forbidden = without_audio_if(true, takes);
        assert_eq!(forbidden.iter().map(|t| (t.index, t.duration_ms)).collect::<Vec<_>>(), [(1, None), (2, None)]);
    }
}
