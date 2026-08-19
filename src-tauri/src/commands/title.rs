//! Automatic Note titles (#90).
//!
//! A Note whose Title Humla owns — empty, a `Recording 19 Aug 14:32`
//! timestamp, or the `Imported audio` import fallback — gets a short phrase
//! derived from the head of its own content once that content settles. The
//! model call is deliberately small and cheap: the first paragraphs of the
//! notes and the transcript, four to six words back, no streaming.
//!
//! Three things this is *not*, all of them settled during the issue's
//! grilling:
//!
//! - **Not driven by the summary.** Summarising in Humla is a button the user
//!   presses, so a summary-completion trigger would leave the motivating case
//!   — a menu-bar recorder who never opens the note — on its timestamp
//!   forever.
//! - **Not live.** The input is the same opening paragraphs whether we ask
//!   during the recording or after it, so firing early buys nothing and
//!   spends GPU exactly when local Whisper needs it.
//! - **Not loud.** Failure is silent. A user who never asked for a Title must
//!   not be told one failed, and a local-only user with no LLM configured
//!   would otherwise be toasted after every recording.

use super::summary::{language_directive, resolve_auto, resolve_provider};
use super::{DEFAULT_LANGUAGE, TITLE_FALLBACK_MODEL};
use crate::db::{self, NotePatch};
use crate::menubar::is_replaceable_title;
use crate::openai;
use crate::AppState;
use tauri::{AppHandle, Emitter, Manager, State};

/// Hard cap on a generated Title. The prompt asks for four to six words; this
/// is the backstop for a model that answers with a sentence.
const TITLE_MAX_CHARS: usize = 60;

/// Past this, the model didn't answer the question — it wrote prose. Truncating
/// that to 60 characters would produce a mangled fragment, which is a worse
/// Title than the timestamp it would replace, so we reject instead.
const TITLE_JUNK_CHARS: usize = 2 * TITLE_MAX_CHARS;

/// How much of each content block the model sees. A title comes from the head
/// of a note — the agenda, the first exchange — and sending the whole
/// transcript would cost real tokens for no better answer.
const BLOCK_CHAR_BUDGET: usize = 750;

/// Turn raw model output into a Title, or `None` when there is nothing usable
/// in it.
///
/// `None` is the "leave the existing Title alone" signal, and it is the type's
/// default rather than a convention the caller has to remember: there is no
/// worse-Title fallback anywhere in this path.
pub(crate) fn clean_title(raw: &str) -> Option<String> {
    // Single line: newlines collapse to spaces, then whitespace runs collapse
    // to one. A small model asked for six words will still sometimes answer
    // with a heading and a blank line.
    let mut t: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    // Peel until nothing more comes off. The three artifacts nest in either
    // order — `"Title: Kickoff"` and `"Kickoff".` both occur — so a fixed
    // sequence would leave whichever one it happened to check first.
    loop {
        let before = t.clone();
        t = strip_label_prefix(&t).to_string();
        t = strip_wrapping_quotes(&t).to_string();
        // Asked for no final period, and told anyway. Only the trailing full
        // stop — a title genuinely ending in `?` or `!` keeps it.
        t = t.trim_end_matches(['.', '\u{2026}']).trim().to_string();
        if t == before {
            break;
        }
    }
    let t = t.as_str();
    if t.is_empty() || !t.chars().any(char::is_alphanumeric) {
        return None;
    }
    if t.chars().count() > TITLE_JUNK_CHARS {
        return None;
    }
    Some(truncate_chars(t, TITLE_MAX_CHARS))
}

/// Peel a `Title:` / `Tittel:` label a model prepended despite being asked not
/// to. Only when something follows it — a title that IS the bare word is junk,
/// and `clean_title`'s emptiness check should be the one to say so.
fn strip_label_prefix(t: &str) -> &str {
    for label in ["title:", "tittel:"] {
        // Compare by chars, not by byte prefix — `t` may well start with a
        // multibyte character, and `&t[..label.len()]` would panic on it.
        if t.chars().count() > label.len()
            && t.chars()
                .zip(label.chars())
                .all(|(a, b)| a.to_ascii_lowercase() == b)
        {
            return t[label.len()..].trim_start();
        }
    }
    t
}

