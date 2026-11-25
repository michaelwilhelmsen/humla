//! Clean transcripts (#217): the reader can show a take with its filler
//! words, accidental repeats and abandoned false starts hidden.
//!
//! Clean is a *view* of the timeline, never a second transcript. A model reads
//! the timed words turn by turn and answers with which of them to hide; it
//! never writes text. So every word on screen is a word that was transcribed,
//! with its own timing, and playback highlighting works in Clean exactly as in
//! Verbatim. The model can't invent content or switch language, and the
//! verbatim timeline (canonical, ADR-0004) is never touched.
//!
//! The answer is stored per take in `clean.json` beside `timeline.jsonl`: for
//! each timeline entry, keyed by a fingerprint of its words, the indices of
//! the words to hide. A renamed speaker keeps its entries' masks; an entry
//! whose words changed (a per-turn edit, a re-diarize that moved a boundary)
//! no longer matches and shows verbatim until the next pass cleans it.
//! `clean.json` is local to this Mac: it is derived, and a teammate's Humla
//! cleans its own copy.
//!
//! Runs after the post-stop chain (and after a deferred transcription), and
//! on demand when the reader asks for Clean on a take that has none. Uses the
//! note's summary provider through `resolve_provider`, so a governed note is
//! refused here the same way its summary is. Failure on the automatic path is
//! silent, like titles: nobody pressed anything.

use super::summary::{attach_copilot, resolve_provider, summary_provider_id};
use super::TITLE_FALLBACK_MODEL;
use crate::providers::ProviderId;
use crate::{db, openai, sessions, AppState};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter, Manager, State};

/// The per-take file holding the masks.
pub(crate) const CLEAN_FILE: &str = "clean.json";

/// Bumped when the meaning of a stored mask changes, so an older file is
/// ignored and the take cleaned again.
const CLEAN_VERSION: u32 = 1;

/// Words per model call. Big enough that a turn and its neighbours read as a
/// conversation, small enough that a small local model keeps every index
/// straight (long inputs make models drift and over-delete).
const WINDOW_WORDS: usize = 160;

/// A turn this long or longer that would lose more than `MAX_HIDDEN_SHARE`
/// of its words keeps them all: past that point the model is rewriting, not
/// tidying.
const SHARE_GUARD_MIN_WORDS: usize = 8;
const MAX_HIDDEN_SHARE: f32 = 0.4;

/// An entry may be hidden whole only when it is this short: a lone "Mm." or
/// "Eh, ja." between two turns. A longer one emptied out is a model error.
const WHOLE_ENTRY_MAX_WORDS: usize = 3;

/// Words a mask may never hide, whatever the model says: negations, in the
/// languages most Humla notes are in. A dropped "ikke" inverts a sentence,
/// and that is the one failure a clean view must not have. The prompt carries
/// the rule for every other language.
const NEVER_HIDE: &[&str] = &[
    "not", "no", "never", "nothing", "nobody", "none", "dont", "doesnt", "didnt", "cant", "wont",
    "isnt", "arent", "wasnt", "ikke", "ikkje", "inte", "ingen", "inga", "aldri", "aldrig", "nei",
    "nej", "nicht", "kein", "keine", "nie", "nein", "pas", "jamais", "rien", "non", "ne", "nunca",
    "nada", "nadie", "nessuno", "mai", "niet", "geen", "nooit", "nie",
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

#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
struct CleanFile {
    v: u32,
    /// What cleaned it, for diagnostics: `<provider>:<model>`.
    #[serde(default)]
    by: String,
    /// Entry fingerprint → indices of its words to hide (empty when the
    /// entry was read and nothing needed hiding).
    #[serde(default)]
    entries: BTreeMap<String, Vec<u32>>,
}

/// The words of one timeline entry as the reader shows them: its timed words,
/// or its text's whitespace tokens when the provider gave no timings. Mask
/// indices point into this list.
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

/// The masks stored for one take, by entry fingerprint. Empty when the take
/// was never cleaned or its file is from another version.
pub(crate) fn read_masks(take_dir: &Path) -> BTreeMap<String, Vec<u32>> {
    std::fs::read_to_string(take_dir.join(CLEAN_FILE))
        .ok()
        .and_then(|s| serde_json::from_str::<CleanFile>(&s).ok())
        .filter(|f| f.v == CLEAN_VERSION)
        .map(|f| f.entries)
        .unwrap_or_default()
}

/// One timeline entry as the clean pass sees it.
#[derive(Debug, Clone)]
struct Entry {
    label: String,
    tokens: Vec<String>,
    fp: String,
}

/// A run of consecutive entries with one label: a turn, as the reader groups
/// it.
#[derive(Debug, Clone)]
struct Turn {
    label: String,
    entries: Vec<usize>,
}

fn turns_of(entries: &[Entry]) -> Vec<Turn> {
    let mut out: Vec<Turn> = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        match out.last_mut() {
            Some(t) if t.label == e.label => t.entries.push(i),
            _ => out.push(Turn { label: e.label.clone(), entries: vec![i] }),
        }
    }
    out
}

