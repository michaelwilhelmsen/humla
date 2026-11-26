//! Clean transcripts (#217): a model rewrites each segment (one speaker's
//! consecutive entries) as a paragraph of clean text, which the user may then
//! edit. Stored in `notes.clean_transcript` and synced with the note, while
//! `timeline.jsonl` and `notes.transcript` stay verbatim (ADR-0004). Each
//! paragraph remembers the fingerprints of the entries it was written from, so
//! a later pass cleans only what changed. A rewrite that fails a guard leaves
//! its paragraph verbatim.

use super::transcript::{fold_word, word_ends_sentence};
use super::summary::{resolve_provider, summary_provider_id, use_light_model, LightJob, ResolvedProvider};
use crate::providers::ProviderId;
use crate::{db, sessions, AppState};
use futures_util::{stream, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use tauri::{AppHandle, Emitter, Manager, State};

/// Bumped when the stored document's meaning changes. A document of a version
/// this build doesn't know is shown and written by none of it.
const CLEAN_DOC_VERSION: u32 = 1;

/// Most verbatim words in one segment: about a paragraph of one speaker, so a
/// false start and its restart land in the same segment.
const SEGMENT_WORDS: usize = 110;

/// Most verbatim words in one model call. Several segments, often several
/// speakers, so each reads in its conversation. A small local model (and a
/// custom endpoint may run one) rewrites every segment of a short window
/// rather than summarising the lot; a cloud model holds a long one, and every
/// call saved is a system prompt not sent again.
const LOCAL_WINDOW_WORDS: usize = 240;
const CLOUD_WINDOW_WORDS: usize = 1000;

/// Model calls a pass keeps in flight at once.
const CONCURRENT_CALLS: usize = 3;

fn window_words(provider: ProviderId) -> usize {
    match provider {
        ProviderId::Local | ProviderId::Custom => LOCAL_WINDOW_WORDS,
        _ => CLOUD_WINDOW_WORDS,
    }
}

/// Why an automatic pass leaves a Copilot note alone: Copilot counts requests,
/// so cleaning waits until the user asks.
const COPILOT_ON_REQUEST: &str = "with Copilot, Clean runs only when you ask.";

/// What a clean pass is asked to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CleanRequest {
    /// Nobody asked: the post-stop chain, or the reader opening Clean on
    /// words no paragraph covers. Cleans those words.
    Auto,
    /// "Clean now": the words no paragraph covers.
    Missing,
    /// "Clean again": every paragraph anew, edited ones included, after
    /// checking every speaker change anew.
    All,
}

/// A segment this short may be cleaned away entirely ("Mm.", "Eh, ja").
const DROP_MAX_WORDS: usize = 4;

/// A rewrite of a segment this long must keep at least `MIN_KEPT_SHARE` of its
/// word count: below that the model summarised.
const LENGTH_GUARD_MIN_WORDS: usize = 8;
const MIN_KEPT_SHARE: f32 = 0.35;

/// At least this share of a rewrite's words must be words the speaker said
/// (aligned to the verbatim text), once it has `ALIGN_GUARD_MIN_WORDS` words:
/// below that the model paraphrased, translated or invented.
const ALIGN_GUARD_MIN_WORDS: usize = 4;
const MIN_ALIGNED_SHARE: f32 = 0.6;

/// Negations a rewrite may never lose, in the languages most Humla notes are
/// in. Dropping "ikke" inverts a sentence; that is the one failure a clean transcript
/// must not have. Answers ("no", "nei") are left out: "Nei, nei, nei" → "Nei."
/// is a fair clean-up. The prompt carries the rule for every other language.
const NEGATIONS: &[&str] = &[
    "not", "never", "nothing", "nobody", "none", "dont", "doesnt", "didnt", "cant", "wont", "isnt",
    "arent", "wasnt", "werent", "shouldnt", "wouldnt", "couldnt", "ikke", "ikkje", "inte", "ingen",
    "inga", "intet", "aldri", "aldrig", "nicht", "kein", "keine", "keinen", "nie", "niemals", "pas",
    "jamais", "rien", "nunca", "nada", "nadie", "niet", "geen", "nooit",
];

/// How often, and for how long, the automatic pass waits for the chain that
/// spawned it to release the note.
const BUSY_POLL: std::time::Duration = std::time::Duration::from_secs(2);
const BUSY_WAIT_LIMIT: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// Notes with a clean pass running, so the reader asking twice (or the
/// post-stop pass and the reader at once) costs one set of model calls.
fn running() -> &'static parking_lot::Mutex<HashSet<String>> {
    static RUNNING: std::sync::OnceLock<parking_lot::Mutex<HashSet<String>>> =
        std::sync::OnceLock::new();
    RUNNING.get_or_init(Default::default)
}

/// Whether transcripts are cleaned at all (Settings → Transcription → Clean
/// transcripts). On unless set to the literal `"false"`.
pub(crate) fn clean_enabled(setting: Option<&str>) -> bool {
    setting != Some("false")
}

/// Whether a model checks the speaker changes after a recording (Settings →
/// Transcription → Fix speaker changes). On unless set to `"false"`.
pub(crate) fn speaker_check_enabled(setting: Option<&str>) -> bool {
    setting != Some("false")
}

/// The model the `clean_model` setting names for `provider`, if it names one
/// for it: the setting is `<provider>:<model>`, or empty for the default.
/// Mirrors `cleanModelFor` (`src/lib/cleanModel.ts`).
pub(crate) fn clean_model_for(setting: Option<&str>, provider: ProviderId) -> Option<String> {
    let (named, model) = setting?.split_once(':')?;
    let model = model.trim();
    (ProviderId::parse(named.trim())? == provider && !model.is_empty()).then(|| model.to_string())
}

pub(super) fn clean_setting_on(app: &AppHandle) -> bool {
    clean_enabled(super::raw_setting(app, "clean_transcripts").as_deref())
}

/// A note's clean transcript: `notes.clean_transcript` as JSON.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub(crate) struct CleanDoc {
    pub(crate) v: u32,
    #[serde(default)]
    pub(crate) takes: Vec<CleanTake>,
}

impl Default for CleanDoc {
    fn default() -> Self {
        Self { v: CLEAN_DOC_VERSION, takes: Vec::new() }
    }
}

impl CleanDoc {
    fn take(&self, session: &str) -> Option<&CleanTake> {
        self.takes.iter().find(|t| t.session == session)
    }

    fn take_mut(&mut self, session: &str) -> &mut CleanTake {
        match self.takes.iter().position(|t| t.session == session) {
            Some(i) => &mut self.takes[i],
            None => {
                self.takes.push(CleanTake { session: session.to_string(), ..Default::default() });
                self.takes.last_mut().expect("just pushed")
            }
        }
    }
}

/// One take's paragraphs, in the order they were said.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub(crate) struct CleanTake {
    pub(crate) session: String,
    /// What cleaned it, for diagnostics: `<provider>:<model>`.
    #[serde(default)]
    pub(crate) by: String,
    #[serde(default)]
    pub(crate) turns: Vec<CleanTurn>,
}

/// A paragraph: one segment's clean text.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub(crate) struct CleanTurn {
    /// Stays the same across passes, so an edit finds its paragraph.
    pub(crate) id: String,
    /// Fingerprints of the timeline entries it was written from, in order.
    pub(crate) members: Vec<String>,
    /// Where its first entry starts in the take, for "play from here".
    pub(crate) start_ms: u64,
    /// The entries' speaker when it was written, or the user's choice.
    pub(crate) speaker: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) speaker_set: bool,
    pub(crate) text: String,
    /// The user rewrote the text.
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) edited: bool,
}

