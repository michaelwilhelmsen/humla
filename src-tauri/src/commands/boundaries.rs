//! Speaker changes a model checks. Diarization sometimes changes speaker a few
//! words off, inside a sentence: the end of one person's sentence goes to the
//! next speaker ("…Keep recorded og" | "Audio."), or the start of the next
//! person's to the previous one ("…fordi de er … Burde" | "Apply stå oppe").
//! `refine_pieces` catches what letter case can tell; this pass shows a model
//! every change that cuts a sentence (the earlier side ends without a
//! terminator or a pause) and moves the words it names, either way, in the
//! take's `timeline.jsonl`. The transcript is then
//! rebuilt from the timeline, so the reader, summaries, chat and export agree.
//!
//! Runs before the clean pass: after a recording, a deferred transcription and
//! a re-diarize (which rebuilds the timeline and drops earlier moves), and on
//! "Clean again". A change once judged is recorded by the words around it in
//! the take's `boundaries.json` and not asked about again.

use super::clean::{fingerprint, light_provider, CleanRequest};
use super::timeline::{commit_rebuilt_transcript, read_timeline_values, rebuild_note_transcript, write_timeline_values};
use super::transcript::piece_ends_terminator;
use crate::sessions;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use tauri::{AppHandle, Manager};

const BOUNDARY_FILE: &str = "boundaries.json";
const BOUNDARY_VERSION: u32 = 1;

/// Words of each side a change is shown with.
const CONTEXT_WORDS: usize = 30;

/// Most words one change may move, and how far from the change the farthest
/// of them may have been said. The prompt states the same limit.
const MAX_MOVE_WORDS: usize = 8;
const MAX_MOVE_MS: u64 = 6_000;

const CHANGES_PER_CALL: usize = 40;

/// Words either side of a change that name it in `boundaries.json`.
const KEY_WORDS: usize = 8;

const BOUNDARY_PROMPT: &str = "You check where a meeting transcript changes speaker. A speech recogniser assigned the words to speakers by voice, and sometimes the change lands a few words off, in the middle of a sentence: the end of one person's sentence is given to the next speaker, or the start of the next person's sentence to the previous one.

