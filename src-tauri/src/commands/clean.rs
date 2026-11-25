//! Clean transcripts (#217): the reader can show a take as a clean read —
//! filler words, stutters and abandoned false starts gone, grammar the speech
//! broke put right, numbers written as numbers — beside the verbatim one.
//!
//! A model rewrites the transcript one segment at a time (a run of one
//! speaker's consecutive timeline entries, about 110 words), keeping the
//! speaker's own words, order and meaning. The rewrite is then aligned back to
//! the verbatim words: every word the speaker actually said keeps its own
//! timing, and a word the model changed or added takes the time of the words
//! it replaced. So playback highlighting and click-to-seek work in Clean as in
//! Verbatim, give or take a word where the text was restructured.
//!
//! The verbatim timeline stays canonical (ADR-0004): nothing here touches
//! `timeline.jsonl` or `notes.transcript`, and summaries, chat, export, MCP and
//! webhooks keep reading verbatim. A rewrite that fails a guard — it drops a
//! negation or a number, or most of what it wrote was never said — leaves its
//! segment verbatim instead.
//!
//! Stored per take in `clean.json` beside `timeline.jsonl`, local to this Mac
//! (derived, never synced). A segment names its entries by a fingerprint of
//! their words, so a renamed speaker keeps it, and an entry whose words changed
//! (a per-turn edit, a re-diarize that moved a boundary) drops its segment back
//! to verbatim until the next pass cleans it.
//!
//! Runs after the post-stop chain and a deferred transcription, and on demand
//! when the reader asks for Clean on a take that lacks it. Uses the note's
//! summary provider through `resolve_provider`, so a governed note is refused
//! here as its summary is. Failure on the automatic path is silent, like titles.

use super::summary::{attach_copilot, resolve_provider, summary_provider_id};
use super::TITLE_FALLBACK_MODEL;
use crate::providers::ProviderId;
use crate::{db, openai, sessions, AppState};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter, Manager, State};

/// The per-take file holding the clean segments.
pub(crate) const CLEAN_FILE: &str = "clean.json";

/// Bumped when the meaning of a stored segment changes, so an older file is
/// ignored and the take cleaned again. 1 was the hide-only mask.
const CLEAN_VERSION: u32 = 2;

/// Most verbatim words in one segment: about a paragraph of one speaker, so a
/// false start and its restart land in the same segment.
const SEGMENT_WORDS: usize = 110;

/// Most verbatim words in one model call. Several segments, often several
/// speakers, so each reads in its conversation; small enough that a small
/// local model rewrites every segment rather than summarising the lot.
const WINDOW_WORDS: usize = 240;

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
/// in. Dropping "ikke" inverts a sentence; that is the one failure a clean read
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

fn clean_setting_on(app: &AppHandle) -> bool {
    clean_enabled(super::raw_setting(app, "clean_transcripts").as_deref())
}

/// One word of a clean segment, with its time in the take's own clock.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub(crate) struct CleanWord {
    pub(crate) text: String,
    pub(crate) start_ms: u64,
    pub(crate) end_ms: u64,
}

/// A run of one speaker's consecutive timeline entries, cleaned.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub(crate) struct Segment {
    /// Fingerprints of the entries it covers, in order.
    pub(crate) members: Vec<String>,
    /// The clean text; `None` when a guard kept the segment verbatim. Empty
    /// when the segment was only filler.
    pub(crate) text: Option<String>,
    /// The clean text's words with times projected from the verbatim words.
    /// Empty when the entries had no word timings.
    #[serde(default)]
    pub(crate) words: Vec<CleanWord>,
}

#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
struct CleanFile {
    v: u32,
    /// What cleaned it, for diagnostics: `<provider>:<model>`.
    #[serde(default)]
    by: String,
    #[serde(default)]
    segments: Vec<Segment>,
}

