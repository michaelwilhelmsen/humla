//! Automatic Note titles (#90).
//!
//! A Note whose Title Humla owns — empty, a `Recording 19 Aug 14:32`
//! timestamp, or the `Imported audio` import fallback — gets a short phrase
//! derived from its own content once that content settles. The model call is
//! deliberately small and cheap: the head of the typed notes, excerpts from
//! across the transcript, and the summary when the Note has one — four to six
//! words back, no streaming. Every trigger (the post-stop chain, a deferred
//! transcription finishing, the body-settled checkpoint and ⋯ → Regenerate
//! title) reads the Note through the same [`title_input`].
//!
//! Three things this is *not*, all of them settled during the issue's
//! grilling:
//!
//! - **Not driven by the summary.** Summarising in Humla is a button the user
//!   presses, so a summary-completion trigger would leave the motivating case
//!   — a menu-bar recorder who never opens the note — on its timestamp
//!   forever. A summary that already exists is read, though: it is the best
//!   distillation of the Note there is.
//! - **Not live.** The transcript is sampled across its whole length, so a
//!   title taken mid-recording would describe part of a meeting — and spend
//!   GPU exactly when local Whisper needs it. After stop is the cheap moment.
//! - **Not loud.** Failure is silent. A user who never asked for a Title must
//!   not be told one failed, and a local-only user with no LLM configured
//!   would otherwise be toasted after every recording.

use super::summary::{
    attach_copilot, language_directive, note_language, resolve_provider, summary_provider_id,
};
use super::TITLE_FALLBACK_MODEL;
use crate::db::{self, NotePatch};
use crate::menubar::is_replaceable_title;
use crate::openai;
use crate::providers::ProviderId;
use crate::recording::TitleStatusPayload;
use crate::AppState;
use tauri::{AppHandle, Emitter, Manager, State};

/// Hard cap on a generated Title. The prompt asks for four to six words; this
/// is the backstop for a model that answers with a sentence.
const TITLE_MAX_CHARS: usize = 60;

/// Past this, the model didn't answer the question — it wrote prose. Truncating
/// that to 60 characters would produce a mangled fragment, which is a worse
/// Title than the timestamp it would replace, so we reject instead.
const TITLE_JUNK_CHARS: usize = 2 * TITLE_MAX_CHARS;

/// How much of the typed notes the model sees: their head, which is where an
/// agenda sits.
const NOTES_CHAR_BUDGET: usize = 750;

/// How much of the transcript the model sees. The head alone is the wrong
/// part — a meeting opens on the weather — so a longer transcript is sampled
/// across its whole length instead (see [`sample_transcript`]). Around 800
/// tokens: noise beside a summary call, and room for every window to hold a
/// few whole exchanges.
const TRANSCRIPT_CHAR_BUDGET: usize = 3_000;

/// How many windows a transcript over budget is cut into. More windows reach
/// more of the meeting; fewer keep each one long enough to follow.
const TRANSCRIPT_WINDOWS: usize = 6;

/// What stands between two transcript windows, and what the prompt tells the
/// model it means: speech was skipped here.
const SKIP_MARK: &str = "\n…\n";

/// How much of an existing summary the model sees. Its opening is the
/// overview, which is all a title needs.
const SUMMARY_CHAR_BUDGET: usize = 1_500;

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
    match head.rfind(char::is_whitespace) {
        // Only when the word boundary keeps most of the budget — otherwise a
        // long final word would throw away half the title.
        Some(i) if i >= max / 2 => head[..i].trim_end().to_string(),
        _ => head.trim_end().to_string(),
    }
}

/// The labelled blocks the model reads: the head of the notes, a sample of the
/// transcript, and the head of the summary when there is one.
///
/// Reuses the summary path's `[Notater]` / `[Transkripsjon]` convention
/// including its `(ingen)` placeholder: both blocks are always sent, so a
/// typed-only, a recorded-only and a mixed Note all take one path with no
/// precedence rule — and a thinking model isn't left wondering whether a
/// missing block is absent or hidden. `[Sammendrag]` is the exception and is
/// sent only when it has content: most Notes are titled before anyone
/// summarises them, and an `(ingen)` in nearly every call would be noise.
pub(crate) fn title_input(body_text: &str, transcript: &str, summary: &str) -> String {
    let notes = head_chars(body_text, NOTES_CHAR_BUDGET);
    let spoken = sample_transcript(transcript);
    let notes_block = if notes.is_empty() { "(ingen)".to_string() } else { notes };
    let transcript_block = if spoken.is_empty() { "(ingen)".to_string() } else { spoken };
    let mut input = format!("[Notater]\n{notes_block}\n\n[Transkripsjon]\n{transcript_block}");
    let summary = truncate_chars(summary.trim(), SUMMARY_CHAR_BUDGET);
    if !summary.is_empty() {
        input.push_str(&format!("\n\n[Sammendrag]\n{summary}"));
    }
    input
}