The user message is a JSON list of speaker changes: {\"id\": 0, \"before_speaker\": \"…\", \"before\": \"…\", \"after_speaker\": \"…\", \"after\": \"…\"}. \"before\" is the end of what one speaker said, \"after\" the start of what the next said.

For each change, find the sentence it cuts through: the words of \"before\" after its last sentence end, and the words of \"after\" up to its first sentence end. One person said that sentence. Decide who, from what each speaker says around it; when the text doesn't tell, it is the speaker who holds more of the sentence. Then answer how to move it there:
- a positive number moves that many words from the start of \"after\" to the end of \"before\";
- a negative number moves that many words from the end of \"before\" to the start of \"after\";
- 0 when the change already falls between two sentences.
Count words as they are separated by spaces, with their punctuation. Never move more than 8.

Examples:
before \"…so we ship it on\", after \"Friday. Good, I'll tell them.\" → 1
before \"…that works for me. So\", after \"what do we do next week? Let me check.\" → -1
before \"…and the price is\", after \"2,000 a month, right? Yes.\" → 4

Answer with JSON only, no explanation: {\"changes\": [{\"id\": 0, \"move\": 0}, …]}, one entry for every id you were given.";

/// A speaker change that cuts a sentence, in one take's timeline.
#[derive(Debug, Clone, PartialEq)]
struct Change {
    /// The last entry with words before the change, and the first after it.
    before: usize,
    after: usize,
    before_label: String,
    after_label: String,
    /// Up to `CONTEXT_WORDS` words either side, in reading order.
    before_words: Vec<String>,
    after_words: Vec<String>,
}

impl Change {
    /// The change named by the words around it, so a timeline rebuilt with
    /// the same words and boundary (or speakers renamed) keeps the judgement.
    fn key(&self) -> String {
        let tail = &self.before_words[self.before_words.len().saturating_sub(KEY_WORDS)..];
        let head = &self.after_words[..self.after_words.len().min(KEY_WORDS)];
        let mut tokens: Vec<String> = tail.to_vec();
        tokens.push("|".to_string());
        tokens.extend(head.iter().cloned());
        fingerprint(&tokens)
    }
}

fn label(v: &Value) -> &str {
    v.get("label").and_then(Value::as_str).unwrap_or("")
}

fn source(v: &Value) -> &str {
    v.get("source").and_then(Value::as_str).unwrap_or("")
}

fn words(v: &Value) -> &[Value] {
    v.get("words").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[])
}

fn word_text(w: &Value) -> &str {
    w.get("text").and_then(Value::as_str).unwrap_or("")
}

fn word_ms(w: &Value, key: &str) -> Option<u64> {
    w.get(key).and_then(Value::as_u64)
}

/// Replace an entry's words, and its text and bounds with them.
fn set_words(entry: &mut Value, ws: Vec<Value>) {
    let text = ws.iter().map(word_text).collect::<Vec<_>>().join(" ");
    if let (Some(start), Some(end)) = (ws.first().and_then(|w| word_ms(w, "start_ms")), ws.last().and_then(|w| word_ms(w, "end_ms"))) {
        entry["start_ms"] = Value::from(start);
        entry["end_ms"] = Value::from(end);
    }
    entry["text"] = Value::String(text);
    entry["words"] = Value::Array(ws);
}

/// The entries of one speaker's run from `from`, walking away from the change
/// (forward or backward), in the same stream.
fn run(entries: &[Value], from: usize, forward: bool) -> Vec<usize> {
    let (who, stream) = (label(&entries[from]), source(&entries[from]));
    let same = |i: usize| label(&entries[i]) == who && source(&entries[i]) == stream;
    if forward {
        (from..entries.len()).take_while(|&i| same(i)).collect()
    } else {
        (0..=from).rev().take_while(|&i| same(i)).collect()
    }
}

/// Whether `text` ends in a pause mark: the speaker trailed off, and a change
/// of speaker there is usually the next person taking over.
fn ends_in_pause(text: &str) -> bool {
    let t = text.trim_end();
    t.ends_with("...") || t.ends_with('…')
}

/// Every speaker change in a take's timeline that cuts a sentence: the earlier
/// side ends without a terminator or a pause, between two timed entries of one
/// stream.
fn candidates(entries: &[Value]) -> Vec<Change> {
    let timed: Vec<usize> = (0..entries.len()).filter(|&i| !words(&entries[i]).is_empty()).collect();
    let mut out = Vec::new();
    for pair in timed.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let (ea, eb) = (&entries[a], &entries[b]);
        if label(ea).is_empty() || label(eb).is_empty() || label(ea) == label(eb) || source(ea) != source(eb) {
            continue;
        }
        let a_text = words(ea).iter().map(word_text).collect::<Vec<_>>().join(" ");
        if piece_ends_terminator(&a_text) || ends_in_pause(&a_text) {
            continue;
        }
        let mut before_words: Vec<String> = run(entries, a, false)
            .iter()
            .flat_map(|&i| words(&entries[i]).iter().rev().map(|w| word_text(w).to_string()))
            .take(CONTEXT_WORDS)
            .collect();
        before_words.reverse();
        let after_words: Vec<String> = run(entries, b, true)
            .iter()
            .flat_map(|&i| words(&entries[i]).iter().map(|w| word_text(w).to_string()))
            .take(CONTEXT_WORDS)
            .collect();
        out.push(Change {
            before: a,
            after: b,
            before_label: label(ea).to_string(),
            after_label: label(eb).to_string(),
            before_words,
            after_words,
        });
    }
    out
}