/// Peel one matching pair of wrapping quotes, straight or typographic.
fn strip_wrapping_quotes(t: &str) -> &str {
    const PAIRS: [(char, char); 5] =
        [('"', '"'), ('\'', '\''), ('\u{201c}', '\u{201d}'), ('\u{00ab}', '\u{00bb}'), ('\u{2018}', '\u{2019}')];
    let mut chars = t.chars();
    let (Some(first), Some(last)) = (chars.next(), chars.next_back()) else { return t };
    for (open, close) in PAIRS {
        if first == open && last == close {
            return t[open.len_utf8()..t.len() - close.len_utf8()].trim();
        }
    }
    t
}

/// Cut to at most `max` chars, on a `char` boundary and, where one is close
/// enough, on a word boundary — so a cap that lands mid-word doesn't leave a
/// half word behind.
fn truncate_chars(t: &str, max: usize) -> String {
    if t.chars().count() <= max {
        return t.to_string();
    }
    let cut = t.char_indices().nth(max).map(|(i, _)| i).unwrap_or(t.len());
    let head = &t[..cut];
    match head.rfind(' ') {
        // Only when the word boundary keeps most of the budget — otherwise a
        // long final word would throw away half the title.
        Some(i) if i >= max / 2 => head[..i].trim_end().to_string(),
        _ => head.trim_end().to_string(),
    }
}

/// The two labelled blocks the model reads, capped at the head of each.
///
/// Reuses the summary path's `[Notater]` / `[Transkripsjon]` convention
/// including its `(ingen)` placeholder: both blocks are always sent, so a
/// typed-only, a recorded-only and a mixed Note all take one path with no
/// precedence rule — and a thinking model isn't left wondering whether a
/// missing block is absent or hidden.
pub(crate) fn title_input(body_text: &str, transcript: &str) -> String {
    let notes = head_chars(body_text, BLOCK_CHAR_BUDGET);
    let spoken = head_chars(transcript, BLOCK_CHAR_BUDGET);
    let notes_block = if notes.is_empty() { "(ingen)".to_string() } else { notes };
    let transcript_block = if spoken.is_empty() { "(ingen)".to_string() } else { spoken };
    format!("[Notater]\n{notes_block}\n\n[Transkripsjon]\n{transcript_block}")
}

/// First `budget` chars of `text`, trimmed, cut on a `char` boundary.
fn head_chars(text: &str, budget: usize) -> String {
    let t = text.trim();
    let cut = t.char_indices().nth(budget).map(|(i, _)| i).unwrap_or(t.len());
    t[..cut].trim_end().to_string()
}

/// The system prompt. Minimal on purpose — a constraint-heavy prompt becomes a
/// checklist a small thinking model re-litigates instead of following.
fn title_prompt(language: &str) -> String {
    format!(
        "Give the note below a short title naming what it is about. \
         Four to six words. Answer with the title alone — no quotes, no label, \
         no final period, no explanation.\n\n{}",
        language_directive(language)
    )
}

/// Title `note_id` if its Title is still one of Humla's — the post-stop
/// recording chain's entry point, spawned rather than awaited.
///
/// Swallows every failure, which is the whole point of it being separate from
/// [`note_generate_title`]: nobody asked for this Title, so nobody is told when
/// one doesn't arrive. See the module docs.
pub(crate) async fn generate_note_title(app: AppHandle, note_id: String) {
    if let Err(e) = try_generate_note_title(&app, &note_id, false).await {
        eprintln!("[title] note {note_id}: {e:#}");
    }
}