/// One model call's worth of entries, in order, and the words it numbers.
#[derive(Debug, Clone, PartialEq)]
struct Window {
    /// Entry indices, in order.
    entries: Vec<usize>,
}

/// Cut the entries that need cleaning into windows of about `WINDOW_WORDS`,
/// breaking between turns where possible and between entries when a turn
/// alone is longer. Entries that don't need cleaning are left out, so a take
/// that lost a few masks to an edit costs one small call, not a whole pass.
fn windows(entries: &[Entry], needs: &dyn Fn(&Entry) -> bool) -> Vec<Window> {
    let mut out: Vec<Window> = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    let mut cur_words = 0usize;
    for turn in turns_of(entries) {
        let wanted: Vec<usize> = turn.entries.iter().copied().filter(|&i| needs(&entries[i])).collect();
        if wanted.is_empty() {
            continue;
        }
        let turn_words: usize = wanted.iter().map(|&i| entries[i].tokens.len()).sum();
        if !cur.is_empty() && cur_words + turn_words > WINDOW_WORDS {
            out.push(Window { entries: std::mem::take(&mut cur) });
            cur_words = 0;
        }
        for i in wanted {
            let n = entries[i].tokens.len();
            if !cur.is_empty() && cur_words + n > WINDOW_WORDS {
                out.push(Window { entries: std::mem::take(&mut cur) });
                cur_words = 0;
            }
            cur.push(i);
            cur_words += n;
        }
    }
    if !cur.is_empty() {
        out.push(Window { entries: cur });
    }
    out
}

/// The window as the model reads it: one line per turn, every word numbered
/// from 0 across the window. Returns the text and, per window word number,
/// which entry and word it is.
fn window_message(entries: &[Entry], w: &Window) -> (String, Vec<(usize, usize)>) {
    let mut text = String::new();
    let mut map: Vec<(usize, usize)> = Vec::new();
    let mut last_label: Option<&str> = None;
    for &ei in &w.entries {
        let e = &entries[ei];
        if last_label != Some(e.label.as_str()) {
            if !text.is_empty() {
                text.push('\n');
            }
            let name = if e.label.is_empty() { "Speaker" } else { e.label.as_str() };
            text.push_str(name);
            text.push(':');
            last_label = Some(e.label.as_str());
        }
        for (wi, tok) in e.tokens.iter().enumerate() {
            text.push_str(&format!(" [{}]{}", map.len(), tok));
            map.push((ei, wi));
        }
    }
    (text, map)
}

const CLEAN_PROMPT: &str = "You tidy meeting transcripts by choosing words to hide. You never rewrite, add or reorder anything.

The user message is part of a transcript, one line per speaker turn. Every word has its number in square brackets in front of it, e.g. [12]word.