/// Move `mv` words across `change`: positive from the start of the later
/// speaker's run to the earlier one, negative the other way. An entry whose
/// words all move changes speaker whole; a partly moved one gives its words to
/// its neighbour on the other side. Returns whether anything moved; a move the
/// timeline no longer fits (labels changed since, too many words, too far from
/// the change) moves nothing.
fn apply_move(entries: &mut [Value], change: &Change, mv: i64) -> bool {
    let n = mv.unsigned_abs() as usize;
    if mv == 0 || n > MAX_MOVE_WORDS {
        return false;
    }
    if label(&entries[change.before]) != change.before_label || label(&entries[change.after]) != change.after_label {
        return false;
    }
    let backward = mv > 0;
    let (side, to_label) = if backward {
        (run(entries, change.after, true), change.before_label.clone())
    } else {
        (run(entries, change.before, false), change.after_label.clone())
    };
    let moving: Vec<&Value> = if backward {
        side.iter().flat_map(|&i| words(&entries[i]).iter()).take(n).collect()
    } else {
        side.iter().flat_map(|&i| words(&entries[i]).iter().rev()).take(n).collect()
    };
    if moving.len() < n {
        return false;
    }
    let edge = if backward {
        words(&entries[change.before]).last().and_then(|w| word_ms(w, "end_ms"))
    } else {
        words(&entries[change.after]).first().and_then(|w| word_ms(w, "start_ms"))
    };
    let farthest = moving.last().and_then(|w| word_ms(w, if backward { "end_ms" } else { "start_ms" }));
    if let (Some(edge), Some(far)) = (edge, farthest) {
        if far.abs_diff(edge) > MAX_MOVE_MS {
            return false;
        }
    }

    let mut left = n;
    let mut target = if backward { change.before } else { change.after };
    for &i in &side {
        if left == 0 {
            break;
        }
        let count = words(&entries[i]).len();
        if count <= left {
            entries[i]["label"] = Value::String(to_label.clone());
            left -= count;
            target = i;
            continue;
        }
        let mut ws = words(&entries[i]).to_vec();
        let moved: Vec<Value> = if backward { ws.drain(..left).collect() } else { ws.split_off(ws.len() - left) };
        set_words(&mut entries[i], ws);
        let mut tw = words(&entries[target]).to_vec();
        if backward {
            tw.extend(moved);
        } else {
            let mut joined = moved;
            joined.extend(tw);
            tw = joined;
        }
        set_words(&mut entries[target], tw);
        left = 0;
    }
    // Empty entries between the two sides follow the earlier one, so they
    // can't split its run into two turns.
    let earlier = label(&entries[change.before]).to_string();
    for i in change.before + 1..change.after {
        if words(&entries[i]).is_empty() {
            entries[i]["label"] = Value::String(earlier.clone());
        }
    }
    true
}

#[derive(Serialize)]
struct ChangeIn<'a> {
    id: usize,
    before_speaker: &'a str,
    before: String,
    after_speaker: &'a str,
    after: String,
}

fn window_message<'a>(changes: impl Iterator<Item = &'a Change>) -> String {
    let items: Vec<ChangeIn> = changes
        .enumerate()
        .map(|(id, c)| ChangeIn {
            id,
            before_speaker: &c.before_label,
            before: c.before_words.join(" "),
            after_speaker: &c.after_label,
            after: c.after_words.join(" "),
        })
        .collect();
    serde_json::to_string(&items).unwrap_or_default()
}

/// The moves a model's answer gives, by window-local id; `None` when the
/// answer isn't one. Accepts `{"changes": [...]}`, possibly wrapped in prose or
/// a code fence, or a bare list of `{id, move}`.
fn parse_changes(raw: &str) -> Option<HashMap<usize, i64>> {
    let as_int = |v: &Value| v.as_i64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()));
    let collect = |arr: &Vec<Value>| -> HashMap<usize, i64> {
        arr.iter()
            .filter_map(|v| {
                let id = usize::try_from(as_int(v.get("id")?)?).ok()?;
                Some((id, as_int(v.get("move")?)?))
            })
            .collect()
    };
    if let (Some(a), Some(b)) = (raw.find('{'), raw.rfind('}')) {
        if a < b {
            if let Ok(v) = serde_json::from_str::<Value>(&raw[a..=b]) {
                if let Some(arr) = v.get("changes").and_then(Value::as_array) {
                    return Some(collect(arr));
                }
            }
        }
    }
    if let (Some(a), Some(b)) = (raw.find('['), raw.rfind(']')) {
        if a < b {
            if let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(&raw[a..=b]) {
                return Some(collect(&arr));
            }
        }
    }
    None
}

#[derive(Serialize, Deserialize, Default)]
struct BoundaryFile {
    v: u32,
    reviewed: Vec<String>,
}

fn read_reviewed(take_dir: &Path) -> HashSet<String> {
    std::fs::read_to_string(take_dir.join(BOUNDARY_FILE))
        .ok()
        .and_then(|s| serde_json::from_str::<BoundaryFile>(&s).ok())
        .filter(|f| f.v == BOUNDARY_VERSION)
        .map(|f| f.reviewed.into_iter().collect())
        .unwrap_or_default()
}

fn write_reviewed(take_dir: &Path, reviewed: &HashSet<String>) -> std::io::Result<()> {
    let mut keys: Vec<String> = reviewed.iter().cloned().collect();
    keys.sort();
    let json = serde_json::to_string(&BoundaryFile { v: BOUNDARY_VERSION, reviewed: keys }).map_err(std::io::Error::other)?;
    let tmp = take_dir.join(format!("{BOUNDARY_FILE}.tmp"));
    std::fs::write(&tmp, json)?;
    std::fs::rename(tmp, take_dir.join(BOUNDARY_FILE))
}