/// The transcript as the model sees it: whole when it fits the budget,
/// otherwise [`TRANSCRIPT_WINDOWS`] excerpts, one from the middle of each equal
/// stretch of it, joined by [`SKIP_MARK`].
///
/// Even spacing is what makes the sample represent the meeting: whatever took
/// most of the time turns up in most of the windows, and the opening small
/// talk and the closing goodbyes get at most a window's share between them,
/// whatever the meeting's length. A window never reaches past its own stretch,
/// so two never overlap and every mark stands for speech that really was
/// skipped.
fn sample_transcript(text: &str) -> String {
    let t = text.trim();
    let total = t.chars().count();
    if total <= TRANSCRIPT_CHAR_BUDGET {
        return t.to_string();
    }
    let marks = (TRANSCRIPT_WINDOWS - 1) * SKIP_MARK.chars().count();
    let window = (TRANSCRIPT_CHAR_BUDGET - marks) / TRANSCRIPT_WINDOWS;
    let stretch = total / TRANSCRIPT_WINDOWS;
    (0..TRANSCRIPT_WINDOWS)
        .map(|i| excerpt(t, i * stretch + (stretch - window) / 2, window))
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(SKIP_MARK)
}

/// At most `len` chars of `t` from char `start`, nudged inward onto word
/// boundaries so an excerpt neither opens nor ends on half a word. Inward only:
/// the excerpt stays inside `[start, start + len)`.
fn excerpt(t: &str, start: usize, len: usize) -> String {
    let from = byte_at(t, start);
    let rest = &t[from..];
    let mid_word = t[..from].chars().next_back().is_some_and(|c| !c.is_whitespace())
        && rest.chars().next().is_some_and(|c| !c.is_whitespace());
    let (rest, skipped) = match rest.char_indices().take(len / 2).find(|(_, c)| c.is_whitespace()) {
        Some((i, _)) if mid_word => (&rest[i..], rest[..i].chars().count()),
        _ => (rest, 0),
    };
    truncate_chars(rest.trim_start(), len - skipped)
}

/// Byte offset of char `n` in `t`, or its length when `t` is shorter.
fn byte_at(t: &str, n: usize) -> usize {
    t.char_indices().nth(n).map_or(t.len(), |(i, _)| i)
}

/// First `budget` chars of `text`, trimmed, cut on a `char` boundary.
fn head_chars(text: &str, budget: usize) -> String {
    let t = text.trim();
    t[..byte_at(t, budget)].trim_end().to_string()
}