async fn try_generate_note_title(
    app: &AppHandle,
    note_id: &str,
    force: bool,
) -> anyhow::Result<Option<String>> {
    let state: State<AppState> = app.state();
    // Keychain lookup out of band — it must not sit inside the DB lock.
    let openai_api_key = super::read_provider_api_key(&state, "openai")
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let (mut provider, language, note) = {
        let conn = state.db.lock();
        let n = db::get_note(&conn, note_id)?;
        if !force && !is_replaceable_title(&n.title) {
            return Ok(None);
        }
        let p = resolve_provider(&conn, &n, openai_api_key)?;
        let global = db::get_setting(&conn, "language")?
            .unwrap_or_else(|| DEFAULT_LANGUAGE.to_string());
        // Same fallback rule as the summary: the note's language wins, empty
        // means "follow the global default".
        let lang = if n.language.trim().is_empty() { global } else { n.language.clone() };
        (p, resolve_auto(&lang, &n), n)
    };
    let body_text = crate::html_text::html_to_text(&note.body);
    if body_text.trim().is_empty() && note.transcript.trim().is_empty() {
        return Ok(None);
    }
    // Thinking on a five-word title is waste, whatever the user set for
    // summaries.
    provider.think = false;
    if provider.base_url == openai::BASE && openai::is_reasoning_model(&provider.model) {
        provider.model = TITLE_FALLBACK_MODEL.to_string();
    }
    let raw = openai::summarize_with_base(
        &provider.base_url,
        &provider.api_key,
        &provider.model,
        provider.think,
        &title_prompt(&language),
        &title_input(&body_text, &note.transcript),
        // No streaming into the summary panel: this call is not a summary and
        // must not touch its UI.
        |_| {},
    )
    .await?;
    let Some(title) = clean_title(&raw) else {
        eprintln!("[title] note {note_id}: model returned nothing usable, keeping the title");
        return Ok(None);
    };
    {
        let conn = state.db.lock();
        // Re-check under the lock: the user may have typed a title of their own
        // while the model was working, and that one wins.
        let current = db::get_note(&conn, note_id)?;
        if !force && !is_replaceable_title(&current.title) {
            return Ok(None);
        }
        db::update_note(&conn, note_id, &NotePatch {
            title: Some(title.clone()),
            ..Default::default()
        })?;
    } // drop the db guard before pinging sync (see the SyncObserver contract)
    // A Title is real content a teammate must see, so unlike the derived
    // local-only writes (`set_detected_language`, `speakers`) this one bumps
    // `updated_at` and goes to the workspace.
    state.sync.note_upserted(note_id);
    // Backend write — the notes store never asked for it, so tell it. This is
    // what puts the new Title in the sidebar, and (via the Note view's guarded
    // adopt) in an open note's title box.
    let _ = app.emit("notes_changed", ());
    eprintln!("[title] note {note_id}: titled {title:?}");
    Ok(Some(title))
}