struct Take {
    dir: std::path::PathBuf,
    /// The timeline as read before the model calls, to tell whether it changed
    /// while they ran.
    read: Vec<Value>,
    entries: Vec<Value>,
    reviewed: HashSet<String>,
    changes: Vec<Change>,
}

/// Check the speaker changes of every take of `note_id` that haven't been
/// (every one on `CleanRequest::All`), move the words the model names, and
/// rebuild the transcript. Returns how many changes moved.
pub(crate) async fn review_note(app: &AppHandle, note_id: &str, request: CleanRequest) -> anyhow::Result<usize> {
    let force = request == CleanRequest::All;
    let app_dir = app.path().app_data_dir()?;
    let recordings = sessions::recordings_dir(&app_dir, note_id);
    let mut takes: Vec<Take> = Vec::new();
    for (_, dir) in sessions::resolve_sessions(&recordings) {
        let path = dir.join("timeline.jsonl");
        if !path.exists() {
            continue;
        }
        let entries = read_timeline_values(&path);
        let reviewed = read_reviewed(&dir);
        let changes = candidates(&entries)
            .into_iter()
            .filter(|c| force || !reviewed.contains(&c.key()))
            .collect();
        takes.push(Take { dir, read: entries.clone(), entries, reviewed, changes });
    }
    let asked: Vec<(usize, usize)> = takes
        .iter()
        .enumerate()
        .flat_map(|(t, take)| (0..take.changes.len()).map(move |c| (t, c)))
        .collect();
    if asked.is_empty() {
        return Ok(0);
    }

    let provider = light_provider(app, note_id, request).await?;
    let mut answers: HashMap<(usize, usize), i64> = HashMap::new();
    for window in asked.chunks(CHANGES_PER_CALL) {
        let message = window_message(window.iter().map(|&(t, c)| &takes[t].changes[c]));
        let raw = provider.complete(BOUNDARY_PROMPT, &message, |_| {}).await?;
        let Some(moves) = parse_changes(&raw) else {
            eprintln!("[turns] note {note_id}: unusable answer, leaving {} speaker changes as they are", window.len());
            continue;
        };
        for (local, &key) in window.iter().enumerate() {
            if let Some(&mv) = moves.get(&local) {
                answers.insert(key, mv);
            }
        }
    }

    // A recording started on the note while the model worked owns its
    // transcript now.
    if super::org::note_busy(app, note_id) {
        anyhow::bail!("the note became busy while its speaker changes were checked");
    }
    let mut moved = 0usize;
    for (t, take) in takes.iter_mut().enumerate() {
        let path = take.dir.join("timeline.jsonl");
        // An edit made while the model worked wins; its changes are asked about next time.
        if read_timeline_values(&path) != take.read {
            eprintln!("[turns] note {note_id}: a timeline changed while its speaker changes were checked, leaving it");
            continue;
        }
        let before: HashSet<String> = candidates(&take.entries).iter().map(Change::key).collect();
        let mut changed = false;
        for (c, change) in take.changes.iter().enumerate() {
            let Some(&mv) = answers.get(&(t, c)) else { continue };
            take.reviewed.insert(change.key());
            if apply_move(&mut take.entries, change, mv) {
                eprintln!("[turns] note {note_id}: moved {} word(s) {} at a speaker change", mv.unsigned_abs(), if mv > 0 { "back" } else { "forward" });
                changed = true;
                moved += 1;
            }
        }
        if changed {
            write_timeline_values(&path, &take.entries).map_err(|e| anyhow::anyhow!(e))?;
            // A change a move created was judged with the one it replaced.
            take.reviewed.extend(candidates(&take.entries).iter().map(Change::key).filter(|k| !before.contains(k)));
        }
        write_reviewed(&take.dir, &take.reviewed)?;
    }
    if moved > 0 {
        let transcript = rebuild_note_transcript(app, note_id).map_err(|e| anyhow::anyhow!(e))?;
        commit_rebuilt_transcript(app, note_id, transcript).map_err(|e| anyhow::anyhow!(e))?;
        let (app, id) = (app.clone(), note_id.to_string());
        tauri::async_runtime::spawn(async move {
            if let Err(e) = super::cloud::upload_note_sessions(&app, &id, false).await {
                eprintln!("[turns] note {id}: timeline upload: {e}");
            }
        });
    }
    Ok(moved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// An entry whose words are spread 300 ms apart from `start_ms`.
    fn entry(label: &str, text: &str, start_ms: u64) -> Value {
        let ws: Vec<Value> = text
            .split_whitespace()
            .enumerate()
            .map(|(k, t)| json!({"text": t, "start_ms": start_ms + k as u64 * 300, "end_ms": start_ms + k as u64 * 300 + 250}))
            .collect();
        let end = ws.last().and_then(|w| w["end_ms"].as_u64()).unwrap_or(start_ms);
        json!({"start_ms": start_ms, "end_ms": end, "label": label, "text": text, "words": ws, "source": "mic"})
    }

    fn texts(entries: &[Value]) -> Vec<(String, String)> {
        entries.iter().map(|e| (label(e).to_string(), e["text"].as_str().unwrap().to_string())).collect()
    }

    #[test]
    fn a_change_after_an_unfinished_sentence_is_a_candidate() {
        let entries = vec![
            entry("Michael", "Så har du den der. Keep recorded og", 0),
            entry("Esben", "Audio.", 3_000),
            entry("Esben", "Den har du nok lurt på.", 3_500),
            entry("Michael", "Ja.", 6_000),
        ];
        let found = candidates(&entries);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].before, found[0].after), (0, 1));
        assert_eq!(found[0].after_words.join(" "), "Audio. Den har du nok lurt på.");
        assert_eq!(found[0].before_words.last().map(String::as_str), Some("og"));
    }

    #[test]
    fn finished_sentences_other_streams_and_unlabelled_entries_are_not() {
        let mut sys = entry("Hege", "fortsetter her.", 3_000);
        sys["source"] = json!("sys");
        let entries = vec![
            entry("Michael", "Dette er ferdig.", 0),
            entry("Esben", "Ja.", 1_000),
            entry("Esben", "Og så", 2_000),
            sys,
            entry("", "uten navn", 4_000),
        ];
        assert!(candidates(&entries).is_empty());
    }

    #[test]
    fn a_change_after_a_pause_is_left_alone() {
        let entries = vec![entry("Sally", "Sånn som ...", 0), entry("Esben", "Hvordan gjør de det?", 2_000)];
        assert!(candidates(&entries).is_empty());
        let entries = vec![entry("Sally", "fordi de er ... Burde", 0), entry("Esben", "Apply stå oppe da.", 2_000)];
        assert_eq!(candidates(&entries).len(), 1);
    }

    #[test]
    fn moving_back_a_whole_entry_changes_its_speaker() {
        let mut entries = vec![
            entry("Michael", "Keep recorded og", 0),
            entry("Esben", "Audio.", 1_000),
            entry("Esben", "Den har du nok lurt på.", 1_500),
        ];
        let change = candidates(&entries).remove(0);
        assert!(apply_move(&mut entries, &change, 1));
        assert_eq!(
            texts(&entries),
            vec![
                ("Michael".into(), "Keep recorded og".into()),
                ("Michael".into(), "Audio.".into()),
                ("Esben".into(), "Den har du nok lurt på.".into()),
            ]
        );
    }

    #[test]
    fn moving_part_of_an_entry_carries_its_words_and_times_across() {
        let mut entries = vec![entry("Sally", "fordi de er ... Burde", 0), entry("Esben", "Apply stå oppe da.", 2_000)];
        let change = candidates(&entries).remove(0);
        assert!(apply_move(&mut entries, &change, -1));
        assert_eq!(
            texts(&entries),
            vec![("Sally".into(), "fordi de er ...".into()), ("Esben".into(), "Burde Apply stå oppe da.".into())]
        );
        assert_eq!(entries[0]["end_ms"], 1_150);
        assert_eq!(entries[1]["start_ms"], 1_200);
        assert_eq!(entries[1]["words"][0]["text"], "Burde");
    }

    #[test]
    fn moving_back_into_a_longer_entry_splits_it() {
        let mut entries = vec![entry("Michael", "Her sto det", 0), entry("Sally", "company profile. Ja, nettopp.", 1_000)];
        let change = candidates(&entries).remove(0);
        assert!(apply_move(&mut entries, &change, 2));
        assert_eq!(
            texts(&entries),
            vec![("Michael".into(), "Her sto det company profile.".into()), ("Sally".into(), "Ja, nettopp.".into())]
        );
    }

    #[test]
    fn a_move_the_timeline_no_longer_fits_moves_nothing() {
        let entries = vec![entry("Michael", "Keep recorded og", 0), entry("Esben", "Audio. Ja.", 1_000)];
        let change = candidates(&entries).remove(0);
        // More words than the side has, more than the limit, nothing at all.
        for mv in [3, 9, -9, 0] {
            let mut e = entries.clone();
            assert!(!apply_move(&mut e, &change, mv), "move {mv}");
            assert_eq!(e, entries);
        }
        // Speakers renamed since the change was read.
        let mut renamed = entries.clone();
        renamed[1]["label"] = json!("Hege");
        assert!(!apply_move(&mut renamed, &change, 1));
        // Words said long after the change.
        let mut far = vec![entry("Michael", "Keep recorded og", 0), entry("Esben", "a b c d e f g h", 1_000)];
        for (k, w) in far[1]["words"].as_array_mut().unwrap().iter_mut().enumerate() {
            w["start_ms"] = json!(1_000 + k as u64 * 1_500);
            w["end_ms"] = json!(1_000 + k as u64 * 1_500 + 200);
        }
        let change = candidates(&far).remove(0);
        assert!(!apply_move(&mut far, &change, 6));
    }

    #[test]
    fn an_empty_entry_between_the_sides_follows_the_earlier_speaker() {
        let mut entries = vec![
            entry("Michael", "Keep recorded og", 0),
            json!({"start_ms": 900, "end_ms": 950, "label": "Esben", "text": "", "words": [], "source": "mic"}),
            entry("Esben", "Audio.", 1_000),
        ];
        let change = candidates(&entries).remove(0);
        assert!(apply_move(&mut entries, &change, 1));
        assert_eq!(label(&entries[1]), "Michael");
        assert_eq!(label(&entries[2]), "Michael");
    }

    #[test]
    fn the_key_survives_renamed_speakers_but_not_moved_words() {
        let entries = vec![entry("Michael", "Keep recorded og", 0), entry("Esben", "Audio. Ja.", 1_000)];
        let change = candidates(&entries).remove(0);
        let mut renamed = entries.clone();
        renamed[0]["label"] = json!("Speaker 1");
        assert_eq!(candidates(&renamed)[0].key(), change.key());
        let mut moved = entries.clone();
        assert!(apply_move(&mut moved, &change, 1));
        assert!(candidates(&moved).iter().all(|c| c.key() != change.key()));
    }

    #[test]
    fn the_prompt_states_the_move_limit() {
        assert!(BOUNDARY_PROMPT.contains(&format!("Never move more than {MAX_MOVE_WORDS}.")));
    }

    #[test]
    fn the_window_message_lists_changes_with_local_ids() {
        let entries = vec![entry("Michael", "Keep recorded og", 0), entry("Esben", "Audio.", 1_000)];
        let found = candidates(&entries);
        let msg: Value = serde_json::from_str(&window_message(found.iter())).unwrap();
        assert_eq!(
            msg,
            json!([{"id": 0, "before_speaker": "Michael", "before": "Keep recorded og", "after_speaker": "Esben", "after": "Audio."}])
        );
    }

    #[test]
    fn parse_changes_reads_objects_fences_bare_lists_and_string_numbers() {
        let want = HashMap::from([(0, 1), (1, -2)]);
        assert_eq!(parse_changes(r#"{"changes": [{"id": 0, "move": 1}, {"id": 1, "move": -2}]}"#), Some(want.clone()));
        assert_eq!(parse_changes("```json\n{\"changes\": [{\"id\": \"0\", \"move\": \"1\"}, {\"id\": 1, \"move\": -2}]}\n```"), Some(want.clone()));
        assert_eq!(parse_changes(r#"[{"id": 0, "move": 1}, {"id": 1, "move": -2}]"#), Some(want));
        assert_eq!(parse_changes("I can't tell."), None);
    }

    #[test]
    fn the_reviewed_file_round_trips_and_ignores_other_versions() {
        let dir = tempfile::tempdir().unwrap();
        let keys: HashSet<String> = ["a".to_string(), "b".to_string()].into();
        write_reviewed(dir.path(), &keys).unwrap();
        assert_eq!(read_reviewed(dir.path()), keys);
        std::fs::write(dir.path().join(BOUNDARY_FILE), r#"{"v": 99, "reviewed": ["a"]}"#).unwrap();
        assert!(read_reviewed(dir.path()).is_empty());
    }
}