/// The words of one timeline entry as the reader shows them: its timed words,
/// or its text's whitespace tokens when the provider gave no timings.
pub(crate) fn entry_tokens(value: &serde_json::Value) -> Vec<String> {
    let words: Vec<String> = value
        .get("words")
        .and_then(|w| w.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|w| w.get("text").and_then(|t| t.as_str()).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if !words.is_empty() {
        return words;
    }
    value
        .get("text")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .split_whitespace()
        .map(str::to_string)
        .collect()
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

/// The segments stored for one take. Empty when the take was never cleaned or
/// its file is from another version.
pub(crate) fn read_segments(take_dir: &Path) -> Vec<Segment> {
    std::fs::read_to_string(take_dir.join(CLEAN_FILE))
        .ok()
        .and_then(|s| serde_json::from_str::<CleanFile>(&s).ok())
        .filter(|f| f.v == CLEAN_VERSION)
        .map(|f| f.segments)
        .unwrap_or_default()
}

/// Which stored segment, if any, starts at each entry and still matches the
/// entries that follow it. `fps` are the take's entry fingerprints in order.
/// The answer is per entry: `Some(segment index)` on a segment's first entry.
pub(crate) fn match_segments(fps: &[String], segments: &[Segment]) -> Vec<Option<usize>> {
    let mut by_first: HashMap<&str, Vec<usize>> = HashMap::new();
    for (si, s) in segments.iter().enumerate() {
        if let Some(first) = s.members.first() {
            by_first.entry(first.as_str()).or_default().push(si);
        }
    }
    let mut out = vec![None; fps.len()];
    let mut i = 0;
    while i < fps.len() {
        let hit = by_first.get(fps[i].as_str()).and_then(|candidates| {
            candidates.iter().copied().find(|&si| {
                let m = &segments[si].members;
                i + m.len() <= fps.len() && fps[i..i + m.len()] == m[..]
            })
        });
        match hit {
            Some(si) => {
                out[i] = Some(si);
                i += segments[si].members.len();
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
    /// Per token, its time when the entry had word timings.
    times: Vec<Option<(u64, u64)>>,
    fp: String,
}

/// A segment to clean: consecutive entry indices of one speaker.
#[derive(Debug, Clone, PartialEq)]
struct Plan {
    entries: Vec<usize>,
}

fn ends_sentence(token: &str) -> bool {
    let core = token.trim_end_matches(['"', '\'', ')', ']', '»', '”', '’']);
    matches!(core.chars().last(), Some('.' | '!' | '?' | '…'))
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
                .rposition(|&j| entries[j].tokens.last().is_some_and(|t| ends_sentence(t)))
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

/// Consecutive segments grouped into model calls of about `WINDOW_WORDS`.
fn plan_windows(entries: &[Entry], segments: &[Plan]) -> Vec<Vec<usize>> {
    let words = |p: &Plan| -> usize { p.entries.iter().map(|&i| entries[i].tokens.len()).sum() };
    let mut out: Vec<Vec<usize>> = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    let mut cur_words = 0;
    for (si, p) in segments.iter().enumerate() {
        let n = words(p);
        if !cur.is_empty() && cur_words + n > WINDOW_WORDS {
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
- remove hesitation sounds and filler words that carry no meaning where they stand, in whatever language is spoken (um, uh, er, eh, ehm, øh, äh, like, you know, I mean, liksom, altså, på en måte, sånn, typ, halt, quasi…);
- remove stutters and words said twice by accident;
- when the speaker starts a sentence, breaks off and starts again, keep only the finished version;
- fix grammar, punctuation and capitalisation where the speech left them broken, so every sentence reads correctly;
- write numbers, percentages and dates the way they are normally written in that language.

Keep the speaker's own words, word order and meaning wherever they are already fine. Never summarise, shorten what was actually said, add information, answer a question, translate, or move words to another segment. Keep every negation, number, name and technical term. Keep each segment in its own language; a segment that mixes languages stays mixed. If a segment is nothing but filler (\"Mm.\", \"Eh.\"), its text is empty.

Answer with JSON only, no explanation: {\"segments\": [{\"id\": 0, \"text\": \"…\"}, …]}, one entry for every id you were given.";

/// The clean texts a model's answer gives, by window-local id; `None` when the
/// answer isn't one. Accepts `{"segments": [...]}`, possibly wrapped in prose
/// or a code fence, or a bare list of `{id, text}`.
fn parse_segments(raw: &str) -> Option<HashMap<usize, String>> {
    let collect = |arr: &Vec<serde_json::Value>| -> HashMap<usize, String> {
        arr.iter()
            .filter_map(|v| {
                let id = v.get("id").and_then(|i| {
                    i.as_u64()
                        .map(|n| n as usize)
                        .or_else(|| i.as_str().and_then(|s| s.trim().parse().ok()))
                })?;
                let text = v.get("text").and_then(|t| t.as_str())?.to_string();
                Some((id, text))
            })
            .collect()
    };
    if let (Some(a), Some(b)) = (raw.find('{'), raw.rfind('}')) {
        if a < b {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw[a..=b]) {
                if let Some(arr) = v.get("segments").and_then(|s| s.as_array()) {
                    return Some(collect(arr));
                }
            }
        }
    }
    if let (Some(a), Some(b)) = (raw.find('['), raw.rfind(']')) {
        if a < b {
            if let Ok(serde_json::Value::Array(arr)) = serde_json::from_str::<serde_json::Value>(&raw[a..=b]) {
                return Some(collect(&arr));
            }
        }
    }
    None
}

/// A word folded to what two spellings of it share: alphanumerics, lowercased.
fn fold(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// The pairs `(clean index, verbatim index)` of a longest common subsequence
/// of the two word lists, compared folded. Words that fold to nothing (a lone
/// dash) never match.
fn align(clean: &[String], verbatim: &[String]) -> Vec<(usize, usize)> {
    let a: Vec<String> = clean.iter().map(|t| fold(t)).collect();
    let b: Vec<String> = verbatim.iter().map(|t| fold(t)).collect();
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

fn has_negation(tokens: &[String]) -> bool {
    tokens.iter().any(|t| NEGATIONS.contains(&fold(t).as_str()))
}

/// Every run of digits in `tokens`, in order.
fn digit_runs(tokens: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for t in tokens {
        let mut cur = String::new();
        for c in t.chars() {
            if c.is_ascii_digit() {
                cur.push(c);
            } else if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        }
        if !cur.is_empty() {
            out.push(cur);
        }
    }
    out
}

/// Why a rewrite is refused, if it is. The segment then stays verbatim.
fn refuse(verbatim: &[String], clean: &[String], aligned: usize) -> Option<&'static str> {
    if clean.is_empty() {
        return (verbatim.len() > DROP_MAX_WORDS).then_some("emptied a segment that had content");
    }
    if has_negation(verbatim) && !has_negation(clean) {
        return Some("dropped a negation");
    }
    let clean_digits = digit_runs(clean).join(" ");
    if digit_runs(verbatim).iter().any(|d| !clean_digits.contains(d.as_str())) {
        return Some("dropped a number");
    }
    if verbatim.len() >= LENGTH_GUARD_MIN_WORDS
        && (clean.len() as f32) < MIN_KEPT_SHARE * verbatim.len() as f32
    {
        return Some("shortened too much");
    }
    if clean.len() >= ALIGN_GUARD_MIN_WORDS && (aligned as f32) < MIN_ALIGNED_SHARE * clean.len() as f32 {
        return Some("wrote words that weren't said");
    }
    None
}

/// Times for the clean words: a word aligned to one the speaker said takes
/// that word's time; a run of changed or added words shares, evenly, the span
/// of the verbatim words between its aligned neighbours (or, with none between
/// them, the gap between those neighbours).
fn project_times(
    clean: &[String],
    times: &[(u64, u64)],
    pairs: &[(usize, usize)],
) -> Vec<CleanWord> {
    let mut by_clean: Vec<Option<usize>> = vec![None; clean.len()];
    for &(ci, vi) in pairs {
        by_clean[ci] = Some(vi);
    }
    let seg_start = times.first().map(|t| t.0).unwrap_or(0);
    let seg_end = times.last().map(|t| t.1).unwrap_or(seg_start);
    let mut out: Vec<CleanWord> = Vec::with_capacity(clean.len());
    let mut ci = 0;
    while ci < clean.len() {
        if let Some(vi) = by_clean[ci] {
            out.push(CleanWord { text: clean[ci].clone(), start_ms: times[vi].0, end_ms: times[vi].1 });
            ci += 1;
            continue;
        }
        // An unaligned run [ci, run_end).
        let run_end = (ci..clean.len()).find(|&k| by_clean[k].is_some()).unwrap_or(clean.len());
        let prev_v = (0..ci).rev().find_map(|k| by_clean[k]);
        let next_v = by_clean.get(run_end).copied().flatten();
        let lo_v = prev_v.map_or(0, |v| v + 1);
        let hi_v = next_v.unwrap_or(times.len());
        let (span_start, span_end) = if lo_v < hi_v {
            (times[lo_v].0, times[hi_v - 1].1)
        } else {
            (prev_v.map_or(seg_start, |v| times[v].1), next_v.map_or(seg_end, |v| times[v].0))
        };
        let span_end = span_end.max(span_start);
        let n = (run_end - ci) as u64;
        let step = (span_end - span_start) / n.max(1);
        for (k, idx) in (ci..run_end).enumerate() {
            let s = span_start + step * k as u64;
            let e = if idx + 1 == run_end { span_end } else { s + step };
            out.push(CleanWord { text: clean[idx].clone(), start_ms: s, end_ms: e.max(s) });
        }
        ci = run_end;
    }
    out
}

/// A segment's clean result from the model's text for it: the guarded text
/// and its timed words, or `text: None` (verbatim) when a guard refused it or
/// the model gave no text for it.
fn clean_segment(entries: &[Entry], plan: &Plan, answer: Option<&str>) -> (Segment, Option<&'static str>) {
    let members: Vec<String> = plan.entries.iter().map(|&i| entries[i].fp.clone()).collect();
    let verbatim: Vec<String> = plan.entries.iter().flat_map(|&i| entries[i].tokens.iter().cloned()).collect();
    let times: Vec<Option<(u64, u64)>> = plan.entries.iter().flat_map(|&i| entries[i].times.iter().copied()).collect();
    let Some(answer) = answer else {
        return (Segment { members, text: None, words: Vec::new() }, Some("no text for it in the answer"));
    };
    let clean: Vec<String> = answer.split_whitespace().map(str::to_string).collect();
    let pairs = align(&clean, &verbatim);
    if let Some(why) = refuse(&verbatim, &clean, pairs.len()) {
        return (Segment { members, text: None, words: Vec::new() }, Some(why));
    }
    let timed: Option<Vec<(u64, u64)>> = times.iter().copied().collect();
    let words = match timed {
        Some(t) if !t.is_empty() => project_times(&clean, &t, &pairs),
        _ => Vec::new(),
    };
    (Segment { members, text: Some(clean.join(" ")), words }, None)
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

/// One take's entries and the segments it already has.
struct TakeWork {
    dir: PathBuf,
    entries: Vec<Entry>,
    existing: Vec<Segment>,
}

fn take_work(dir: &Path) -> TakeWork {
    let values = super::timeline::read_timeline_values(&dir.join("timeline.jsonl"));
    let entries = values
        .iter()
        .map(|v| {
            let tokens = entry_tokens(v);
            let timed: Vec<(u64, u64)> = v
                .get("words")
                .and_then(|w| w.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|w| Some((w.get("start_ms")?.as_u64()?, w.get("end_ms")?.as_u64()?)))
                        .collect()
                })
                .unwrap_or_default();
            let times = if timed.len() == tokens.len() && !timed.is_empty() {
                timed.into_iter().map(Some).collect()
            } else {
                vec![None; tokens.len()]
            };
            Entry {
                label: v.get("label").and_then(|s| s.as_str()).unwrap_or("").to_string(),
                fp: fingerprint(&tokens),
                tokens,
                times,
            }
        })
        .collect();
    TakeWork { dir: dir.to_path_buf(), entries, existing: read_segments(dir) }
}

fn write_segments(dir: &Path, by: &str, segments: Vec<Segment>) -> std::io::Result<()> {
    let file = CleanFile { v: CLEAN_VERSION, by: by.to_string(), segments };
    let json = serde_json::to_string(&file).map_err(std::io::Error::other)?;
    let tmp = dir.join(format!("{CLEAN_FILE}.tmp"));
    std::fs::write(&tmp, json)?;
    std::fs::rename(tmp, dir.join(CLEAN_FILE))
}

/// The stored segments that still match the take's entries, and which entries
/// they cover.
fn live_segments(take: &TakeWork) -> (Vec<Segment>, HashSet<usize>) {
    let fps: Vec<String> = take.entries.iter().map(|e| e.fp.clone()).collect();
    let mut kept = Vec::new();
    let mut covered = HashSet::new();
    for (i, hit) in match_segments(&fps, &take.existing).into_iter().enumerate() {
        if let Some(si) = hit {
            let seg = take.existing[si].clone();
            covered.extend(i..i + seg.members.len());
            kept.push(seg);
        }
    }
    (kept, covered)
}

/// Clean every take of `note_id` that has entries without a segment (every
/// entry when `force`).
async fn clean_note_inner(app: &AppHandle, note_id: &str, force: bool) -> anyhow::Result<CleanOutcome> {
    if super::org::note_busy(app, note_id) {
        anyhow::bail!("This note is being recorded or transcribed. Clean runs when it's done.");
    }
    let app_dir = app.path().app_data_dir()?;
    let recordings = sessions::recordings_dir(&app_dir, note_id);
    let takes: Vec<TakeWork> = sessions::resolve_sessions(&recordings)
        .into_iter()
        .map(|(_, dir)| take_work(&dir))
        .collect();
    struct TakePlan {
        kept: Vec<Segment>,
        segments: Vec<Plan>,
        windows: Vec<Vec<usize>>,
    }
    let plans: Vec<TakePlan> = takes
        .iter()
        .map(|t| {
            let (kept, covered) = if force { (Vec::new(), HashSet::new()) } else { live_segments(t) };
            let segments = plan_segments(&t.entries, &|i| {
                !t.entries[i].tokens.is_empty() && !covered.contains(&i)
            });
            let windows = plan_windows(&t.entries, &segments);
            TakePlan { kept, segments, windows }
        })
        .collect();
    let total: usize = plans.iter().map(|p| p.windows.len()).sum();
    if total == 0 {
        return Ok(CleanOutcome::default());
    }

    // Provider first, then its key: the summary path's rule.
    let state: State<AppState> = app.state();
    let provider_id = {
        let conn = state.db.lock();
        let n = db::get_note(&conn, note_id)?;
        summary_provider_id(&conn, &n)?
    };
    let api_key = match provider_id.keychain_account() {
        Some(_) => super::read_provider_api_key(&state, provider_id.as_str()).map_err(|e| anyhow::anyhow!("{e}"))?,
        None => None,
    };
    let mut provider = {
        let conn = state.db.lock();
        let n = db::get_note(&conn, note_id)?;
        resolve_provider(&conn, &n, api_key)?
    };
    // A careful rewrite, not a reasoning task: thinking slows a pass with many
    // calls and tempts the model to improve on what was said.
    provider.think = false;
    if provider.provider == ProviderId::OpenAi && openai::is_reasoning_model(&provider.model) {
        provider.model = TITLE_FALLBACK_MODEL.to_string();
    }
    attach_copilot(app, &mut provider).await?;
    let by = format!("{}:{}", provider.provider.as_str(), provider.model);

    emit_status(app, note_id, true, 0, total, None);
    let mut outcome = CleanOutcome::default();
    let mut done = 0usize;
    let mut failure: Option<anyhow::Error> = None;
    'takes: for (take, plan) in takes.iter().zip(plans) {
        if plan.windows.is_empty() {
            continue;
        }
        let mut segments = plan.kept;
        for window in &plan.windows {
            let message = window_message(&take.entries, &plan.segments, window);
            let answer = match provider.complete(CLEAN_PROMPT, &message, |_| {}).await {
                Ok(a) => a,
                Err(e) => {
                    failure = Some(e);
                    // Keep what this take got so far; the rest is cleaned on
                    // the next pass.
                    let _ = write_segments(&take.dir, &by, segments);
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
            for (local, &si) in window.iter().enumerate() {
                let (seg, refused) = clean_segment(&take.entries, &plan.segments[si], texts.get(&local).map(String::as_str));
                outcome.segments += 1;
                if let Some(why) = refused {
                    outcome.kept_verbatim += 1;
                    eprintln!("[clean] note {note_id}: kept a segment verbatim ({why})");
                }
                segments.push(seg);
            }
            done += 1;
            emit_status(app, note_id, true, done, total, None);
        }
        write_segments(&take.dir, &by, segments)?;
    }
    // A note trashed and purged while the model was working must not keep the
    // file this pass just wrote.
    super::notes::purge_note_assets_if_gone(app, note_id);
    if let Some(e) = failure {
        return Err(e);
    }
    eprintln!(
        "[clean] note {note_id}: cleaned {} segments, {} kept verbatim ({by})",
        outcome.segments, outcome.kept_verbatim
    );
    Ok(outcome)
}

/// Run a clean pass with the one-per-note guard and the status bracket.
async fn clean_note_guarded(app: &AppHandle, note_id: &str, force: bool) -> anyhow::Result<CleanOutcome> {
    if !running().lock().insert(note_id.to_string()) {
        return Ok(CleanOutcome::default());
    }
    let result = clean_note_inner(app, note_id, force).await;
    running().lock().remove(note_id);
    // Always close the bracket, so the reader leaves "Cleaning…" and reloads
    // the timeline (or shows why it couldn't).
    emit_status(app, note_id, false, 0, 0, result.as_ref().err().map(|e| e.to_string()));
    result
}

/// The post-stop chain's entry point, spawned rather than awaited. Silent:
/// nobody asked for this pass, so nobody is told when it doesn't run (no
/// summary provider set up, a governed note, a model that's down).
pub(crate) async fn clean_note_quietly(app: AppHandle, note_id: String) {
    if !clean_setting_on(&app) {
        return;
    }
    // The chain that spawned this still holds the note (a stop in progress, a
    // deferred transcription), so wait for it to let go.
    let mut waited = std::time::Duration::ZERO;
    while super::org::note_busy(&app, &note_id) {
        if waited >= BUSY_WAIT_LIMIT {
            eprintln!("[clean] note {note_id}: still busy, not cleaning now");
            return;
        }
        tokio::time::sleep(BUSY_POLL).await;
        waited += BUSY_POLL;
    }
    if let Err(e) = clean_note_guarded(&app, &note_id, false).await {
        eprintln!("[clean] note {note_id}: {e:#}");
    }
}

/// Clean a note from the reader: on asking for Clean on a take that lacks it,
/// or on "Clean again" (`force`, every segment rewritten anew).
#[tauri::command]
pub async fn note_clean(app: AppHandle, note_id: String, force: bool) -> Result<CleanOutcome, String> {
    clean_note_guarded(&app, &note_id, force).await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    /// A timed entry whose words run 300 ms apart from `start_ms`.
    fn entry(label: &str, text: &str, start_ms: u64) -> Entry {
        let tokens = toks(text);
        let times = (0..tokens.len())
            .map(|k| Some((start_ms + 300 * k as u64, start_ms + 300 * k as u64 + 250)))
            .collect();
        Entry { label: label.into(), fp: fingerprint(&tokens), tokens, times }
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
        assert_eq!(plan_windows(&entries, &plans), vec![vec![0, 1], vec![2, 3], vec![4]]);
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
    fn kept_words_keep_their_time_and_changed_words_share_the_span_they_replace() {
        let e = entry("Hege", "Churn er, eh, den er faktisk nede, fra fire komma to prosent.", 10_000);
        let plan = Plan { entries: vec![0] };
        let (seg, refused) = clean_segment(std::slice::from_ref(&e), &plan, Some("Churn er faktisk nede, fra 4,2 prosent."));
        assert!(refused.is_none());
        assert_eq!(seg.text.as_deref(), Some("Churn er faktisk nede, fra 4,2 prosent."));
        let w = &seg.words;
        assert_eq!(w.len(), 7);
        // "Churn" was said first: its own time.
        assert_eq!((w[0].start_ms, w[0].end_ms), (10_000, 10_250));
        // "4,2" replaced "fire komma to" (verbatim 8..=10): it spans them.
        assert_eq!(w[5].text, "4,2");
        assert_eq!((w[5].start_ms, w[5].end_ms), (10_000 + 300 * 8, 10_000 + 300 * 10 + 250));
        // Times never run backwards.
        assert!(w.windows(2).all(|p| p[0].start_ms <= p[1].start_ms));
    }

    #[test]
    fn an_added_word_takes_the_gap_between_its_neighbours() {
        let words = project_times(&toks("a new b"), &[(0, 100), (500, 600)], &[(0, 0), (2, 1)]);
        assert_eq!((words[1].start_ms, words[1].end_ms), (100, 500));
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
    fn a_refused_or_missing_segment_stays_verbatim_and_is_marked_read() {
        let e = entry("Hege", "Vi var, eh, 12 personer der.", 0);
        let plan = Plan { entries: vec![0] };
        let (seg, why) = clean_segment(std::slice::from_ref(&e), &plan, Some("Vi var mange der."));
        assert_eq!(seg.text, None);
        assert_eq!(seg.members, vec![e.fp.clone()]);
        assert_eq!(why, Some("dropped a number"));
        let (seg, why) = clean_segment(&[e], &plan, None);
        assert_eq!(seg.text, None);
        assert!(why.is_some());
    }

    #[test]
    fn untimed_entries_get_text_and_no_words() {
        let tokens = toks("Eh, det fikser jeg.");
        let e = Entry { label: "Stian".into(), fp: fingerprint(&tokens), times: vec![None; tokens.len()], tokens };
        let (seg, _) = clean_segment(&[e], &Plan { entries: vec![0] }, Some("Det fikser jeg."));
        assert_eq!(seg.text.as_deref(), Some("Det fikser jeg."));
        assert!(seg.words.is_empty());
    }

    #[test]
    fn stored_segments_match_only_the_exact_run_of_entries() {
        let fps: Vec<String> = ["a", "b", "c", "d"].iter().map(|s| s.to_string()).collect();
        let seg = |m: &[&str]| Segment { members: m.iter().map(|s| s.to_string()).collect(), text: Some("x".into()), words: vec![] };
        let segments = vec![seg(&["b", "c"]), seg(&["d", "e"])];
        // "d e" no longer matches: "e" is gone (an edit).
        assert_eq!(match_segments(&fps, &segments), vec![None, Some(0), None, None]);
    }

    #[test]
    fn segments_round_trip_through_the_file_and_ignore_other_versions() {
        let dir = tempfile::tempdir().unwrap();
        let s = vec![Segment {
            members: vec!["abc".into()],
            text: Some("Hei.".into()),
            words: vec![CleanWord { text: "Hei.".into(), start_ms: 0, end_ms: 200 }],
        }];
        write_segments(dir.path(), "local:gemma", s.clone()).unwrap();
        assert_eq!(read_segments(dir.path()), s);
        std::fs::write(dir.path().join(CLEAN_FILE), r#"{"v":1,"entries":{"abc":[1]}}"#).unwrap();
        assert!(read_segments(dir.path()).is_empty());
    }
}