Hide only:
- hesitation sounds and filler noises, in whatever language is spoken (um, uh, er, eh, ehm, øh, äh, euh, hmm, mm…);
- filler words that carry no meaning where they stand (like, you know, I mean, liksom, altså, på en måte, sånn, typ, halt, quasi, genre, o sea, tipo…), but only where removing them leaves the sentence's meaning unchanged;
- a word or phrase repeated by accident (\"I I think\", \"jeg har jeg har\": hide the first copy);
- a false start the speaker abandoned and then said properly (hide the abandoned words).

Never hide a negation (not, no, never, ikke, ingen, aldri, nicht, pas…), a number, a name, a question word, or any word that carries meaning. Keep repetition said on purpose (\"nei nei\", \"very very good\"). Keep short replies such as \"yes\", \"ja\", \"okay\" when they answer someone. When unsure, keep the word.

Answer with JSON only, no explanation: {\"hide\": [numbers]}. If nothing should be hidden, answer {\"hide\": []}.";

/// The word numbers a model's answer asks to hide, or `None` when the answer
/// isn't one: a JSON object with a `hide` array, possibly wrapped in prose or
/// a code fence, or a bare array of numbers. Numbers may come as strings.
fn parse_hide(raw: &str) -> Option<Vec<usize>> {
    let nums = |arr: &Vec<serde_json::Value>| -> Vec<usize> {
        arr.iter()
            .filter_map(|v| {
                v.as_u64()
                    .map(|n| n as usize)
                    .or_else(|| v.as_str().and_then(|s| s.trim().parse::<usize>().ok()))
            })
            .collect()
    };
    if let (Some(a), Some(b)) = (raw.find('{'), raw.rfind('}')) {
        if a < b {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw[a..=b]) {
                if let Some(arr) = v.get("hide").and_then(|h| h.as_array()) {
                    return Some(nums(arr));
                }
            }
        }
    }
    if let (Some(a), Some(b)) = (raw.find('['), raw.rfind(']')) {
        if a < b {
            if let Ok(serde_json::Value::Array(arr)) = serde_json::from_str::<serde_json::Value>(&raw[a..=b]) {
                return Some(nums(&arr));
            }
        }
    }
    None
}

fn protected(token: &str) -> bool {
    if token.chars().any(|c| c.is_ascii_digit()) {
        return true;
    }
    let folded: String = token
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    NEVER_HIDE.contains(&folded.as_str())
}

/// The model's word numbers turned into per-entry masks for the window's
/// entries, after the guards: out-of-range numbers dropped, protected words
/// kept, an entry hidden whole only when it's tiny, and a long turn that
/// would lose too much kept whole. Every entry in the window gets a mask
/// (possibly empty): it has been read.
fn masks_for_window(entries: &[Entry], w: &Window, map: &[(usize, usize)], hide: &[usize]) -> BTreeMap<usize, Vec<u32>> {
    let mut per_entry: BTreeMap<usize, Vec<u32>> = w.entries.iter().map(|&ei| (ei, Vec::new())).collect();
    let mut seen = HashSet::new();
    for &n in hide {
        let Some(&(ei, wi)) = map.get(n) else { continue };
        if !seen.insert(n) || protected(&entries[ei].tokens[wi]) {
            continue;
        }
        per_entry.entry(ei).or_default().push(wi as u32);
    }
    for (ei, mask) in per_entry.iter_mut() {
        mask.sort_unstable();
        let len = entries[*ei].tokens.len();
        if mask.len() >= len && len > WHOLE_ENTRY_MAX_WORDS {
            mask.clear();
        }
    }
    // The share guard runs per turn, over the turn's entries in this window.
    let in_window: HashSet<usize> = w.entries.iter().copied().collect();
    for turn in turns_of(entries) {
        let members: Vec<usize> = turn.entries.iter().copied().filter(|i| in_window.contains(i)).collect();
        let total: usize = members.iter().map(|&i| entries[i].tokens.len()).sum();
        let hidden: usize = members.iter().map(|i| per_entry.get(i).map_or(0, Vec::len)).sum();
        if total >= SHARE_GUARD_MIN_WORDS && hidden as f32 / total as f32 > MAX_HIDDEN_SHARE {
            for i in members {
                if let Some(m) = per_entry.get_mut(&i) {
                    m.clear();
                }
            }
        }
    }
    per_entry
}

/// Lifecycle of a clean pass, on its own channel so cleaning note B never
/// touches note A's view: `active` brackets the model calls, `done`/`total`
/// count windows.
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
    /// Timeline entries read by the model in this pass.
    pub entries: usize,
    /// Words hidden across them.
    pub hidden_words: usize,
}

/// One take's entries and the masks it already has.
struct TakeWork {
    dir: PathBuf,
    entries: Vec<Entry>,
    existing: BTreeMap<String, Vec<u32>>,
}

fn take_work(dir: &Path) -> TakeWork {
    let values = super::timeline::read_timeline_values(&dir.join("timeline.jsonl"));
    let entries = values
        .iter()
        .map(|v| {
            let tokens = entry_tokens(v);
            Entry {
                label: v.get("label").and_then(|s| s.as_str()).unwrap_or("").to_string(),
                fp: fingerprint(&tokens),
                tokens,
            }
        })
        .collect();
    TakeWork { dir: dir.to_path_buf(), entries, existing: read_masks(dir) }
}

fn write_masks(dir: &Path, by: &str, entries: BTreeMap<String, Vec<u32>>) -> std::io::Result<()> {
    let file = CleanFile { v: CLEAN_VERSION, by: by.to_string(), entries };
    let json = serde_json::to_string(&file).map_err(std::io::Error::other)?;
    let tmp = dir.join(format!("{CLEAN_FILE}.tmp"));
    std::fs::write(&tmp, json)?;
    std::fs::rename(tmp, dir.join(CLEAN_FILE))
}

/// Clean every take of `note_id` that has entries without a mask (every
/// entry when `force`). Returns what was done; errors name why nothing (or
/// not everything) was.
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
    let needs = |t: &TakeWork, e: &Entry| -> bool {
        !e.tokens.is_empty() && (force || !t.existing.contains_key(&e.fp))
    };
    let plan: Vec<Vec<Window>> = takes.iter().map(|t| windows(&t.entries, &|e| needs(t, e))).collect();
    let total: usize = plan.iter().map(Vec::len).sum();
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
    // Deciding which word is filler is classification, not reasoning: thinking
    // makes models over-delete and slow down a pass with many calls.
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
    'takes: for (take, wins) in takes.iter().zip(plan) {
        if wins.is_empty() {
            continue;
        }
        // Keep the masks still matching an entry; drop the ones for words
        // that are gone, so the file doesn't grow with every edit.
        let live: HashSet<&str> = take.entries.iter().map(|e| e.fp.as_str()).collect();
        let mut masks: BTreeMap<String, Vec<u32>> = take
            .existing
            .iter()
            .filter(|(fp, _)| live.contains(fp.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for w in &wins {
            let (message, map) = window_message(&take.entries, w);
            let answer = match provider.complete(CLEAN_PROMPT, &message, |_| {}).await {
                Ok(a) => a,
                Err(e) => {
                    failure = Some(e);
                    // Keep what this take got so far; the rest is cleaned on
                    // the next pass.
                    let _ = write_masks(&take.dir, &by, masks);
                    break 'takes;
                }
            };
            // An answer that isn't one (prose, a refusal) marks the window as
            // read with nothing hidden: asking again would cost the same and
            // most likely get the same.
            let hide = parse_hide(&answer).unwrap_or_else(|| {
                eprintln!("[clean] note {note_id}: unusable answer, keeping a window verbatim");
                Vec::new()
            });
            for (ei, mask) in masks_for_window(&take.entries, w, &map, &hide) {
                outcome.entries += 1;
                outcome.hidden_words += mask.len();
                masks.insert(take.entries[ei].fp.clone(), mask);
            }
            done += 1;
            emit_status(app, note_id, true, done, total, None);
        }
        write_masks(&take.dir, &by, masks)?;
    }
    // A note trashed and purged while the model was working must not keep the
    // file this pass just wrote.
    super::notes::purge_note_assets_if_gone(app, note_id);
    if let Some(e) = failure {
        return Err(e);
    }
    eprintln!(
        "[clean] note {note_id}: read {} entries, hid {} words ({by})",
        outcome.entries, outcome.hidden_words
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

/// Clean a note from the reader: on asking for Clean on a take that has no
/// masks, or on "Clean again" (`force`, every entry re-read).
#[tauri::command]
pub async fn note_clean(app: AppHandle, note_id: String, force: bool) -> Result<CleanOutcome, String> {
    clean_note_guarded(&app, &note_id, force).await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(label: &str, text: &str) -> Entry {
        let tokens: Vec<String> = text.split_whitespace().map(str::to_string).collect();
        Entry { label: label.into(), fp: fingerprint(&tokens), tokens }
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
        let a: Vec<String> = vec!["Ja,".into(), "ehm,".into(), "jeg".into()];
        // Pinned: the value is stored on disk, so it must not change between
        // builds.
        assert_eq!(fingerprint(&a), fingerprint(&a.clone()));
        assert_eq!(fingerprint(&[]), "cbf29ce484222325");
        let b: Vec<String> = vec!["Ja,".into(), "jeg".into()];
        assert_ne!(fingerprint(&a), fingerprint(&b));
        // Token boundaries count: "ab c" ≠ "a bc".
        assert_ne!(
            fingerprint(&["ab".to_string(), "c".to_string()]),
            fingerprint(&["a".to_string(), "bc".to_string()])
        );
    }

    #[test]
    fn windows_break_between_turns_and_skip_entries_already_cleaned() {
        let long = vec!["ord"; 100].join(" ");
        let entries = vec![
            entry("Michael", &long),
            entry("Hege", &long),
            entry("Hege", "kort svar"),
            entry("Michael", "ferdig her"),
        ];
        let all = windows(&entries, &|_| true);
        assert_eq!(all, vec![Window { entries: vec![0] }, Window { entries: vec![1, 2, 3] }]);
        let only_last = windows(&entries, &|e| e.tokens.len() < 10);
        assert_eq!(only_last, vec![Window { entries: vec![2, 3] }]);
    }

    #[test]
    fn a_turn_longer_than_a_window_splits_between_its_entries() {
        let chunk = vec!["ord"; 90].join(" ");
        let entries = vec![entry("Hege", &chunk), entry("Hege", &chunk), entry("Hege", &chunk)];
        let w = windows(&entries, &|_| true);
        assert_eq!(w.len(), 3);
    }

    #[test]
    fn the_window_numbers_every_word_once_across_turns() {
        let entries = vec![entry("Michael", "Eh, skal vi"), entry("Hege", "Ja, ehm, klart.")];
        let (text, map) = window_message(&entries, &Window { entries: vec![0, 1] });
        assert_eq!(text, "Michael: [0]Eh, [1]skal [2]vi\nHege: [3]Ja, [4]ehm, [5]klart.");
        assert_eq!(map[4], (1, 1));
    }

    #[test]
    fn parse_hide_reads_objects_fences_bare_arrays_and_strings() {
        assert_eq!(parse_hide(r#"{"hide": [0, 4]}"#), Some(vec![0, 4]));
        assert_eq!(parse_hide("```json\n{\"hide\": [\"2\", 3]}\n```"), Some(vec![2, 3]));
        assert_eq!(parse_hide("Here: [1, 2]"), Some(vec![1, 2]));
        assert_eq!(parse_hide(r#"{"hide": []}"#), Some(vec![]));
        assert_eq!(parse_hide("I would hide the fillers."), None);
    }

    #[test]
    fn masks_drop_out_of_range_and_protected_words() {
        let entries = vec![entry("Hege", "Ehm, det er ikke 4,2 prosent, altså.")];
        let w = Window { entries: vec![0] };
        let (_, map) = window_message(&entries, &w);
        // 0 Ehm, 1 det 2 er 3 ikke 4 4,2 5 prosent, 6 altså.
        let masks = masks_for_window(&entries, &w, &map, &[0, 3, 4, 6, 99, 0]);
        assert_eq!(masks[&0], vec![0, 6]);
    }

    #[test]
    fn a_tiny_entry_may_be_hidden_whole_but_a_long_one_may_not() {
        let entries = vec![entry("Hege", "Mm."), entry("Michael", "dette er en hel setning")];
        let w = Window { entries: vec![0, 1] };
        let (_, map) = window_message(&entries, &w);
        let masks = masks_for_window(&entries, &w, &map, &[0, 1, 2, 3, 4, 5]);
        assert_eq!(masks[&0], vec![0]);
        assert!(masks[&1].is_empty());
    }

    #[test]
    fn a_long_turn_that_would_lose_too_much_keeps_every_word() {
        let entries = vec![entry("Hege", "en to tre fire fem seks sju åtte ni ti")];
        let w = Window { entries: vec![0] };
        let (_, map) = window_message(&entries, &w);
        let masks = masks_for_window(&entries, &w, &map, &[0, 1, 2, 3, 4]);
        assert!(masks[&0].is_empty(), "{:?}", masks[&0]);
        let masks = masks_for_window(&entries, &w, &map, &[0, 1]);
        assert_eq!(masks[&0], vec![0, 1]);
    }

    #[test]
    fn every_entry_in_a_window_is_marked_read_even_with_nothing_hidden() {
        let entries = vec![entry("Hege", "Helt greit."), entry("Michael", "Bra.")];
        let w = Window { entries: vec![0, 1] };
        let (_, map) = window_message(&entries, &w);
        let masks = masks_for_window(&entries, &w, &map, &[]);
        assert_eq!(masks.len(), 2);
        assert!(masks.values().all(Vec::is_empty));
    }

    #[test]
    fn masks_round_trip_through_the_file_and_ignore_other_versions() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = BTreeMap::new();
        m.insert("abc".to_string(), vec![1, 3]);
        write_masks(dir.path(), "local:gemma", m.clone()).unwrap();
        assert_eq!(read_masks(dir.path()), m);
        std::fs::write(dir.path().join(CLEAN_FILE), r#"{"v":99,"entries":{"abc":[1]}}"#).unwrap();
        assert!(read_masks(dir.path()).is_empty());
    }
}