/// Title a Note from the frontend. Two callers, and `force` is the whole of the
/// difference between them:
///
/// - `force: true` — the ⋯ menu's **Regenerate title**. The escape hatch for a
///   bad automatic Title, and the only way to re-title a Note recorded in
///   several takes (the automatic path is one-shot by construction: once it
///   writes a Title, that Title is no longer replaceable). Overwrites whatever
///   is there, including a Title the user typed, because the user asked.
/// - `force: false` — the Note view's body-settled checkpoint, which is what
///   titles a Note that was typed and never recorded. Eligibility decides, and
///   an ineligible Note costs nothing: this returns before any model call.
///
/// Returns the new Title so the open Note view can put it in its draft directly
/// — the guarded adopt that carries the automatic path deliberately refuses to
/// overwrite a user-owned title, and regenerate asks for exactly that.
///
/// Reports failure, unlike the post-stop path, because a `force` call is a
/// button press and is owed an answer.
#[tauri::command]
pub async fn note_generate_title(
    app: AppHandle,
    note_id: String,
    force: bool,
) -> Result<Option<String>, String> {
    try_generate_note_title(&app, &note_id, force)
        .await
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_a_well_formed_title_verbatim() {
        assert_eq!(clean_title("Kickoff with Hege").as_deref(), Some("Kickoff with Hege"));
    }

    #[test]
    fn collapses_a_multi_line_answer_onto_one_line() {
        assert_eq!(
            clean_title("\n  Kickoff\n  with   Hege \n").as_deref(),
            Some("Kickoff with Hege")
        );
    }

    #[test]
    fn strips_wrapping_quotes_and_a_trailing_period() {
        assert_eq!(clean_title("\"Kickoff with Hege\".").as_deref(), Some("Kickoff with Hege"));
        assert_eq!(clean_title("“Kickoff with Hege”").as_deref(), Some("Kickoff with Hege"));
        assert_eq!(clean_title("«Oppstart med Hege»").as_deref(), Some("Oppstart med Hege"));
        assert_eq!(clean_title("'Kickoff with Hege'").as_deref(), Some("Kickoff with Hege"));
    }

    #[test]
    fn keeps_question_and_exclamation_marks() {
        // Only the full stop is an artifact of the model ignoring the prompt.
        assert_eq!(clean_title("Should we ship on Friday?").as_deref(), Some("Should we ship on Friday?"));
    }

    #[test]
    fn strips_a_label_the_model_added() {
        assert_eq!(clean_title("Title: Kickoff with Hege").as_deref(), Some("Kickoff with Hege"));
        assert_eq!(clean_title("\"Tittel: Oppstart med Hege\"").as_deref(), Some("Oppstart med Hege"));
    }

    #[test]
    fn junk_leaves_the_title_alone() {
        // Every one of these must reach the caller's "change nothing" path.
        assert_eq!(clean_title(""), None);
        assert_eq!(clean_title("   \n\t "), None);
        assert_eq!(clean_title("\"\""), None);
        assert_eq!(clean_title("..."), None);
        assert_eq!(clean_title("---"), None);
    }

    #[test]
    fn a_paragraph_leaves_the_title_alone() {
        // A model that answered with prose did not answer the question, and a
        // 60-char slice of prose is a worse title than the timestamp it would
        // replace.
        let paragraph = "This note covers the kickoff meeting with Hege, where the team \
                         discussed the launch timeline, the remaining blockers, and who \
                         would own each of them.";
        assert_eq!(clean_title(paragraph), None);
    }

    #[test]
    fn a_slightly_long_title_is_capped_not_rejected() {
        // Between "a title" and "a paragraph": trim it rather than lose it.
        let long = "Kickoff meeting with Hege about the launch timeline and blockers";
        let t = clean_title(long).unwrap();
        assert!(t.chars().count() <= TITLE_MAX_CHARS, "{t:?}");
        assert!(long.starts_with(&t), "{t:?}");
    }

    #[test]
    fn a_cap_never_splits_a_multibyte_character() {
        // 61 two-byte chars: every cut inside this string is mid-character
        // unless the cut is made on char indices.
        let wide = "æ".repeat(61);
        let t = clean_title(&wide).unwrap();
        assert_eq!(t.chars().count(), TITLE_MAX_CHARS);
        // Reaching this line at all means the slice was valid UTF-8.
        assert!(t.chars().all(|c| c == 'æ'));
    }

    #[test]
    fn a_multibyte_title_is_capped_on_a_word_boundary() {
        let long = "Oppstartsmøte med Hege om lanseringsplanen og gjenstående blokkere";
        let t = clean_title(long).unwrap();
        assert!(t.chars().count() <= TITLE_MAX_CHARS, "{t:?}");
        assert!(!t.ends_with(' '), "{t:?}");
        assert!(long.starts_with(&t), "{t:?}");
    }

    #[test]
    fn both_blocks_are_always_sent() {
        // A typed-only note and a recorded-only note take the same path — the
        // empty side says "(ingen)" rather than going missing.
        let typed = title_input("agenda: launch", "");
        assert!(typed.contains("[Notater]\nagenda: launch"), "{typed}");
        assert!(typed.contains("[Transkripsjon]\n(ingen)"), "{typed}");

        let recorded = title_input("", "Hege: hei");
        assert!(recorded.contains("[Notater]\n(ingen)"), "{recorded}");
        assert!(recorded.contains("[Transkripsjon]\nHege: hei"), "{recorded}");
    }

    #[test]
    fn each_block_is_capped_at_its_own_budget() {
        let long = "a ".repeat(2000);
        let input = title_input(&long, &long);
        // Two capped blocks plus the labels — nowhere near the raw input.
        assert!(input.len() < 4 * BLOCK_CHAR_BUDGET, "{}", input.len());
        assert!(input.contains("[Notater]"));
        assert!(input.contains("[Transkripsjon]"));
    }

    #[test]
    fn a_cap_on_a_block_never_splits_a_multibyte_character() {
        let wide = "æ".repeat(BLOCK_CHAR_BUDGET + 40);
        let input = title_input(&wide, "");
        // Would have panicked on a byte-index slice.
        assert!(input.contains(&"æ".repeat(BLOCK_CHAR_BUDGET)));
    }

    #[test]
    fn the_prompt_carries_the_notes_language() {
        assert!(title_prompt("no").contains("norsk"));
        assert!(title_prompt("en").contains("English"));
        // On `auto` the only cue is the transcript block — same rule as #167.
        assert!(title_prompt("auto").contains("transcript"));
    }
}