/// The system prompt. Minimal on purpose — a constraint-heavy prompt becomes a
/// checklist a small thinking model re-litigates instead of following. The one
/// sentence beyond the format is the one the transcript sample needs: what the
/// marks between excerpts are, and that the small talk is not the subject.
fn title_prompt(language: &str) -> String {
    format!(
        "Give the note below a short title naming what it is about. \
         Four to six words. Answer with the title alone — no quotes, no label, \
         no final period, no explanation. Name what most of it is about, not \
         the small talk around it; … marks skipped speech.\n\n{}",
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
    // Which provider first, then that provider's key — the summary path's rule,
    // so a title never opens a Keychain slot its provider doesn't use.
    let provider_id = {
        let conn = state.db.lock();
        let n = db::get_note(&conn, note_id)?;
        if !force && !is_replaceable_title(&n.title) {
            return Ok(None);
        }
        summary_provider_id(&conn, &n)?
    };
    let api_key = match provider_id.keychain_account() {
        Some(_) => super::read_provider_api_key(&state, provider_id.as_str())
            .map_err(|e| anyhow::anyhow!("{e}"))?,
        None => None,
    };
    let (mut provider, language, note) = {
        let conn = state.db.lock();
        let n = db::get_note(&conn, note_id)?;
        if !force && !is_replaceable_title(&n.title) {
            return Ok(None);
        }
        let p = resolve_provider(&conn, &n, api_key)?;
        (p, note_language(&conn, &n)?, n)
    };
    if crate::html_text::html_to_text(&note.body).trim().is_empty()
        && note.transcript.trim().is_empty()
        && note.summary.trim().is_empty()
    {
        // Regenerate is owed the real reason. `None` would reach it as "the
        // model gave back nothing usable" about a model that was never asked.
        if force {
            anyhow::bail!("This note has nothing to title yet. Type some notes or record first.");
        }
        return Ok(None);
    }
    // Thinking on a five-word title is waste, whatever the user set for
    // summaries.
    provider.think = false;
    if provider.provider == ProviderId::OpenAi && openai::is_reasoning_model(&provider.model) {
        provider.model = TITLE_FALLBACK_MODEL.to_string();
    }
    // Everything cheap and every refusal is behind us: from here a model call
    // is definitely happening, which is exactly what the Note view shimmers
    // over. Bracketing any earlier would flash a placeholder on notes we then
    // decline to title.
    attach_copilot(app, &mut provider).await?;
    if let Some(client) = provider.copilot.as_ref() {
        let models = crate::copilot::session::list_models(client).await.unwrap_or_default();
        if let Some(cheapest) = crate::copilot::session::cheapest(&models) {
            provider.model = cheapest.to_string();
        }
    }
    emit_title_status(app, note_id, true);
    let written = write_generated_title(app, note_id, &provider, &language, &note, force).await;
    emit_title_status(app, note_id, false);
    written
}

/// Ask the model, clean the answer, and write it. Split out so
/// [`try_generate_note_title`] can bracket exactly this with `title_status` —
/// the `?`s in here are why: an early return must still clear the shimmer.
async fn write_generated_title(
    app: &AppHandle,
    note_id: &str,
    provider: &super::summary::ResolvedProvider,
    language: &str,
    note: &db::Note,
    force: bool,
) -> anyhow::Result<Option<String>> {
    let state: State<AppState> = app.state();
    let body_text = crate::html_text::html_to_text(&note.body);
    // No streaming into the summary panel: this call is not a summary and must
    // not touch its UI.
    let raw = provider
        .complete(
            &title_prompt(language),
            &title_input(&body_text, &note.transcript, &note.summary),
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

fn emit_title_status(app: &AppHandle, note_id: &str, active: bool) {
    let _ = app.emit("title_status", TitleStatusPayload {
        note_id: note_id.to_string(),
        active,
    });
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
        let typed = title_input("agenda: launch", "", "");
        assert!(typed.contains("[Notater]\nagenda: launch"), "{typed}");
        assert!(typed.contains("[Transkripsjon]\n(ingen)"), "{typed}");

        let recorded = title_input("", "Hege: hei", "");
        assert!(recorded.contains("[Notater]\n(ingen)"), "{recorded}");
        assert!(recorded.contains("[Transkripsjon]\nHege: hei"), "{recorded}");
    }

    #[test]
    fn the_summary_is_sent_only_when_there_is_one() {
        assert!(!title_input("agenda", "Hege: hei", "").contains("[Sammendrag]"));
        assert!(!title_input("agenda", "Hege: hei", "  \n ").contains("[Sammendrag]"));
        let summarised = title_input("agenda", "Hege: hei", "Launch moved to Friday.");
        assert!(
            summarised.ends_with("[Sammendrag]\nLaunch moved to Friday."),
            "{summarised}"
        );
    }

    #[test]
    fn each_block_is_capped_at_its_own_budget() {
        let long = "a ".repeat(20_000);
        let input = title_input(&long, &long, &long);
        let budget = NOTES_CHAR_BUDGET + TRANSCRIPT_CHAR_BUDGET + SUMMARY_CHAR_BUDGET;
        // Three capped blocks plus their labels — nowhere near the raw input.
        assert!(input.chars().count() < budget + 100, "{}", input.chars().count());
        assert!(input.contains("[Notater]"));
        assert!(input.contains("[Transkripsjon]"));
        assert!(input.contains("[Sammendrag]"));
    }

    #[test]
    fn a_cap_on_a_block_never_splits_a_multibyte_character() {
        let wide = "æ".repeat(NOTES_CHAR_BUDGET + 40);
        let input = title_input(&wide, "", "");
        // Would have panicked on a byte-index slice.
        assert!(input.contains(&"æ".repeat(NOTES_CHAR_BUDGET)));
    }

    #[test]
    fn the_prompt_carries_the_notes_language() {
        assert!(title_prompt("no").contains("norsk"));
        assert!(title_prompt("en").contains("English"));
        // On `auto` the only cue is the transcript block — same rule as #167.
        assert!(title_prompt("auto").contains("transcript"));
    }

    #[test]
    fn the_prompt_explains_the_mark_it_samples_with() {
        let mark = SKIP_MARK.trim();
        assert!(title_prompt("en").contains(&format!("{mark} marks skipped speech")));
    }

    /// A meeting of `minutes` at roughly speaking pace: small talk, then one
    /// topic, then goodbyes, each in its own words so a test can see which
    /// parts a sample reached.
    fn meeting(minutes: usize) -> String {
        let chars = minutes * 800;
        let fill = |line: &str, n: usize| line.repeat(n / line.len() + 1);
        format!(
            "{}{}{}",
            fill("You: fint vær i dag, hvordan var helgen? ", 1_200),
            fill("Hege: ORION-lanseringen utsettes til fredag, jeg eier blokkerne. ", chars),
            fill("You: takk for i dag, ha det bra! ", 1_200),
        )
    }

    #[test]
    fn a_transcript_within_budget_is_sent_whole() {
        let short = "You: hei Hege.\nHege: hei, hvordan går det?";
        assert_eq!(sample_transcript(short), short);
        assert_eq!(sample_transcript(""), "");
        let exact = "a".repeat(TRANSCRIPT_CHAR_BUDGET);
        assert_eq!(sample_transcript(&exact), exact);
    }

    #[test]
    fn a_long_meeting_is_sampled_mostly_from_its_substance() {
        // The failure #90 kept producing: a head-only input titled the weather.
        for minutes in [10, 60, 180] {
            let sampled = sample_transcript(&meeting(minutes));
            let windows: Vec<&str> = sampled.split(SKIP_MARK).collect();
            assert_eq!(windows.len(), TRANSCRIPT_WINDOWS, "{minutes} min: {sampled}");
            let on_topic = windows.iter().filter(|w| w.contains("ORION")).count();
            assert!(on_topic >= TRANSCRIPT_WINDOWS - 2, "{minutes} min: {sampled}");
        }
    }

    #[test]
    fn a_sample_never_exceeds_the_budget() {
        for len in [TRANSCRIPT_CHAR_BUDGET + 1, TRANSCRIPT_CHAR_BUDGET + 7, 9_999, 250_000] {
            let text = "ord og ".repeat(len / 7 + 1);
            let sampled = sample_transcript(&text[..len]);
            assert!(sampled.chars().count() <= TRANSCRIPT_CHAR_BUDGET, "{len}");
        }
    }

    #[test]
    fn windows_just_over_the_budget_do_not_overlap() {
        // Numbered words make any duplicate visible: a word that turns up in two
        // windows means a mark claimed a skip that never happened.
        let words: Vec<String> = (0..700).map(|i| format!("w{i}")).collect();
        let text = words.join(" ");
        assert!(text.chars().count() > TRANSCRIPT_CHAR_BUDGET);
        let sampled = sample_transcript(&text);
        let mut seen = std::collections::HashSet::new();
        for word in sampled.split_whitespace().filter(|w| *w != "…") {
            assert!(seen.insert(word), "{word} sampled twice: {sampled}");
        }
    }

    #[test]
    fn an_excerpt_never_opens_or_ends_on_half_a_word() {
        let words: Vec<String> = (0..5_000).map(|i| format!("ord{i}")).collect();
        let text = words.join(" ");
        let whole: std::collections::HashSet<&str> = text.split(' ').collect();
        let sampled = sample_transcript(&text);
        for word in sampled.split_whitespace().filter(|w| *w != "…") {
            assert!(whole.contains(word), "{word} is half a word: {sampled}");
        }
    }

    #[test]
    fn an_excerpt_keeps_a_word_it_starts_on() {
        // Already on a boundary — after a space or a newline — is not mid-word,
        // so the first word must survive.
        assert_eq!(excerpt("aaa ORION bbb", 4, 9), "ORION bbb");
        assert_eq!(excerpt("aaa\nORION bbb", 4, 9), "ORION bbb");
        assert_eq!(excerpt("aaa ORION bbb", 5, 10), "bbb");
    }

    #[test]
    fn a_sample_never_splits_a_multibyte_character() {
        let wide = "æøå ".repeat(5_000);
        let sampled = sample_transcript(&wide);
        // Would have panicked on a byte-index slice.
        assert!(sampled.contains("æøå"));
        assert!(sampled.chars().count() <= TRANSCRIPT_CHAR_BUDGET);
    }
}