impl CleanTurn {
    fn touched(&self) -> bool {
        self.edited || self.speaker_set
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// The stored document, an empty one when there is none, or `None` when it
/// can't be read or is of another version.
pub(crate) fn parse_doc(json: &str) -> Option<CleanDoc> {
    if json.trim().is_empty() {
        return Some(CleanDoc::default());
    }
    serde_json::from_str::<CleanDoc>(json).ok().filter(|d| d.v == CLEAN_DOC_VERSION)
}

const UNREADABLE_DOC: &str = "this note's clean transcript was written by another version of Humla.";

/// The words of one timeline entry as the reader shows them: its timed words,
/// or its text's whitespace tokens when the provider gave no timings.
pub(crate) fn entry_tokens(value: &serde_json::Value) -> Vec<String> {
    let words = value.get("words").and_then(|w| w.as_array()).into_iter().flatten();
    tokens_of(
        words.filter_map(|w| w.get("text").and_then(|t| t.as_str())),
        value.get("text").and_then(|t| t.as_str()).unwrap_or(""),
    )
}

/// An entry's words for its fingerprint: its timed words, or its text when it
/// has none. The one rule for files and for the reader, so the two agree.
pub(crate) fn tokens_of<'a>(words: impl Iterator<Item = &'a str>, text: &str) -> Vec<String> {
    let words: Vec<String> = words.map(str::to_string).collect();
    if !words.is_empty() {
        return words;
    }
    text.split_whitespace().map(str::to_string).collect()
}

/// A stable fingerprint of an entry's words (64-bit FNV-1a, hex). Stable
/// across builds and platforms, unlike std's hasher, because it is stored.
pub(crate) fn fingerprint(tokens: &[String]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for (i, t) in tokens.iter().enumerate() {
        if i > 0 {
            h ^= u64::from(b' ');
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        for b in t.as_bytes() {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    format!("{h:016x}")
}

/// A JSON file of a take's directory, or `None` when it's missing or unreadable.
pub(super) fn read_take_file<T: serde::de::DeserializeOwned>(take_dir: &Path, name: &str) -> Option<T> {
    std::fs::read_to_string(take_dir.join(name)).ok().and_then(|s| serde_json::from_str(&s).ok())
}

/// Replace a JSON file of a take's directory whole.
pub(super) fn write_take_file<T: Serialize>(take_dir: &Path, name: &str, value: &T) -> std::io::Result<()> {
    let json = serde_json::to_string(value).map_err(std::io::Error::other)?;
    let tmp = take_dir.join(format!("{name}.tmp"));
    std::fs::write(&tmp, json)?;
    std::fs::rename(tmp, take_dir.join(name))
}

/// Whether paragraph `t` can stand at entry `i`: it started there when it was
/// written, and the entries from there on are still the words it was written
/// from.
fn fits(entries: &[Entry], i: usize, t: &CleanTurn) -> bool {
    let m = &t.members;
    !m.is_empty()
        && i + m.len() <= entries.len()
        && entries[i].start_ms == t.start_ms
        && entries[i..i + m.len()].iter().zip(m).all(|(e, fp)| e.fp == *fp)
}

/// Which stored paragraph, if any, stands at each entry (`Some(index)` on its
/// first entry). A paragraph lands only where it started, and once. Where
/// paragraphs overlap, the ones that together cover the most entries win.
fn match_turns(entries: &[Entry], turns: &[CleanTurn]) -> Vec<Option<usize>> {
    let mut by_start: HashMap<(u64, &str), Vec<usize>> = HashMap::new();
    for (ti, t) in turns.iter().enumerate() {
        if let Some(first) = t.members.first() {
            by_start.entry((t.start_ms, first.as_str())).or_default().push(ti);
        }
    }
    let candidates = |i: usize| -> Vec<usize> {
        let e = &entries[i];
        by_start
            .get(&(e.start_ms, e.fp.as_str()))
            .into_iter()
            .flatten()
            .copied()
            .filter(|&ti| fits(entries, i, &turns[ti]))
            .collect()
    };
    let n = entries.len();
    // best[i]: the most entries from i on that paragraphs can cover; span[i]:
    // the length of the paragraph chosen at i, 0 for none.
    let mut best = vec![0usize; n + 1];
    let mut span = vec![0usize; n];
    for i in (0..n).rev() {
        best[i] = best[i + 1];
        for ti in candidates(i) {
            let m = turns[ti].members.len();
            if m + best[i + m] > best[i] {
                best[i] = m + best[i + m];
                span[i] = m;
            }
        }
    }
    let mut out = vec![None; n];
    let mut used = vec![false; turns.len()];
    let mut i = 0;
    while i < n {
        let pick = if span[i] > 0 {
            candidates(i).into_iter().find(|&ti| !used[ti] && turns[ti].members.len() == span[i])
        } else {
            None
        };
        match pick {
            Some(ti) => {
                used[ti] = true;
                out[i] = Some(ti);
                i += span[i];
            }
            None => i += 1,
        }
    }
    out
}

/// One timeline entry as the clean pass sees it.
#[derive(Debug, Clone)]
struct Entry {
    label: String,
    tokens: Vec<String>,
    start_ms: u64,
    fp: String,
}

/// A segment to clean: consecutive entry indices of one speaker.
#[derive(Debug, Clone, PartialEq)]
struct Plan {
    entries: Vec<usize>,
}


/// Cut the entries that need cleaning into segments: runs of one speaker's
/// consecutive entries, at most `SEGMENT_WORDS` words, cut after an entry that
/// ends a sentence where one is near the limit. An entry that doesn't need
/// cleaning ends the run, so a take that lost a few segments to an edit costs
/// one small call, not a whole pass.
fn plan_segments(entries: &[Entry], needs: &dyn Fn(usize) -> bool) -> Vec<Plan> {
    let mut out: Vec<Plan> = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    let mut cur_words = 0usize;
    let flush = |cur: &mut Vec<usize>, cur_words: &mut usize, out: &mut Vec<Plan>| {
        if !cur.is_empty() {
            out.push(Plan { entries: std::mem::take(cur) });
        }
        *cur_words = 0;
    };
    for (i, e) in entries.iter().enumerate() {
        if !needs(i) {
            flush(&mut cur, &mut cur_words, &mut out);
            continue;
        }
        let same_speaker = cur.last().is_some_and(|&j| entries[j].label == e.label);
        if !same_speaker {
            flush(&mut cur, &mut cur_words, &mut out);
        } else if cur_words + e.tokens.len() > SEGMENT_WORDS {
            // Over the limit: cut after the latest entry in this segment that
            // ends a sentence, so a restart isn't split from its false start.
            let cut = cur
                .iter()
                .rposition(|&j| entries[j].tokens.last().is_some_and(|t| word_ends_sentence(t)))
                .filter(|&p| p + 1 < cur.len());
            match cut {
                Some(p) => {
                    let tail = cur.split_off(p + 1);
                    out.push(Plan { entries: std::mem::take(&mut cur) });
                    cur_words = tail.iter().map(|&j| entries[j].tokens.len()).sum();
                    cur = tail;
                }
                None => flush(&mut cur, &mut cur_words, &mut out),
            }
        }
        cur.push(i);
        cur_words += e.tokens.len();
    }
    flush(&mut cur, &mut cur_words, &mut out);
    out
}

/// Consecutive segments grouped into model calls of about `limit` words.
fn plan_windows(entries: &[Entry], segments: &[Plan], limit: usize) -> Vec<Vec<usize>> {
    let words = |p: &Plan| -> usize { p.entries.iter().map(|&i| entries[i].tokens.len()).sum() };
    let mut out: Vec<Vec<usize>> = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    let mut cur_words = 0;
    for (si, p) in segments.iter().enumerate() {
        let n = words(p);
        if !cur.is_empty() && cur_words + n > limit {
            out.push(std::mem::take(&mut cur));
            cur_words = 0;
        }
        cur.push(si);
        cur_words += n;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[derive(Serialize)]
struct SegmentIn<'a> {
    id: usize,
    speaker: &'a str,
    text: String,
}

/// The window as the model reads it: a JSON list of segments with ids local
/// to the window.
fn window_message(entries: &[Entry], segments: &[Plan], window: &[usize]) -> String {
    let list: Vec<SegmentIn> = window
        .iter()
        .enumerate()
        .map(|(id, &si)| {
            let p = &segments[si];
            let label = entries[p.entries[0]].label.as_str();
            SegmentIn {
                id,
                speaker: if label.is_empty() { "Speaker" } else { label },
                text: p.entries.iter().flat_map(|&i| entries[i].tokens.iter().cloned()).collect::<Vec<_>>().join(" "),
            }
        })
        .collect();
    serde_json::to_string_pretty(&list).unwrap_or_default()
}

const CLEAN_PROMPT: &str = "You turn raw meeting-transcript segments into a clean read, the way a professional transcriber writes clean verbatim.

The user message is a JSON list of segments in order: {\"id\": 0, \"speaker\": \"…\", \"text\": \"…\"}. Each segment is one speaker's words, exactly as a speech recogniser heard them. Read the segments around each one as context.

For every segment, write its text cleaned:
- remove hesitation sounds and filler words that carry no meaning where they stand, in whatever language is spoken;
- remove stutters and words said twice by accident;
- \"...\" and \"…\" mark where the speaker paused or broke off and never appear in the clean text: when the speaker breaks off and starts again, keep only the finished version; join a sentence a pause only interrupted; end a thought left unfinished with a dash (\"–\") rather than dropping it, and drop only a fragment that says nothing on its own;
- repair words the recogniser split or ran together;
- fix grammar, punctuation and capitalisation where the speech left them broken, so every sentence reads correctly;
- write numbers, percentages and dates the way they are normally written in that language.

Keep the speaker's own words, word order and meaning wherever they are already fine. Never summarise, shorten what was actually said, add information, answer a question, translate, or move words to another segment. Keep every negation, number, name, abbreviation and technical term as said. Keep each segment in its own language; a segment that mixes languages stays mixed. If a segment is nothing but filler (\"Mm.\", \"Eh.\"), its text is empty.

Example. Segments:
[{\"id\": 0, \"speaker\": \"Anna\", \"text\": \"So we, uh, we should ... we should move the launch to ... Friday. Mm. Because the, the tests ... the tests aren't done. And the budget is ...\"}, {\"id\": 1, \"speaker\": \"Ben\", \"text\": \"Yeah. Yeah, okay. And then ... I'll, I'll tell the ... Should I tell the team?\"}]
Answer:
{\"segments\": [{\"id\": 0, \"text\": \"So we should move the launch to Friday, because the tests aren't done. And the budget is –\"}, {\"id\": 1, \"text\": \"Yeah, okay. Should I tell the team?\"}]}

Answer with JSON only, no explanation: {\"segments\": [{\"id\": 0, \"text\": \"…\"}, …]}, one entry for every id you were given.";

/// Most vocabulary terms the prompt carries.
const PROMPT_TERMS: usize = 200;

/// The prompt, with the note's vocabulary (the organisation's and the user's)
/// so a name the recogniser misheard is written as it should be.
fn clean_prompt(terms: &[String]) -> String {
    if terms.is_empty() {
        return CLEAN_PROMPT.to_string();
    }
    format!(
        "{CLEAN_PROMPT}\n\nNames and terms these meetings use, spelled as they should be: {}. Where the recogniser misheard one of them, write it this way.",
        terms.join(", ")
    )
}

fn note_vocabulary(app: &AppHandle, note_id: &str) -> anyhow::Result<Vec<String>> {
    let state: State<AppState> = app.state();
    let conn = state.db.lock();
    let workspace = db::get_note(&conn, note_id)?.workspace_id;
    let user = db::get_setting(&conn, "custom_vocabulary")?.unwrap_or_default();
    let org = crate::org_policy::vocabulary_for(&conn, Some(&workspace));
    Ok(super::transcribe::merge_vocabulary(&org, &user, super::transcribe::VocabCap::Terms(PROMPT_TERMS)))
}

/// The list a model's JSON answer gives under `key`, possibly wrapped in prose
/// or a code fence, or as a bare list; `None` when the answer isn't one.
pub(super) fn answer_list(raw: &str, key: &str) -> Option<Vec<serde_json::Value>> {
    if let (Some(a), Some(b)) = (raw.find('{'), raw.rfind('}')) {
        if a < b {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw[a..=b]) {
                if let Some(arr) = v.get(key).and_then(|s| s.as_array()) {
                    return Some(arr.clone());
                }
            }
        }
    }
    if let (Some(a), Some(b)) = (raw.find('['), raw.rfind(']')) {
        if a < b {
            if let Ok(serde_json::Value::Array(arr)) = serde_json::from_str::<serde_json::Value>(&raw[a..=b]) {
                return Some(arr);
            }
        }
    }
    None
}

/// A JSON number, or a number written as a string.
pub(super) fn json_int(v: &serde_json::Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

/// The clean texts a model's answer gives, by window-local id.
fn parse_segments(raw: &str) -> Option<HashMap<usize, String>> {
    let items = answer_list(raw, "segments")?;
    Some(
        items
            .iter()
            .filter_map(|v| {
                let id = usize::try_from(json_int(v.get("id")?)?).ok()?;
                Some((id, v.get("text")?.as_str()?.to_string()))
            })
            .collect(),
    )
}


/// The pairs `(clean index, verbatim index)` of a longest common subsequence
/// of the two word lists, compared folded. Words that fold to nothing (a lone
/// dash) never match.
fn align(clean: &[String], verbatim: &[String]) -> Vec<(usize, usize)> {
    let a: Vec<String> = clean.iter().map(|t| fold_word(t)).collect();
    let b: Vec<String> = verbatim.iter().map(|t| fold_word(t)).collect();
    let (n, m) = (a.len(), b.len());
    let mut dp = vec![0u16; (n + 1) * (m + 1)];
    let at = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[at(i, j)] = if !a[i].is_empty() && a[i] == b[j] {
                dp[at(i + 1, j + 1)] + 1
            } else {
                dp[at(i + 1, j)].max(dp[at(i, j + 1)])
            };
        }
    }
    let mut pairs = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if !a[i].is_empty() && a[i] == b[j] {
            pairs.push((i, j));
            i += 1;
            j += 1;
        } else if dp[at(i + 1, j)] >= dp[at(i, j + 1)] {
            i += 1;
        } else {
            j += 1;
        }
    }
    pairs
}

/// Whether `tokens` hold a negation. "Ikke sant?" / ", ikke sant," is a tag
/// ("right?") that a rewrite may drop, not a negation; "ikke sant." is one.
fn has_negation(tokens: &[String]) -> bool {
    tokens.iter().enumerate().any(|(i, t)| {
        let tag = tokens
            .get(i + 1)
            .is_some_and(|next| fold_word(next) == "sant" && (next.ends_with('?') || next.ends_with(',')));
        NEGATIONS.contains(&fold_word(t).as_str()) && !tag
    })
}

/// The numbers written in digits, each as its digits alone, so a rewrite may
/// group or punctuate them differently: "2400", "2 400" and "2.400" are one
/// number, as are "12.30" and "12:30". A space joins only a group of three.
fn numbers(tokens: &[String]) -> Vec<String> {
    let chars: Vec<char> = tokens.join(" ").chars().collect();
    let digits_from = |k: usize| chars[k..].iter().take_while(|c| c.is_ascii_digit()).count();
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut k = 0;
    while k < chars.len() {
        let c = chars[k];
        if c.is_ascii_digit() {
            cur.push(c);
        } else if !cur.is_empty() {
            let next = k + 1 < chars.len() && chars[k + 1].is_ascii_digit();
            let joins = match c {
                '.' | ',' | ':' => next,
                ' ' | '\u{a0}' | '\u{202f}' => next && digits_from(k + 1) == 3,
                _ => false,
            };
            if !joins {
                out.push(std::mem::take(&mut cur));
            }
        }
        k += 1;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// A dash ends a broken-off thought, so punctuation after it ("–?", from a
/// recogniser's "...?") goes.
fn tidy_dash(token: &str) -> String {
    let mut chars = token.chars();
    match chars.next() {
        Some(dash @ ('–' | '—')) if !token[dash.len_utf8()..].is_empty() && chars.all(|c| c.is_ascii_punctuation()) => {
            dash.to_string()
        }
        _ => token.to_string(),
    }
}

/// Words that are spoken, not pause marks or dashes.
fn spoken(tokens: &[String]) -> usize {
    tokens.iter().filter(|t| !fold_word(t).is_empty()).count()
}

/// Clean words the speaker said: those aligned to a verbatim word, plus words
/// the recogniser ran together and the rewrite split ("Craftdu" → "Craft du")
/// or split and the rewrite joined.
fn said_words(clean: &[String], verbatim: &[String], pairs: &[(usize, usize)]) -> usize {
    let verb: Vec<String> = verbatim.iter().map(|t| fold_word(t)).collect();
    let singles: HashSet<&str> = verb.iter().map(String::as_str).filter(|s| !s.is_empty()).collect();
    let pairs_joined: HashSet<String> = verb.windows(2).map(|w| format!("{}{}", w[0], w[1])).collect();
    let c: Vec<String> = clean.iter().map(|t| fold_word(t)).collect();
    let mut said: HashSet<usize> = pairs.iter().map(|&(i, _)| i).collect();
    for i in 0..c.len() {
        if said.contains(&i) || c[i].is_empty() {
            continue;
        }
        if pairs_joined.contains(&c[i]) {
            said.insert(i);
        } else if i + 1 < c.len() && singles.contains(format!("{}{}", c[i], c[i + 1]).as_str()) {
            said.insert(i);
            said.insert(i + 1);
        }
    }
    said.len()
}

/// Why a rewrite is refused, if it is. The segment then stays verbatim.
fn refuse(verbatim: &[String], clean: &[String], aligned: usize) -> Option<&'static str> {
    let (said, written) = (spoken(verbatim), spoken(clean));
    if written == 0 {
        return (said > DROP_MAX_WORDS).then_some("emptied a segment that had content");
    }
    if has_negation(verbatim) && !has_negation(clean) {
        return Some("dropped a negation");
    }
    let written_numbers = numbers(clean);
    if numbers(verbatim).iter().any(|n| !written_numbers.contains(n)) {
        return Some("dropped a number");
    }
    if said >= LENGTH_GUARD_MIN_WORDS && (written as f32) < MIN_KEPT_SHARE * said as f32 {
        return Some("shortened too much");
    }
    if written >= ALIGN_GUARD_MIN_WORDS && (aligned as f32) < MIN_ALIGNED_SHARE * written as f32 {
        return Some("wrote words that weren't said");
    }
    None
}

/// A segment's paragraph from the model's text for it: the guarded text, or
/// the verbatim words when a guard refused the rewrite or the model gave none.
fn clean_turn(entries: &[Entry], plan: &Plan, answer: Option<&str>) -> (CleanTurn, Option<&'static str>) {
    let first = &entries[plan.entries[0]];
    let verbatim: Vec<String> = plan.entries.iter().flat_map(|&i| entries[i].tokens.iter().cloned()).collect();
    let (text, refused) = match answer {
        None => (verbatim.join(" "), Some("no text for it in the answer")),
        Some(answer) => {
            let clean: Vec<String> = answer.split_whitespace().map(tidy_dash).collect();
            let pairs = align(&clean, &verbatim);
            match refuse(&verbatim, &clean, said_words(&clean, &verbatim, &pairs)) {
                Some(why) => (verbatim.join(" "), Some(why)),
                None => (clean.join(" "), None),
            }
        }
    };
    let turn = CleanTurn {
        id: uuid::Uuid::new_v4().simple().to_string(),
        members: plan.entries.iter().map(|&i| entries[i].fp.clone()).collect(),
        start_ms: first.start_ms,
        speaker: first.label.clone(),
        speaker_set: false,
        text,
        edited: false,
    };
    (turn, refused)
}

/// Lifecycle of a clean pass, on its own channel so cleaning note B never
/// touches note A's view: `active` brackets the model calls, `done`/`total`
/// count them.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CleanStatusPayload {
    note_id: String,
    active: bool,
    done: usize,
    total: usize,
    /// Set on the closing event when the pass failed.
    error: Option<String>,
}

fn emit_status(app: &AppHandle, note_id: &str, active: bool, done: usize, total: usize, error: Option<String>) {
    let _ = app.emit(
        "clean_status",
        CleanStatusPayload { note_id: note_id.to_string(), active, done, total, error },
    );
}

/// What a clean pass did.
#[derive(Clone, Serialize, Default, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CleanOutcome {
    /// Segments the model was asked to clean.
    pub segments: usize,
    /// Of those, the ones kept verbatim by a guard.
    pub kept_verbatim: usize,
}

/// A take's timeline entries as the clean pass and the view read them.
fn take_entries(dir: &Path) -> Vec<Entry> {
    super::timeline::read_timeline_values(&dir.join("timeline.jsonl"))
        .iter()
        .map(|v| {
            let tokens = entry_tokens(v);
            Entry {
                label: v.get("label").and_then(|s| s.as_str()).unwrap_or("").to_string(),
                start_ms: v.get("start_ms").and_then(|s| s.as_u64()).unwrap_or(0),
                fp: fingerprint(&tokens),
                tokens,
            }
        })
        .collect()
}

/// The note's takes in order: session id and entries.
fn note_takes(app: &AppHandle, note_id: &str) -> anyhow::Result<Vec<(String, Vec<Entry>)>> {
    let recordings = sessions::recordings_dir(&app.path().app_data_dir()?, note_id);
    Ok(sessions::resolve_sessions(&recordings)
        .into_iter()
        .map(|(entry, dir)| (entry.id, take_entries(&dir)))
        .collect())
}

/// Where a stored take's paragraphs land on its entries now: each matched
/// paragraph with the entry it starts at, and whether a paragraph the user
/// touched no longer matches (the verbatim changed under it).
fn place_turns<'a>(entries: &[Entry], turns: &'a [CleanTurn]) -> (Vec<(usize, &'a CleanTurn)>, bool) {
    let mut placed = Vec::new();
    let mut matched = vec![false; turns.len()];
    for (i, hit) in match_turns(entries, turns).into_iter().enumerate() {
        if let Some(ti) = hit {
            matched[ti] = true;
            placed.push((i, &turns[ti]));
        }
    }
    let stale = turns.iter().zip(&matched).any(|(t, &m)| t.touched() && !m);
    (placed, stale)
}

/// What the Clean view shows of one stretch of a take.
#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum CleanItem {
    /// A paragraph of clean text.
    #[serde(rename_all = "camelCase")]
    Turn { id: String, speaker: String, text: String, start_ms: u64, edited: bool },
    /// Verbatim words no paragraph covers yet.
    #[serde(rename_all = "camelCase")]
    Gap { speaker: String, text: String, start_ms: u64, words: usize },
}

#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CleanTakeView {
    pub session_id: String,
    pub items: Vec<CleanItem>,
    /// The verbatim changed under paragraphs the user edited: the take shows
    /// its paragraphs as they are, and only "Clean again" writes it anew.
    pub stale: bool,
}

/// The Clean view of a note.
#[derive(Serialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct CleanView {
    pub takes: Vec<CleanTakeView>,
    /// Verbatim words in the takes, and of those the ones paragraphs cover.
    pub words: usize,
    pub cleaned_words: usize,
    /// Paragraphs the user edited or gave another speaker.
    pub edited: usize,
    /// The stored document is of another version, so it is neither shown nor
    /// written.
    pub unreadable: bool,
}

/// One take as the Clean view shows it, with its verbatim and covered words.
/// A paragraph keeps the entries' current speaker unless the user chose one.
fn take_view(session_id: &str, entries: &[Entry], take: Option<&CleanTake>) -> (CleanTakeView, usize, usize) {
    let words: usize = entries.iter().map(|e| e.tokens.len()).sum();
    let turns = take.map(|t| t.turns.as_slice()).unwrap_or_default();
    let (placed, stale) = place_turns(entries, turns);
    let paragraph = |t: &CleanTurn, speaker: &str| CleanItem::Turn {
        id: t.id.clone(),
        speaker: speaker.to_string(),
        text: t.text.clone(),
        start_ms: t.start_ms,
        edited: t.edited,
    };
    if stale {
        let items = turns.iter().filter(|t| !t.text.trim().is_empty()).map(|t| paragraph(t, &t.speaker)).collect();
        return (CleanTakeView { session_id: session_id.to_string(), items, stale }, words, words);
    }
    let starts: HashMap<usize, &CleanTurn> = placed.into_iter().collect();
    let mut items: Vec<CleanItem> = Vec::new();
    let mut cleaned = 0;
    let mut i = 0;
    while i < entries.len() {
        let e = &entries[i];
        if let Some(t) = starts.get(&i) {
            let n = t.members.len();
            cleaned += entries[i..i + n].iter().map(|e| e.tokens.len()).sum::<usize>();
            if !t.text.trim().is_empty() {
                items.push(paragraph(t, if t.speaker_set { &t.speaker } else { &e.label }));
            }
            i += n;
            continue;
        }
        if !e.tokens.is_empty() {
            match items.last_mut() {
                Some(CleanItem::Gap { speaker, text, words, .. }) if *speaker == e.label => {
                    text.push(' ');
                    text.push_str(&e.tokens.join(" "));
                    *words += e.tokens.len();
                }
                _ => items.push(CleanItem::Gap {
                    speaker: e.label.clone(),
                    text: e.tokens.join(" "),
                    start_ms: e.start_ms,
                    words: e.tokens.len(),
                }),
            }
        }
        i += 1;
    }
    (CleanTakeView { session_id: session_id.to_string(), items, stale }, words, cleaned)
}

/// The Clean view of every take, from the stored document.
fn build_view(takes: &[(String, Vec<Entry>)], doc: Option<&CleanDoc>) -> CleanView {
    let Some(doc) = doc else {
        return CleanView { unreadable: true, ..Default::default() };
    };
    let mut view = CleanView::default();
    for (session, entries) in takes {
        let (take, words, cleaned) = take_view(session, entries, doc.take(session));
        view.words += words;
        view.cleaned_words += cleaned;
        view.takes.push(take);
    }
    view.edited = doc
        .takes
        .iter()
        .filter(|t| takes.iter().any(|(s, _)| *s == t.session))
        .flat_map(|t| &t.turns)
        .filter(|t| t.touched())
        .count();
    view
}

fn read_doc(conn: &rusqlite::Connection, note_id: &str) -> anyhow::Result<CleanDoc> {
    parse_doc(&db::get_note(conn, note_id)?.clean_transcript).ok_or_else(|| anyhow::anyhow!(UNREADABLE_DOC))
}

fn write_doc(conn: &rusqlite::Connection, note_id: &str, doc: &CleanDoc) -> anyhow::Result<()> {
    db::set_clean_transcript(conn, note_id, &serde_json::to_string(doc)?)?;
    Ok(())
}

/// A take's paragraphs after a pass wrote `fresh` ones, each with the entry it
/// starts at: the stored paragraphs that still match, edits included, and a
/// fresh one wherever none does. `replace` ("Clean again") drops what was
/// stored. `None` when a paragraph the user touched no longer matches: the
/// take changed under the pass, and only "Clean again" may write it.
fn merge_take(entries: &[Entry], stored: &[CleanTurn], fresh: &[(usize, CleanTurn)], replace: bool) -> Option<Vec<CleanTurn>> {
    let mut covered = vec![false; entries.len()];
    let mut turns: Vec<(usize, CleanTurn)> = Vec::new();
    if !replace {
        let (placed, stale) = place_turns(entries, stored);
        if stale {
            return None;
        }
        for (i, t) in placed {
            covered[i..i + t.members.len()].iter_mut().for_each(|c| *c = true);
            turns.push((i, t.clone()));
        }
    }
    for (i, t) in fresh {
        let span = *i..*i + t.members.len();
        if span.end <= entries.len() && !covered[span.clone()].iter().any(|&c| c) {
            covered[span].iter_mut().for_each(|c| *c = true);
            turns.push((*i, t.clone()));
        }
    }
    turns.sort_by_key(|(i, _)| *i);
    Some(turns.into_iter().map(|(_, t)| t).collect())
}

/// Store a pass's paragraphs for a take (`merge_take`), read and written
/// under the database lock so an edit made while the model worked is kept.
/// False when the take changed under the pass and nothing was written.
fn write_take(
    app: &AppHandle,
    note_id: &str,
    session: &str,
    entries: &[Entry],
    by: &str,
    fresh: &[(usize, CleanTurn)],
    replace: bool,
) -> anyhow::Result<bool> {
    let state: State<AppState> = app.state();
    let conn = state.db.lock();
    let mut doc = read_doc(&conn, note_id)?;
    let stored = doc.take(session).map(|t| t.turns.as_slice()).unwrap_or_default();
    let Some(turns) = merge_take(entries, stored, fresh, replace) else {
        return Ok(false);
    };
    let take = doc.take_mut(session);
    take.by = by.to_string();
    take.turns = turns;
    write_doc(&conn, note_id, &doc)?;
    Ok(true)
}

/// Clean every take of `note_id` that has entries no paragraph covers (every
/// entry for `CleanRequest::All`). A take whose verbatim changed under
/// paragraphs the user edited is left alone, except by "Clean again".
async fn clean_note_inner(app: &AppHandle, note_id: &str, request: CleanRequest) -> anyhow::Result<CleanOutcome> {
    let force = request == CleanRequest::All;
    let takes = note_takes(app, note_id)?;
    let doc = {
        let state: State<AppState> = app.state();
        let conn = state.db.lock();
        read_doc(&conn, note_id)?
    };
    let plans: Vec<Vec<Plan>> = takes
        .iter()
        .map(|(session, entries)| {
            let mut covered = vec![false; entries.len()];
            if !force {
                let turns = doc.take(session).map(|t| t.turns.as_slice()).unwrap_or_default();
                let (placed, stale) = place_turns(entries, turns);
                if stale {
                    return Vec::new();
                }
                for (i, t) in placed {
                    covered[i..i + t.members.len()].iter_mut().for_each(|c| *c = true);
                }
            }
            plan_segments(entries, &|i| !entries[i].tokens.is_empty() && !covered[i])
        })
        .collect();
    if plans.iter().all(Vec::is_empty) {
        return Ok(CleanOutcome::default());
    }

    let provider = light_provider(app, note_id, request, LightJob::Clean).await?;
    let prompt = clean_prompt(&note_vocabulary(app, note_id)?);
    let by = format!("{}:{}", provider.provider.as_str(), provider.model);
    let limit = window_words(provider.provider);
    let windows: Vec<Vec<Vec<usize>>> = takes
        .iter()
        .zip(&plans)
        .map(|((_, entries), segments)| plan_windows(entries, segments, limit))
        .collect();
    let total: usize = windows.iter().map(Vec::len).sum();

    emit_status(app, note_id, true, 0, total, None);
    let mut outcome = CleanOutcome::default();
    let mut done = 0usize;
    let mut wrote = false;
    let mut failure: Option<anyhow::Error> = None;
    'takes: for (((session, entries), segments), windows) in takes.iter().zip(&plans).zip(&windows) {
        if windows.is_empty() {
            continue;
        }
        let mut fresh: Vec<(usize, CleanTurn)> = Vec::new();
        let (provider, prompt) = (&provider, prompt.as_str());
        // By index: a closure over borrowed windows makes the spawned pass
        // fail to prove `Send`.
        let calls = (0..windows.len()).map(|wi| {
            let message = window_message(entries, segments, &windows[wi]);
            async move { (wi, provider.complete(prompt, &message, |_| {}).await) }
        });
        let mut answers = stream::iter(calls).buffer_unordered(CONCURRENT_CALLS);
        while let Some((wi, answer)) = answers.next().await {
            let answer = match answer {
                Ok(a) => a,
                Err(e) => {
                    failure = Some(e);
                    break 'takes;
                }
            };
            // An answer that isn't one (prose, a refusal) leaves its segments
            // verbatim, marked as read: asking again would most likely get the
            // same.
            let texts = parse_segments(&answer).unwrap_or_else(|| {
                eprintln!("[clean] note {note_id}: unusable answer, keeping a window verbatim");
                HashMap::new()
            });
            for (local, &si) in windows[wi].iter().enumerate() {
                let plan = &segments[si];
                let (turn, refused) = clean_turn(entries, plan, texts.get(&local).map(String::as_str));
                outcome.segments += 1;
                if let Some(why) = refused {
                    outcome.kept_verbatim += 1;
                    eprintln!("[clean] note {note_id}: kept a segment verbatim ({why})");
                }
                fresh.push((plan.entries[0], turn));
            }
            // Written per call, so the reader fills in while the rest is
            // cleaned. "Clean again" writes once per take: until then the
            // reader keeps the paragraphs it has.
            if !force {
                match write_take(app, note_id, session, entries, &by, &fresh, false) {
                    Ok(true) => wrote = true,
                    Ok(false) => continue 'takes,
                    Err(e) => {
                        failure = Some(e);
                        break 'takes;
                    }
                }
            }
            done += 1;
            emit_status(app, note_id, true, done, total, None);
        }
        if force {
            write_take(app, note_id, session, entries, &by, &fresh, true)?;
            wrote = true;
        }
    }
    if wrote {
        super::note_changed_for_sync(app, note_id);
    }
    if let Some(e) = failure {
        return Err(e);
    }
    eprintln!(
        "[clean] note {note_id}: cleaned {} segments, {} kept verbatim ({by})",
        outcome.segments, outcome.kept_verbatim
    );
    Ok(outcome)
}

/// The provider a pass calls for `job`: the note's summary provider on the
/// job's model. Provider first, then its key: the summary path's rule. An
/// `Auto` pass leaves Copilot, which counts requests, alone.
pub(super) async fn light_provider(
    app: &AppHandle,
    note_id: &str,
    request: CleanRequest,
    job: LightJob,
) -> anyhow::Result<ResolvedProvider> {
    let state: State<AppState> = app.state();
    let provider_id = {
        let conn = state.db.lock();
        let n = db::get_note(&conn, note_id)?;
        summary_provider_id(&conn, &n)?
    };
    if request == CleanRequest::Auto && provider_id == ProviderId::Copilot {
        anyhow::bail!(COPILOT_ON_REQUEST);
    }
    let api_key = match provider_id.keychain_account() {
        Some(_) => super::read_provider_api_key(&state, provider_id.as_str()).map_err(|e| anyhow::anyhow!("{e}"))?,
        None => None,
    };
    let mut provider = {
        let conn = state.db.lock();
        let n = db::get_note(&conn, note_id)?;
        resolve_provider(&conn, &n, api_key)?
    };
    let chosen = match job {
        LightJob::Clean => clean_model_for(super::raw_setting(app, "clean_model").as_deref(), provider.provider),
        LightJob::Title | LightJob::SpeakerCheck => None,
    };
    use_light_model(app, &mut provider, job, chosen).await?;
    Ok(provider)
}

/// Run a pass unless one is running on the note already.
async fn clean_note_guarded(
    app: &AppHandle,
    note_id: &str,
    request: CleanRequest,
    turns: bool,
    clean: bool,
) -> anyhow::Result<CleanOutcome> {
    if !running().lock().insert(note_id.to_string()) {
        return Ok(CleanOutcome::default());
    }
    run_claimed(app, note_id, request, turns, clean).await
}

/// A pass for a caller that holds the note's place in `running()`, released
/// at the end with the closing status: the speaker changes first when `turns`
/// (`boundaries::review_note`), then the clean pass when `clean`. A failed
/// speaker check is logged and the clean pass goes on.
async fn run_claimed(
    app: &AppHandle,
    note_id: &str,
    request: CleanRequest,
    turns: bool,
    clean: bool,
) -> anyhow::Result<CleanOutcome> {
    let result = async {
        if super::org::note_busy(app, note_id) {
            anyhow::bail!("This note is being recorded or transcribed. Clean runs when it's done.");
        }
        if turns {
            if let Err(e) = super::boundaries::review_note(app, note_id, request).await {
                eprintln!("[turns] note {note_id}: {e:#}");
            }
        }
        if clean {
            clean_note_inner(app, note_id, request).await
        } else {
            Ok(CleanOutcome::default())
        }
    }
    .await;
    // A note trashed and purged while the model was working must not keep the
    // files this pass just wrote.
    super::notes::purge_note_assets_if_gone(app, note_id);
    running().lock().remove(note_id);
    // Always close the bracket, so the reader leaves "Cleaning…" and reloads
    // the timeline (or shows why it couldn't).
    emit_status(app, note_id, false, 0, 0, result.as_ref().err().map(|e| e.to_string()));
    result
}

/// The entry point of the chains that write a take's timeline (post-stop,
/// deferred transcription, re-diarize), spawned rather than awaited: the
/// speaker check and the clean pass, each when its setting is on. Silent: nobody
/// asked for this pass, so nobody is told when it doesn't run (no summary
/// provider set up, a governed note, a model that's down).
pub(crate) async fn clean_note_quietly(app: AppHandle, note_id: String) {
    let turns = speaker_check_enabled(super::raw_setting(&app, "speaker_check").as_deref());
    let clean = clean_setting_on(&app);
    if !turns && !clean {
        return;
    }
    // The chain that spawned this still holds the note (a stop in progress, a
    // deferred transcription), and the reader may have started a pass that
    // skips the speakers: wait for both to let go rather than skip ours.
    let mut waited = std::time::Duration::ZERO;
    while super::org::note_busy(&app, &note_id) || !running().lock().insert(note_id.clone()) {
        if waited >= BUSY_WAIT_LIMIT {
            eprintln!("[clean] note {note_id}: still busy, not cleaning now");
            return;
        }
        tokio::time::sleep(BUSY_POLL).await;
        waited += BUSY_POLL;
    }
    if let Err(e) = run_claimed(&app, &note_id, CleanRequest::Auto, turns, clean).await {
        eprintln!("[clean] note {note_id}: {e:#}");
    }
}

/// Whether the note's summary provider is set up and allowed for it, which a
/// clean pass needs.
#[tauri::command]
pub async fn note_clean_available(app: AppHandle, note_id: String) -> bool {
    clean_available(&app, &note_id).unwrap_or(false)
}

fn clean_available(app: &AppHandle, note_id: &str) -> anyhow::Result<bool> {
    let state: State<AppState> = app.state();
    let provider_id = {
        let conn = state.db.lock();
        let n = db::get_note(&conn, note_id)?;
        summary_provider_id(&conn, &n)?
    };
    if provider_id == ProviderId::Copilot {
        let signed_in = crate::keychain::read(&state.keychain_cache, crate::copilot::CREDENTIALS_ACCOUNT);
        return Ok(matches!(signed_in, Ok(Some(_))));
    }
    let api_key = match provider_id.keychain_account() {
        Some(_) => super::read_provider_api_key(&state, provider_id.as_str()).map_err(|e| anyhow::anyhow!("{e}"))?,
        None => None,
    };
    let conn = state.db.lock();
    let n = db::get_note(&conn, note_id)?;
    Ok(resolve_provider(&conn, &n, api_key).is_ok())
}

/// Clean a note from the reader: on opening Clean on words no paragraph
/// covers (`Auto`), on "Clean now" (`Missing`) or on "Clean again" (`All`),
/// which checks the speaker changes anew first when that's on. The reader's
/// other requests change no speakers: they also run on notes the user only reads.
#[tauri::command]
pub async fn note_clean(app: AppHandle, note_id: String, request: CleanRequest) -> Result<CleanOutcome, String> {
    let turns = request == CleanRequest::All && speaker_check_enabled(super::raw_setting(&app, "speaker_check").as_deref());
    clean_note_guarded(&app, &note_id, request, turns, true).await.map_err(|e| e.to_string())
}

/// The Clean view of a note: its paragraphs, and verbatim stretches no
/// paragraph covers yet.
#[tauri::command]
pub fn note_clean_view(app: AppHandle, note_id: String) -> Result<CleanView, String> {
    let takes = note_takes(&app, &note_id).map_err(|e| e.to_string())?;
    let state: State<AppState> = app.state();
    let stored = db::get_note(&state.db.lock(), &note_id).map_err(|e| e.to_string())?.clean_transcript;
    Ok(build_view(&takes, parse_doc(&stored).as_ref()))
}

/// The user's change to a paragraph: new text, another speaker, or both.
#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct CleanEdit {
    pub text: Option<String>,
    pub speaker: Option<String>,
}

/// Apply `edit` to the paragraph `turn_id` of the take `session_id`. A
/// paragraph the user changed stays as they left it when the verbatim is
/// cleaned again.
fn apply_edit(doc: &mut CleanDoc, session_id: &str, turn_id: &str, edit: &CleanEdit) -> Result<bool, String> {
    let turn = doc
        .takes
        .iter_mut()
        .filter(|t| t.session == session_id)
        .flat_map(|t| t.turns.iter_mut())
        .find(|t| t.id == turn_id)
        .ok_or("This paragraph is gone: the transcript was cleaned again.")?;
    let mut changed = false;
    if let Some(text) = &edit.text {
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if text.is_empty() {
            return Err("A paragraph can't be empty.".into());
        }
        if text != turn.text {
            turn.text = text;
            turn.edited = true;
            changed = true;
        }
    }
    if let Some(speaker) = &edit.speaker {
        if !turn.speaker_set || *speaker != turn.speaker {
            turn.speaker = speaker.clone();
            turn.speaker_set = true;
            changed = true;
        }
    }
    Ok(changed)
}

#[tauri::command]
pub fn note_clean_edit(
    app: AppHandle,
    note_id: String,
    session_id: String,
    turn_id: String,
    edit: CleanEdit,
) -> Result<(), String> {
    {
        let state: State<AppState> = app.state();
        let conn = state.db.lock();
        let mut doc = read_doc(&conn, &note_id).map_err(|e| e.to_string())?;
        if !apply_edit(&mut doc, &session_id, &turn_id, &edit)? {
            return Ok(());
        }
        write_doc(&conn, &note_id, &doc).map_err(|e| e.to_string())?;
    }
    super::note_changed_for_sync(&app, &note_id);
    Ok(())
}

/// Rename a speaker in the paragraphs, as `note_timeline_rename` does in the
/// timelines. Whether anything changed.
pub(super) fn rename_speaker(conn: &rusqlite::Connection, note_id: &str, old: &str, new: &str) -> anyhow::Result<bool> {
    let Some(mut doc) = parse_doc(&db::get_note(conn, note_id)?.clean_transcript) else {
        return Ok(false);
    };
    let mut changed = false;
    for turn in doc.takes.iter_mut().flat_map(|t| t.turns.iter_mut()).filter(|t| t.speaker == old) {
        turn.speaker = new.to_string();
        changed = true;
    }
    if changed {
        write_doc(conn, note_id, &doc)?;
    }
    Ok(changed)
}

/// The note's transcript with its paragraphs in place of the words they were
/// written from, labelled like the stored one, for the summary and export.
/// `None` when there are none, or when the timelines don't account for
/// `transcript`, which then stays what they read.
pub(super) fn clean_text(app: &AppHandle, note_id: &str, transcript: &str) -> Option<String> {
    let takes = note_takes(app, note_id).ok()?;
    let stored = {
        let state: State<AppState> = app.state();
        let conn = state.db.lock();
        db::get_note(&conn, note_id).ok()?.clean_transcript
    };
    let view = build_view(&takes, parse_doc(&stored).as_ref());
    let has_paragraphs = view.takes.iter().flat_map(|t| &t.items).any(|i| matches!(i, CleanItem::Turn { .. }));
    if !has_paragraphs {
        return None;
    }
    let verbatim = super::timeline::rebuild_note_transcript(app, note_id).ok()?;
    if verbatim.trim() != transcript.trim() {
        return None;
    }
    Some(view_text(&view))
}

/// A Clean view as text, one `Label: text` line per speaker's run.
fn view_text(view: &CleanView) -> String {
    view.takes
        .iter()
        .map(|take| {
            super::timeline::group_lines(take.items.iter().map(|item| match item {
                CleanItem::Turn { speaker, text, .. } | CleanItem::Gap { speaker, text, .. } => {
                    (speaker.as_str(), text.as_str())
                }
            }))
        })
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    fn entry(label: &str, text: &str, start_ms: u64) -> Entry {
        let tokens = toks(text);
        Entry { label: label.into(), fp: fingerprint(&tokens), tokens, start_ms }
    }

    #[test]
    fn clean_is_on_unless_turned_off() {
        assert!(clean_enabled(None));
        assert!(clean_enabled(Some("true")));
        assert!(!clean_enabled(Some("false")));
    }

    #[test]
    fn entry_tokens_prefer_timed_words_and_fall_back_to_text() {
        let timed = serde_json::json!({
            "text": "Hello world.",
            "words": [{"text": "Hello", "start_ms": 0, "end_ms": 1}, {"text": "world.", "start_ms": 1, "end_ms": 2}]
        });
        assert_eq!(entry_tokens(&timed), vec!["Hello", "world."]);
        let untimed = serde_json::json!({ "text": "  Hei  du. ", "words": [] });
        assert_eq!(entry_tokens(&untimed), vec!["Hei", "du."]);
    }

    #[test]
    fn fingerprint_is_stable_and_word_sensitive() {
        // Pinned: the value is stored on disk, so it must not change between
        // builds.
        assert_eq!(fingerprint(&[]), "cbf29ce484222325");
        assert_ne!(fingerprint(&toks("Ja, ehm, jeg")), fingerprint(&toks("Ja, jeg")));
        assert_ne!(fingerprint(&toks("ab c")), fingerprint(&toks("a bc")));
    }

    #[test]
    fn segments_are_one_speakers_run_and_cut_after_a_sentence() {
        let long = vec!["ord"; 60].join(" ") + ".";
        let entries = vec![
            entry("Michael", &long, 0),
            entry("Michael", "og så videre", 20_000),
            entry("Michael", &long, 22_000),
            entry("Hege", "Ja.", 40_000),
        ];
        let plans = plan_segments(&entries, &|_| true);
        // 61 + 3 fit; adding 61 more crosses 110, and the cut goes after the
        // entry that ended a sentence (the first), not mid-thought.
        assert_eq!(
            plans,
            vec![
                Plan { entries: vec![0] },
                Plan { entries: vec![1, 2] },
                Plan { entries: vec![3] }
            ]
        );
    }

    #[test]
    fn an_entry_already_cleaned_ends_the_run() {
        let entries = vec![entry("Hege", "en", 0), entry("Hege", "to", 300), entry("Hege", "tre", 600)];
        let plans = plan_segments(&entries, &|i| entries[i].tokens[0] != "to");
        assert_eq!(plans, vec![Plan { entries: vec![0] }, Plan { entries: vec![2] }]);
    }

    #[test]
    fn windows_group_segments_up_to_the_limit() {
        let entries: Vec<Entry> = (0..5).map(|i| entry(if i % 2 == 0 { "A" } else { "B" }, &vec!["ord"; 100].join(" "), i * 40_000)).collect();
        let plans = plan_segments(&entries, &|_| true);
        assert_eq!(plans.len(), 5);
        assert_eq!(plan_windows(&entries, &plans, 240), vec![vec![0, 1], vec![2, 3], vec![4]]);
        assert_eq!(plan_windows(&entries, &plans, 1000), vec![vec![0, 1, 2, 3, 4]]);
    }

    #[test]
    fn the_window_message_lists_segments_with_local_ids() {
        let entries = vec![entry("Michael", "Eh, skal vi", 0), entry("", "Ja, ehm, klart.", 2_000)];
        let plans = plan_segments(&entries, &|_| true);
        let msg: serde_json::Value = serde_json::from_str(&window_message(&entries, &plans, &[0, 1])).unwrap();
        assert_eq!(msg[0]["id"], 0);
        assert_eq!(msg[0]["speaker"], "Michael");
        assert_eq!(msg[1]["speaker"], "Speaker");
        assert_eq!(msg[1]["text"], "Ja, ehm, klart.");
    }

    #[test]
    fn parse_segments_reads_objects_fences_bare_lists_and_string_ids() {
        let got = parse_segments(r#"{"segments": [{"id": 0, "text": "Skal vi?"}, {"id": "1", "text": ""}]}"#).unwrap();
        assert_eq!(got[&0], "Skal vi?");
        assert_eq!(got[&1], "");
        let fenced = parse_segments("```json\n[{\"id\": 2, \"text\": \"Ja.\"}]\n```").unwrap();
        assert_eq!(fenced[&2], "Ja.");
        assert!(parse_segments("Here is the cleaned text.").is_none());
    }

    #[test]
    fn align_pairs_words_that_fold_alike_in_order() {
        let pairs = align(&toks("Skal vi ta gjennomgangen nå?"), &toks("Eh, skal vi, skal vi ta gjennomgangen nå?"));
        assert_eq!(pairs.len(), 5);
        // The LCS takes the later "skal vi": what's kept is what was said last.
        assert!(pairs.iter().all(|&(c, v)| v > c));
    }

    #[test]
    fn guards_keep_a_segment_verbatim() {
        let verbatim = toks("Det er, eh, ikke noe problem for oss i det hele tatt.");
        // Lost the negation.
        assert_eq!(refuse(&verbatim, &toks("Det er et problem for oss."), 5), Some("dropped a negation"));
        // Lost a number.
        let n = toks("Vi var, eh, 12 personer der.");
        assert_eq!(refuse(&n, &toks("Vi var mange personer der."), 4), Some("dropped a number"));
        // Summarised.
        let long = toks("Vi gikk gjennom alle tallene fra forrige kvartal sammen med styret.");
        assert_eq!(refuse(&long, &toks("Tallene ble gjennomgått."), 1), Some("shortened too much"));
        // Paraphrased.
        assert_eq!(
            refuse(&verbatim, &toks("Dette utgjør overhodet ingen utfordring for teamet."), 1),
            Some("wrote words that weren't said")
        );
        // Emptied real content.
        assert_eq!(refuse(&verbatim, &[], 0), Some("emptied a segment that had content"));
        // A fair clean-up passes, and so does emptying pure filler.
        let ok = toks("Det er ikke noe problem for oss i det hele tatt.");
        assert_eq!(refuse(&verbatim, &ok, ok.len()), None);
        assert_eq!(refuse(&toks("Mm."), &[], 0), None);
    }

    #[test]
    fn pause_marks_and_dashes_are_not_words_to_the_guards() {
        // An unfinished thought ended with a dash: nothing unsaid was written.
        let v = toks("Jeg tror ikke det var noen ...");
        let c = toks("Jeg tror ikke det var noen –");
        assert_eq!(refuse(&v, &c, said_words(&c, &v, &align(&c, &v))), None);
        // Pause marks don't make a segment long enough to be summarised.
        let v = toks("Og så ... Men ... Ja ... Nei ... Så ...");
        assert_eq!(refuse(&v, &toks("Ja."), 1), None);
    }

    #[test]
    fn the_clean_model_applies_only_to_the_provider_it_names() {
        assert_eq!(clean_model_for(Some("openai:gpt-6-luna"), ProviderId::OpenAi), Some("gpt-6-luna".into()));
        assert_eq!(clean_model_for(Some("local:qwen3:14b"), ProviderId::Local), Some("qwen3:14b".into()));
        assert_eq!(clean_model_for(Some("ollama:qwen3:14b"), ProviderId::Local), Some("qwen3:14b".into()));
        assert_eq!(clean_model_for(Some("openai:gpt-6-luna"), ProviderId::Anthropic), None);
        assert_eq!(clean_model_for(Some("openai:"), ProviderId::OpenAi), None);
        assert_eq!(clean_model_for(Some(""), ProviderId::OpenAi), None);
        assert_eq!(clean_model_for(None, ProviderId::OpenAi), None);
    }

    #[test]
    fn the_speaker_check_is_on_unless_turned_off() {
        assert!(speaker_check_enabled(None));
        assert!(speaker_check_enabled(Some("true")));
        assert!(!speaker_check_enabled(Some("false")));
    }

    #[test]
    fn a_dash_drops_the_punctuation_after_it() {
        assert_eq!(tidy_dash("–?"), "–");
        assert_eq!(tidy_dash("—."), "—");
        assert_eq!(tidy_dash("–"), "–");
        assert_eq!(tidy_dash("til"), "til");
        assert_eq!(tidy_dash("–ish"), "–ish");
    }

    #[test]
    fn a_tag_question_and_a_grouped_number_are_not_losses() {
        let check = |v: &str, c: &str| {
            let (v, c) = (toks(v), toks(c));
            refuse(&v, &c, said_words(&c, &v, &align(&c, &v)))
        };
        assert_eq!(check("Når du var ferdig, ikke sant, så var du der.", "Når du var ferdig, så var du der."), None);
        assert_eq!(check("Ikke sant? Det går bra.", "Det går bra."), None);
        assert_eq!(check("Det er ikke sant.", "Det er sant."), Some("dropped a negation"));
        assert_eq!(check("Det er jo 2400 dokumenter.", "Det er jo 2 400 dokumenter."), None);
        assert_eq!(check("Det er jo 2400 dokumenter.", "Det er jo mange dokumenter."), Some("dropped a number"));
    }

    #[test]
    fn a_changed_number_is_refused_and_a_rewritten_one_is_not() {
        let check = |v: &str, c: &str| {
            let (v, c) = (toks(v), toks(c));
            refuse(&v, &c, said_words(&c, &v, &align(&c, &v)))
        };
        assert_eq!(check("Vi var 15 der.", "Vi var 150 der."), Some("dropped a number"));
        assert_eq!(check("Det tar 3 dager.", "Det tar 13 dager."), Some("dropped a number"));
        assert_eq!(check("Møtet er 12.30 i morgen.", "Møtet er 12:30 i morgen."), None);
        assert_eq!(check("Fra 4,2 til 3,9 prosent.", "Fra 4,2 til 3,9 prosent."), None);
        assert_eq!(check("Det koster 2.400 kroner.", "Det koster 2 400 kroner."), None);
        // Two numbers side by side stay two.
        assert_eq!(numbers(&toks("side 2 3 og 4")), vec!["2", "3", "4"]);
    }

    #[test]
    fn words_the_recogniser_ran_together_or_split_count_as_said() {
        let v = toks("Samme Small Craftdu var også der, så vi kan transcribe på nytt.");
        let c = toks("Samme Small Craft du var også der, så vi kan transcribe pånytt.");
        assert_eq!(said_words(&c, &v, &align(&c, &v)), c.len());
        let c = toks("Samme store båt var også der.");
        assert!(said_words(&c, &v, &align(&c, &v)) < c.len());
    }

    #[test]
    fn a_paragraph_is_the_clean_text_starting_where_its_entries_do() {
        let entries = [entry("Hege", "Ja, ehm, jeg har", 4_000), entry("Hege", "jeg har notatene.", 5_000)];
        let (turn, why) = clean_turn(&entries, &Plan { entries: vec![0, 1] }, Some("Ja, jeg har notatene."));
        assert_eq!(why, None);
        assert_eq!(turn.text, "Ja, jeg har notatene.");
        assert_eq!(turn.members, vec![entries[0].fp.clone(), entries[1].fp.clone()]);
        assert_eq!((turn.start_ms, turn.speaker.as_str()), (4_000, "Hege"));
        assert!(!turn.touched());
    }

    #[test]
    fn a_refused_or_missing_segment_stays_verbatim_and_is_marked_read() {
        let e = entry("Hege", "Vi var, eh, 12 personer der.", 0);
        let plan = Plan { entries: vec![0] };
        let (turn, why) = clean_turn(std::slice::from_ref(&e), &plan, Some("Vi var mange der."));
        assert_eq!(turn.text, "Vi var, eh, 12 personer der.");
        assert_eq!(turn.members, vec![e.fp.clone()]);
        assert_eq!(why, Some("dropped a number"));
        let (turn, why) = clean_turn(&[e], &plan, None);
        assert_eq!(turn.text, "Vi var, eh, 12 personer der.");
        assert!(why.is_some());
    }

    #[test]
    fn a_paragraph_matches_only_the_exact_run_of_entries() {
        let e: Vec<Entry> = ["a", "b", "c", "d"].iter().enumerate().map(|(i, w)| entry("H", w, i as u64)).collect();
        let gone = CleanTurn { members: vec![e[3].fp.clone(), fingerprint(&toks("e"))], ..para("de", &[&e[3]], "x") };
        // "d e" no longer matches: "e" is gone (an edit).
        assert_eq!(match_turns(&e, &[para("bc", &[&e[1], &e[2]], "x"), gone]), vec![None, Some(0), None, None]);
    }

    #[test]
    fn a_paragraph_lands_only_where_it_started_and_once() {
        // "Ja." said alone later, and as the start of a longer run earlier.
        let e: Vec<Entry> = ["Ja.", "Bra.", "x", "Ja.", "y"].iter().enumerate().map(|(i, w)| entry("H", w, i as u64 * 1_000)).collect();
        let all = [
            para("ja-bra", &[&e[0], &e[1]], "Ja, bra."),
            para("x", &[&e[2]], "x"),
            para("ja", &[&e[3]], "Ja."),
            para("y", &[&e[4]], "y"),
        ];
        assert_eq!(match_turns(&e, &all), vec![Some(0), None, Some(1), Some(2), Some(3)]);
        // The later "Ja." alone never takes the earlier run's place, so the
        // run's own paragraph still fits there when a pass writes it.
        assert_eq!(match_turns(&e, &all[2..3]), vec![None, None, None, Some(0), None]);
        // Untimed entries all start at 0: each paragraph still lands once.
        let flat: Vec<Entry> = ["Ja.", "x", "Ja."].iter().map(|w| entry("H", w, 0)).collect();
        let twice = [para("a", &[&flat[0]], "Ja."), para("b", &[&flat[1]], "x"), para("c", &[&flat[2]], "Ja.")];
        assert_eq!(match_turns(&flat, &twice), vec![Some(0), Some(1), Some(2)]);
    }

    /// A paragraph written from `entries`, saying `text`.
    fn para(id: &str, entries: &[&Entry], text: &str) -> CleanTurn {
        CleanTurn {
            id: id.into(),
            members: entries.iter().map(|e| e.fp.clone()).collect(),
            start_ms: entries[0].start_ms,
            speaker: entries[0].label.clone(),
            speaker_set: false,
            text: text.into(),
            edited: false,
        }
    }

    fn take(turns: Vec<CleanTurn>) -> CleanTake {
        CleanTake { session: "s1".into(), by: String::new(), turns }
    }

    fn item_texts(view: &CleanTakeView) -> Vec<String> {
        view.items
            .iter()
            .map(|i| match i {
                CleanItem::Turn { speaker, text, .. } => format!("{speaker}: {text}"),
                CleanItem::Gap { speaker, text, .. } => format!("{speaker} (verbatim): {text}"),
            })
            .collect()
    }

    #[test]
    fn the_view_shows_paragraphs_and_verbatim_where_none_covers_the_words() {
        let e = [
            entry("Michael", "Eh, skal vi starte?", 0),
            entry("Hege", "Ja.", 2_000),
            entry("Hege", "Bra, bra.", 3_000),
            entry("Michael", "ny tekst", 4_000),
        ];
        let t = take(vec![para("a", &[&e[0]], "Skal vi starte?"), para("b", &[&e[1], &e[2]], "")]);
        let (view, words, cleaned) = take_view("s1", &e, Some(&t));
        // A paragraph cleaned down to nothing is left out; its words are covered.
        assert_eq!(item_texts(&view), ["Michael: Skal vi starte?", "Michael (verbatim): ny tekst"]);
        assert_eq!((words, cleaned), (9, 7));
        assert!(!view.stale);
    }

    #[test]
    fn a_paragraph_follows_the_speaker_of_its_words_unless_the_user_chose_one() {
        let e = [entry("Speaker 2", "Det fikser jeg.", 0), entry("Speaker 2", "Og det.", 1_000)];
        let mut chosen = para("b", &[&e[1]], "Og det.");
        chosen.speaker = "Hege".into();
        chosen.speaker_set = true;
        let t = take(vec![para("a", &[&e[0]], "Det fikser jeg."), chosen]);
        let (view, ..) = take_view("s1", &e, Some(&t));
        assert_eq!(item_texts(&view), ["Speaker 2: Det fikser jeg.", "Hege: Og det."]);
    }

    #[test]
    fn a_take_changed_under_an_edited_paragraph_shows_the_paragraphs_as_they_are() {
        let old = entry("Hege", "Vi var tolv.", 0);
        let now = [entry("Michael", "Vi var tretten.", 0)];
        let mut edited = para("a", &[&old], "Vi var 12, sa Hege.");
        edited.edited = true;
        let t = take(vec![edited]);
        let (view, words, cleaned) = take_view("s1", &now, Some(&t));
        assert!(view.stale);
        assert_eq!(item_texts(&view), ["Hege: Vi var 12, sa Hege."]);
        assert_eq!(words, cleaned);
    }

    #[test]
    fn a_pass_keeps_edits_and_fills_only_the_gaps() {
        let e = [entry("Hege", "Eh, ja.", 0), entry("Hege", "Vi var tolv.", 1_000), entry("Michael", "Bra.", 2_000)];
        let mut edited = para("a", &[&e[1]], "Vi var tolv, sa hun.");
        edited.edited = true;
        let stale_untouched = CleanTurn { members: vec!["gone".into()], ..para("x", &[&e[0]], "Gammel.") };
        let fresh = vec![
            (0, para("f0", &[&e[0]], "Ja.")),
            (1, para("f1", &[&e[1]], "Vi var tolv.")),
            (2, para("f2", &[&e[2]], "Bra.")),
        ];
        let merged = merge_take(&e, &[stale_untouched, edited.clone()], &fresh, false).unwrap();
        let ids: Vec<&str> = merged.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["f0", "a", "f2"]);
        // "Clean again" replaces the edit.
        let merged = merge_take(&e, &[edited.clone()], &fresh, true).unwrap();
        assert_eq!(merged.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["f0", "f1", "f2"]);
        // An edited paragraph whose words changed stops the merge.
        let changed = [entry("Hege", "Eh, ja.", 0), entry("Hege", "Vi var tretten.", 1_000)];
        assert_eq!(merge_take(&changed, &[edited], &fresh[..1], false), None);
    }

    #[test]
    fn an_edit_marks_the_paragraph_and_ignores_no_change() {
        let e = entry("Hege", "Vi var tolv.", 0);
        let mut doc = CleanDoc { v: CLEAN_DOC_VERSION, takes: vec![take(vec![para("a", &[&e], "Vi var tolv.")])] };
        let same = CleanEdit { text: Some(" Vi var  tolv. ".into()), speaker: None };
        assert_eq!(apply_edit(&mut doc, "s1", "a", &same), Ok(false));
        let fix = CleanEdit { text: Some("Vi var tolv hos Cefor.".into()), speaker: Some("Hege".into()) };
        assert_eq!(apply_edit(&mut doc, "s1", "a", &fix), Ok(true));
        let t = &doc.takes[0].turns[0];
        assert_eq!((t.text.as_str(), t.edited, t.speaker_set), ("Vi var tolv hos Cefor.", true, true));
        assert!(apply_edit(&mut doc, "s1", "a", &CleanEdit { text: Some("  ".into()), speaker: None }).is_err());
        assert!(apply_edit(&mut doc, "s2", "a", &fix).is_err());
    }

    #[test]
    fn the_document_round_trips_and_another_version_is_left_alone() {
        let e = entry("Hege", "Ja.", 0);
        let mut doc = CleanDoc::default();
        doc.take_mut("s1").turns.push(para("a", &[&e], "Ja."));
        let json = serde_json::to_string(&doc).unwrap();
        assert!(!json.contains("edited"), "untouched flags stay out of the stored JSON: {json}");
        assert_eq!(parse_doc(&json), Some(doc));
        assert_eq!(parse_doc(""), Some(CleanDoc::default()));
        assert_eq!(parse_doc(r#"{"v": 2, "takes": []}"#), None);
        assert!(build_view(&[], None).unreadable);
    }

    #[test]
    fn the_prompt_carries_the_vocabulary_only_when_there_is_one() {
        assert_eq!(clean_prompt(&[]), CLEAN_PROMPT);
        let p = clean_prompt(&["Cefor".into(), "Humla".into()]);
        assert!(p.starts_with(CLEAN_PROMPT) && p.ends_with("spelled as they should be: Cefor, Humla. Where the recogniser misheard one of them, write it this way."));
    }

    #[test]
    fn the_summary_text_joins_a_speakers_paragraphs_into_one_line() {
        let e = [entry("Michael", "Eh, skal vi?", 0), entry("Michael", "Ja, la oss.", 1_000), entry("Hege", "Ok.", 2_000)];
        let t = take(vec![para("a", &[&e[0]], "Skal vi?"), para("b", &[&e[1]], "La oss.")]);
        let view = build_view(&[("s1".into(), e.to_vec())], Some(&CleanDoc { v: CLEAN_DOC_VERSION, takes: vec![t] }));
        assert_eq!(view_text(&view), "Michael: Skal vi? La oss.\nHege: Ok.");
    }
}
