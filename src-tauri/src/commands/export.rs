//! Note export (issue #18). Assembles a note's selected content — summary,
//! transcript, typed notes — into a single Markdown or plain-text document
//! and writes it to a user-chosen path.
//!
//! The path is chosen on the frontend via the dialog plugin's native save
//! panel and passed in as `spec.path`; this command is a thin shell around
//! the pure [`build_export_document`] assembler (unit-tested below) plus a
//! filesystem write. Unlike `open_in_finder`, the write is intentionally NOT
//! constrained to the app data directory — the whole point is to let the user
//! save anywhere they picked in the panel.

use super::err;
use crate::db;
use crate::html_text::html_to_text;
use crate::AppState;
use serde::Deserialize;
use tauri::{AppHandle, Manager};

/// Output file format. Adding a new format is a matter of adding a variant
/// here plus a branch in [`heading`] / [`build_export_document`] — no change
/// to the command surface or the frontend invoke shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExportFormat {
    Markdown,
    Txt,
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

/// Render a section heading for the given format and level (1 = document
/// title, 2 = section). Markdown uses ATX `#`/`##`; plain text underlines the
/// text with `=` (title) or `-` (section) so the structure survives without
/// markup.
fn heading(format: ExportFormat, level: u8, text: &str) -> String {
    match format {
        ExportFormat::Markdown => format!("{} {}", "#".repeat(level as usize), text),
        ExportFormat::Txt => {
            let bar = if level <= 1 { '=' } else { '-' };
            let underline: String = std::iter::repeat(bar).take(text.chars().count().max(1)).collect();
            format!("{text}\n{underline}")
        }
    }
}

/// Strip a leading `Label: ` speaker prefix from a single transcript line.
///
/// Transcript turns are stored as `Label: text` (e.g. `Speaker 1: …`, `You: …`,
/// or a renamed name). We only remove a *short*, single-segment prefix anchored
/// at the line start so a colon inside spoken text (`So the plan is: ship it`)
/// is left alone unless it happens to sit within the first ~40 chars — an
/// acceptable v1 trade-off given the fixed `Label: ` storage shape.
fn strip_label(line: &str) -> &str {
    if let Some(idx) = line.find(": ") {
        // Prefix must be reasonably short and free of its own line breaks to
        // look like a speaker label rather than a mid-sentence colon.
        let prefix = &line[..idx];
        if !prefix.is_empty() && prefix.chars().count() <= 40 && !prefix.contains(['\n', '\r']) {
            return &line[idx + 2..];
        }
    }
    line
}

/// Assemble the export document from a note's fields and a spec. Pure — no I/O,
/// so the whole content contract is unit-testable without a database or a
/// filesystem. `body_html` is the raw Tiptap HTML; it's converted to plain
/// text via the shared [`html_to_text`] helper (richer HTML→Markdown is a
/// deliberate later refinement, per the issue).
pub(crate) fn build_export_document(
    title: &str,
    summary: &str,
    transcript: &str,
    body_html: &str,
    spec: &ExportSpec,
) -> String {
    let title = if title.trim().is_empty() { "Untitled note" } else { title.trim() };
    let mut sections: Vec<String> = Vec::new();

    if spec.include_summary {
        sections.push(section(spec.format, "Summary", summary.trim()));
    }
    if spec.include_transcript {
        let body = if spec.include_speaker_labels {
            transcript.trim().to_string()
        } else {
            transcript
                .lines()
                .map(strip_label)
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_string()
        };
        sections.push(section(spec.format, "Transcript", &body));
    }
    if spec.include_notes {
        let notes = html_to_text(body_html);
        sections.push(section(spec.format, "Notes", notes.trim()));
    }

    let mut doc = heading(spec.format, 1, title);
    if !sections.is_empty() {
        doc.push_str("\n\n");
        doc.push_str(&sections.join("\n\n"));
    }
    doc.push('\n');
    doc
}

/// Render one `## Heading` + body block. A checked-but-empty section still
/// emits its heading with an explicit empty marker so the user sees the
/// section was intentionally included but had no content.
fn section(format: ExportFormat, name: &str, body: &str) -> String {
    let head = heading(format, 2, name);
    if body.is_empty() {
        let placeholder = match format {
            ExportFormat::Markdown => "_(empty)_",
            ExportFormat::Txt => "(empty)",
        };
        format!("{head}\n\n{placeholder}")
    } else {
        format!("{head}\n\n{body}")
    }
}

/// Export a note to the path chosen in the native save panel. Fetches the
/// note, builds the document via [`build_export_document`], and writes it.
#[tauri::command]
pub fn export_note(app: AppHandle, note_id: String, spec: ExportSpec) -> Result<(), String> {
    let state: tauri::State<AppState> = app.state();
    let note = {
        let conn = state.db.lock();
        db::get_note(&conn, &note_id).map_err(err)?
    };
    let doc = build_export_document(
        &note.title,
        &note.summary,
        &note.transcript,
        &note.body,
        &spec,
    );
    std::fs::write(&spec.path, doc).map_err(|e| format!("write export file: {e}"))?;
    Ok(())
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
}
