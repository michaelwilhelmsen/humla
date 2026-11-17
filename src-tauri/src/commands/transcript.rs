//! Assembling the labelled transcript from transcribed chunks: per-speaker
//! pieces split on word timings, mic echo dropped against the system stream,
//! short interjections and continuation chains folded into the surrounding
//! speaker, and run-on sentences mended across line boundaries.

use super::diarization::assign_speaker;
use crate::diarize;
use crate::recording::{ChunkRecord, ChunkSource};

/// One contiguous run of words within a chunk that all belong to the
/// same speaker. A 15-second VAD chunk that opens with one voice and
/// ends with another becomes two pieces; a single-voice chunk stays
/// one piece.
///
/// `words` are kept chunk-relative (matching `ChunkWord`'s convention)
/// so the timeline serialiser can rebase them to stream-absolute the
/// same way it always has, and may be empty when the underlying chunk
/// had no word data and we fell back to whole-chunk labelling.
#[derive(Clone, Debug)]
pub(super) struct LabelledPiece {
    pub(super) label: Option<String>,
    pub(super) text: String,
    pub(super) words: Vec<crate::recording::ChunkWord>,
}

/// Wrap a chunk's full text + words as a single labelled piece. Used by
/// the paths that don't split (mic = "You" in hybrid mode, single-
/// speaker fallbacks when diarize returned nothing).
pub(super) fn single_piece(c: &ChunkRecord, label: Option<String>) -> Vec<LabelledPiece> {
    vec![LabelledPiece {
        label,
        text: c.text.clone(),
        words: c.words.clone(),
    }]
}

/// Split a chunk into per-speaker pieces by walking its word timings
/// against the diarizer's segments. Each word's stream-absolute
/// midpoint (chunk.start_ms + word's chunk-relative midpoint) is looked
/// up in `segments`; consecutive same-speaker words coalesce into one
/// piece, and the speaker label changes mid-chunk produce additional
/// pieces.
///
/// We use the word's *midpoint* rather than its `start_ms` so a word
/// straddling a segment boundary lands on whichever side it spends more
/// of its duration in — start-only would give the leading word of a
/// new turn the previous speaker's label.
///
/// Falls back to whole-chunk labelling (one piece, label decided by
/// `chunk.start_ms`) when the chunk has no words. That covers OpenAI
/// chunks (current API path doesn't expose word timestamps) and
/// re-diarize from older diagnostic JSONs that didn't persist words.
pub(super) fn split_by_segments(
    c: &ChunkRecord,
    segments: &[diarize::Segment],
    display_map: &std::collections::HashMap<String, u32>,
) -> Vec<LabelledPiece> {
    let labels: std::collections::HashMap<String, String> = display_map
        .iter()
        .map(|(sid, n)| (sid.clone(), format!("Speaker {n}")))
        .collect();
    split_by_labels(c, segments, &labels)
}

/// Same word-walking split as `split_by_segments`, but the label map holds
/// the *final* display string per speaker id rather than a number. That
/// lets the hybrid branch mix a channel-attributed label (`You`, when the
/// mic diarize found a single voice) with numbered ones on the same
/// recording, and lets the second stream's numbers continue past the
/// first's instead of restarting at 1.
pub(super) fn split_by_labels(
    c: &ChunkRecord,
    segments: &[diarize::Segment],
    labels: &std::collections::HashMap<String, String>,
) -> Vec<LabelledPiece> {
    let label_for_time = |abs_ms: u64| -> Option<String> {
        assign_speaker(abs_ms, segments)
            .and_then(|sid| labels.get(sid))
            .cloned()
    };
    if c.words.is_empty() {
        return single_piece(c, label_for_time(c.start_ms));
    }
    let mut pieces: Vec<LabelledPiece> = Vec::new();
    for word in &c.words {
        let mid = word.start_ms.saturating_add(word.end_ms.saturating_sub(word.start_ms) / 2);
        let abs = c.start_ms.saturating_add(mid);
        let label = label_for_time(abs);
        match pieces.last_mut() {
            Some(last) if last.label == label => {
                last.text.push(' ');
                last.text.push_str(&word.text);
                last.words.push(word.clone());
            }
            _ => {
                pieces.push(LabelledPiece {
                    label,
                    text: word.text.clone(),
                    words: vec![word.clone()],
                });
            }
        }
    }
    pieces
}

/// Cross-stream echo dedup: when a meeting plays through laptop speakers the
/// mic picks the remote voices up again, so this drops mic chunks whose tokens
/// are mostly contained in the sys chunks overlapping them in time. A no-op
/// without sys chunks.
pub(super) fn dedup_mic_against_sys(chunks: &[ChunkRecord]) -> Vec<ChunkRecord> {
    // How far apart the two streams can place the same sound — their start
    // offset plus the speaker-to-mic path, a few hundred ms — with room for a
    // chunk end estimated from its word count.
    const TOLERANCE_MS: u64 = 2_000;
    // Token-overlap threshold above which a mic chunk is considered an
    // echo of the sys text and dropped. Genuine simultaneous speech
    // (you talking while the remote speaks) typically shares <0.3 of
    // tokens because the words are different.
    const SIMILARITY_THRESHOLD: f32 = 0.6;
    // Skip dedup for mic chunks under this many tokens — brief acks
    // ("yeah", "ok") match by chance against any windowed sys text.
    const MIN_MIC_TOKENS: usize = 3;

    let sys_spans: Vec<(u64, u64, &str)> = chunks
        .iter()
        .filter(|c| c.source == ChunkSource::Sys)
        .map(|c| (c.start_ms, chunk_end_ms(c), c.text.as_str()))
        .collect();
    if sys_spans.is_empty() {
        return chunks.to_vec();
    }

    let mut kept: Vec<ChunkRecord> = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        if chunk.source != ChunkSource::Mic {
            kept.push(chunk.clone());
            continue;
        }
        let mic_tokens = normalize_tokens(&chunk.text);
        if mic_tokens.len() < MIN_MIC_TOKENS {
            kept.push(chunk.clone());
            continue;
        }
        let lower = chunk.start_ms.saturating_sub(TOLERANCE_MS);
        let upper = chunk_end_ms(chunk).saturating_add(TOLERANCE_MS);
        let mut sys_window = String::new();
        for &(start, end, text) in &sys_spans {
            if start <= upper && end >= lower {
                if !sys_window.is_empty() {
                    sys_window.push(' ');
                }
                sys_window.push_str(text);
            }
        }
        if sys_window.is_empty() {
            kept.push(chunk.clone());
            continue;
        }
        let sim = token_containment(&mic_tokens, &normalize_tokens(&sys_window));
        if sim < SIMILARITY_THRESHOLD {
            kept.push(chunk.clone());
        }
        // else: this mic chunk is an echo of the sys content — drop it.
    }
    kept
}

/// Stream-absolute end of a chunk's speech: its last word's end, or an
/// estimate from its word count when the provider returned no word timings.
/// Never later than the longest chunk the audio-capture sidecar cuts
/// (`maxSeconds` on its `ChunkWriter`s).
fn chunk_end_ms(c: &ChunkRecord) -> u64 {
    const MAX_CHUNK_MS: u64 = 15_000;
    let spoken_ms = match c.words.iter().map(|w| w.end_ms).max() {
        Some(end_ms) => end_ms,
        None => estimated_speech_ms(&c.text),
    };
    c.start_ms.saturating_add(spoken_ms.min(MAX_CHUNK_MS))
}

/// How long `text` takes to say at a typical conversational ~350 ms a word,
/// never under a second.
pub(super) fn estimated_speech_ms(text: &str) -> u64 {
    (text.split_whitespace().count() as u64)
        .saturating_mul(350)
        .max(1_000)
}

/// Lowercase, split on non-alphanumeric, drop tokens shorter than 2
/// chars. Matches the granularity Whisper emits — punctuation differs
/// across streams ("Hello." vs "hello") and one-letter tokens are too
/// noisy to count toward overlap.
pub(super) fn normalize_tokens(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 1)
        .map(|w| w.to_string())
        .collect()
}

/// Jaccard similarity: |A ∩ B| / |A ∪ B|. 1.0 when the sets are
/// equal, scaling down with each token unique to either side. Used
/// for cross-chunk dedup where both chunks come from the same source
/// at similar VAD lengths — penalising added unique tokens lets us
/// keep legitimate continuations (chunk N+1 = chunk N's content +
/// new sentence) while still catching exact / near-exact repeats
/// from a Whisper hallucination loop.
pub(super) fn token_jaccard(a: &[String], b: &[String]) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let set_a: std::collections::HashSet<&str> = a.iter().map(String::as_str).collect();
    let set_b: std::collections::HashSet<&str> = b.iter().map(String::as_str).collect();
    let inter = set_a.intersection(&set_b).count() as f32;
    let union = set_a.union(&set_b).count() as f32;
    if union == 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// Share of `a`'s distinct tokens that also appear in `b`: |A ∩ B| / |A|.
/// Directional — a long `b` doesn't dilute it, and a short `b` can't account
/// for a longer `a`.
fn token_containment(a: &[String], b: &[String]) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let set_a: std::collections::HashSet<&str> = a.iter().map(String::as_str).collect();
    let set_b: std::collections::HashSet<&str> = b.iter().map(String::as_str).collect();
    set_a.intersection(&set_b).count() as f32 / set_a.len() as f32
}

/// Maximum word count for a labelled piece to be considered a "short
/// interjection" eligible for absorption into the surrounding speaker.
/// Bumped from 3 → 6 after the segment-cleaning pass exposed a long
/// tail of clearly mis-attributed mid-sentence fragments at the
/// 4–6 word range ("Sjefen over alle sjefer", "Jeg tok det feil. Det er"
/// etc.) — all sandwich-shaped, all sub-3500ms, all obviously the
/// surrounding speaker's own words. Real ≥7 word replies, even when
/// sandwiched, still pass through.
const BRIDGE_MAX_WORDS: usize = 6;

/// Maximum acoustic duration for a piece to be considered an
/// interjection. Word-count alone can't separate "yeah totally" (a
/// 0.6s backchannel) from "let me check" (a 1.5s real reply); the
/// acoustic length is what makes them distinguishable. Bumped from
/// 1500 → 3500 ms together with `BRIDGE_MAX_WORDS` so a 4-second
/// "Den er litt sånn …" mid-sentence fragment can be absorbed when
/// it's clearly the surrounding speaker — without sweeping up true
/// short replies delivered slowly (those usually run past 3500 ms).
const BRIDGE_MAX_DURATION_MS: u64 = 3_500;

/// Collapse short cross-speaker interjections into the surrounding
/// speaker run. The diarizer (FluidAudio community-1 + VBx) sometimes
/// flips speaker mid-utterance for a brief backchannel — a 0.4 s
/// "yeah" inside a longer monologue gets its own segment, the
/// word-level split in `split_by_segments` honours that, and the user
/// ends up with `Speaker 1: ... Speaker 2: yeah Speaker 1: ...` cut
/// across three lines. Tuning the diarizer's `minDurationOn` upstream
/// reduces the noise floor; this pass cleans up what slips through.
///
/// Rule: a piece P is rewritten to its neighbours' label when
///   - P has a label,
///   - the previous labelled piece and the next labelled piece both
///     have the same label as each other, different from P's,
///   - P has word timestamps available (so we can measure duration),
///   - P's text has at most `BRIDGE_MAX_WORDS` words AND its acoustic
///     span is at most `BRIDGE_MAX_DURATION_MS`.
///
/// Pieces with no word timestamps are skipped: that's the OpenAI
/// transcribe path and older diagnostic JSONs, where we can't tell a
/// real short turn from a backchannel and the safer default is to
/// leave the speaker boundary in place.
///
/// Pieces with no label (the graceful-degrade path when diarize emits
/// nothing) are passed over when finding the "previous"/"next"
/// labelled neighbour, so they don't break the sandwich pattern.
pub(super) fn bridge_short_interjections(pieces: &mut [LabelledPiece]) {
    if pieces.len() < 3 {
        return;
    }
    for i in 1..pieces.len() - 1 {
        let cur_label = match &pieces[i].label {
            Some(l) => l.clone(),
            None => continue,
        };
        if pieces[i].text.split_whitespace().count() > BRIDGE_MAX_WORDS {
            continue;
        }
        let words = &pieces[i].words;
        if words.is_empty() {
            continue;
        }
        let span_ms = words
            .last()
            .map(|w| w.end_ms)
            .unwrap_or(0)
            .saturating_sub(words.first().map(|w| w.start_ms).unwrap_or(0));
        if span_ms > BRIDGE_MAX_DURATION_MS {
            continue;
        }
        let prev_label = pieces[..i]
            .iter()
            .rev()
            .find_map(|p| p.label.clone());
        let next_label = pieces[i + 1..]
            .iter()
            .find_map(|p| p.label.clone());
        let (Some(prev), Some(next)) = (prev_label, next_label) else {
            continue;
        };
        if prev != next || prev == cur_label {
            continue;
        }
        pieces[i].label = Some(prev);
    }
}

/// Maximum chain length to consider for text-continuation absorption.
/// Bounds the blast radius if the algorithm latches onto a long
/// legitimate cross-speaker exchange that happens to alternate without
/// clean punctuation breaks. Six pieces covers every mis-attribution
/// chain we've observed in dogfooded recordings.
const CONTINUATION_CHAIN_MAX_LEN: usize = 6;

/// Whether `text`'s last meaningful character is a sentence terminator
/// (`.!?:;`). Trailing ellipsis ("..." or Unicode "…") is stripped first
/// because Whisper uses them to mark trailing-off mid-utterance, not
/// sentence end — a distinction `ends_sentence` deliberately ignores
/// since its callers (the text-level merge passes) want to be
/// conservative about fusing across any pause cue.
fn piece_ends_terminator(text: &str) -> bool {
    let mut trimmed = text.trim_end();
    loop {
        let prev_len = trimmed.len();
        trimmed = trimmed.trim_end_matches('…').trim_end();
        if trimmed.ends_with("...") {
            trimmed = trimmed[..trimmed.len() - 3].trim_end();
        }
        if trimmed.len() == prev_len {
            break;
        }
    }
    let core = trimmed
        .trim_end_matches(['"', '\'', ')', ']', '»', '”', '’']);
    matches!(
        core.chars().last(),
        Some('.') | Some('!') | Some('?') | Some(':') | Some(';')
    )
}

/// Whether `text` opens with a continuation cue — lowercase letter or
/// leading ellipsis. Pairs with `piece_ends_terminator` to detect when
/// two consecutive pieces are one sentence even though the diarizer
/// gave them different labels.
fn piece_starts_continuation(text: &str) -> bool {
    let trimmed = text.trim_start();
    if trimmed.starts_with("...") || trimmed.starts_with('…') {
        return true;
    }
    trimmed
        .chars()
        .next()
        .map(|c| c.is_lowercase())
        .unwrap_or(false)
}

/// Collapse "text continuation chains" — consecutive pieces where each
/// pair signals "still one sentence" via text cues (previous ends
/// without a terminator AND current starts with a lowercase letter or
/// ellipsis). Within such a chain, if labels disagree, one wins and
/// the whole chain is relabelled to it.
///
/// Winner selection:
///   1. If the pieces immediately before and after the chain share a
///      label, AND that label appears somewhere in the chain, use it.
///      This catches the A-B-A-long-fragment pattern: surrounding
///      Speaker 1 context outvotes the mis-attributed middle even
///      when the middle has more words.
///   2. Otherwise the label with the highest total word count in the
///      chain wins. The longer text is the more reliably-diarized
///      side, so when context can't break the tie we trust word
///      density.
///
/// Complements `bridge_short_interjections`, which only fires on
/// strict A-B-A sandwiches with B short and brief. This rule catches:
///   - A-B-A with B long (multi-word mid-sentence fragments the
///     acoustic bridge rejects on word/duration limits)
///   - A-B-C with no clean acoustic sandwich but clear textual flow
///   - Longer chains where an end-to-end engine alternated labels piece by
///     piece through a single speaker's utterance
///
/// Trades a small risk of merging a legitimate cross-speaker
/// interruption (the interrupter happens to finish the original
/// speaker's sentence) for substantial reduction of single-speaker
/// fragmentation. Acceptable because clean interruptions usually
/// carry their own punctuation cues; we only act when the text says
/// "still one sentence."
pub(super) fn absorb_text_continuation_chains(pieces: &mut [LabelledPiece]) {
    if pieces.len() < 2 {
        return;
    }
    let mut i = 0;
    while i < pieces.len() {
        let mut end = i;
        while end + 1 < pieces.len()
            && end - i + 1 < CONTINUATION_CHAIN_MAX_LEN
            && !piece_ends_terminator(&pieces[end].text)
            && piece_starts_continuation(&pieces[end + 1].text)
        {
            end += 1;
        }
        if end > i {
            let pre_label = if i > 0 {
                pieces[i - 1].label.clone()
            } else {
                None
            };
            let post_label = if end + 1 < pieces.len() {
                pieces[end + 1].label.clone()
            } else {
                None
            };
            let mut word_count_by_label: std::collections::HashMap<String, usize> =
                std::collections::HashMap::new();
            for piece in &pieces[i..=end] {
                if let Some(label) = &piece.label {
                    let wc = piece.text.split_whitespace().count();
                    *word_count_by_label.entry(label.clone()).or_insert(0) += wc;
                }
            }
            if word_count_by_label.len() >= 2 {
                let chosen = pre_label
                    .as_ref()
                    .filter(|l| post_label.as_ref() == Some(l))
                    .filter(|l| word_count_by_label.contains_key(l.as_str()))
                    .cloned()
                    .or_else(|| {
                        word_count_by_label
                            .iter()
                            .max_by_key(|(_, &c)| c)
                            .map(|(k, _)| k.clone())
                    });
                if let Some(winner) = chosen {
                    for piece in &mut pieces[i..=end] {
                        if piece.label.is_some() {
                            piece.label = Some(winner.clone());
                        }
                    }
                }
            }
        }
        i = end + 1;
    }
}

/// Rebuild the transcript by walking chunks in chronological order and
/// emitting each one prefixed with its assigned label. Same-label runs
/// get a single space between chunks (continuation); label changes get
/// a newline + new prefix. Chunks the labeller declines to label
/// (returns `None`) get joined to whatever came before them with a
/// space, no prefix change — typically only happens when diarize
/// produces zero segments and we're degrading gracefully.
///
/// Chronological ordering uses `(start_ms, source)`. Mic and system
/// chunks each carry start_ms relative to their own stream's first
/// frame — close to but not exactly the same as global wall time
/// (the streams start within a few hundred ms of each other). The
/// tie-break preferring `Mic` reflects the typical UX assumption that
/// the user speaks first; in practice the imprecision is well below
/// the threshold a reader would notice.
///
/// Cross-stream echo dedup runs first via `dedup_mic_against_sys` —
/// see that function for details. Short cross-speaker interjections
/// are absorbed into the surrounding speaker via
/// `bridge_short_interjections` before emit.
/// Pure pipeline up to (but not including) `bridge_short_interjections`.
/// Extracted so the diagnostic dump can capture the same pre-bridge piece
/// sequence the transcript emitter walks, without duplicating the
/// dedup/sort/split logic.
pub(super) fn build_pieces_unbridged(
    chunks: &[ChunkRecord],
    split_chunk: &(dyn Fn(&ChunkRecord) -> Vec<LabelledPiece> + Send),
) -> Vec<LabelledPiece> {
    let kept = dedup_mic_against_sys(chunks);
    let mut sorted: Vec<&ChunkRecord> = kept.iter().collect();
    sorted.sort_by_key(|c| {
        let source_rank = match c.source {
            ChunkSource::Mic => 0,
            ChunkSource::Sys => 1,
        };
        (c.start_ms, source_rank)
    });

    let mut all_pieces: Vec<LabelledPiece> = Vec::new();
    for chunk in sorted {
        for piece in split_chunk(chunk) {
            if piece.text.trim().is_empty() {
                continue;
            }
            all_pieces.push(piece);
        }
    }
    all_pieces
}

pub(super) fn build_labelled_transcript(
    chunks: &[ChunkRecord],
    split_chunk: &(dyn Fn(&ChunkRecord) -> Vec<LabelledPiece> + Send),
) -> String {
    let mut all_pieces = build_pieces_unbridged(chunks, split_chunk);
    bridge_short_interjections(&mut all_pieces);
    absorb_text_continuation_chains(&mut all_pieces);

    let mut output = String::new();
    let mut last_label: Option<String> = None;

    for piece in all_pieces {
        let trimmed = piece.text.trim();
        if trimmed.is_empty() {
            continue;
        }
        match &piece.label {
            Some(label) => {
                if last_label.as_deref() != Some(label.as_str()) {
                    if !output.is_empty() {
                        output.push('\n');
                    }
                    output.push_str(&format!("{label}: "));
                    last_label = Some(label.clone());
                } else {
                    output.push(' ');
                }
            }
            None => {
                if !output.is_empty() {
                    output.push(' ');
                }
            }
        }
        output.push_str(trimmed);
    }
    merge_run_on_sentences(&output)
}

/// Strip a leading `<label>: ` from a transcript line, returning the
/// remainder. Mirrors the format `build_labelled_transcript` emits, so
/// the merge pass can drop the absorbed line's prefix when joining.
fn strip_label_prefix(line: &str) -> &str {
    let trimmed = line.trim_start();
    if let Some(colon) = trimmed.find(':') {
        let label = &trimmed[..colon];
        if !label.is_empty() && label.len() <= 40 && !label.contains('\n') {
            let rest = &trimmed[colon + 1..];
            return rest.trim_start();
        }
    }
    line
}

/// Whether `line` ends with a sentence-terminating punctuation mark,
/// allowing trailing close-quote/close-paren after the terminator
/// (`he said.` / `(yes!)` / `"done."`). Trailing whitespace is ignored.
///
/// Em-dash (`—`), en-dash (`–`), and a final ASCII hyphen are also
/// treated as terminators. They mark a speaker trailing off mid-thought
/// or being interrupted — fusing across them would join two distinct
/// turns into nonsense.
fn ends_sentence(line: &str) -> bool {
    let trimmed = line.trim_end();
    let cleaned: String = trimmed
        .chars()
        .rev()
        .take_while(|c| matches!(c, '"' | '\'' | ')' | ']' | '»' | '”' | '’' | ' '))
        .collect();
    let len = cleaned.chars().count();
    let mut chars = trimmed.chars();
    let total = trimmed.chars().count();
    if total == 0 {
        return false;
    }
    let target_idx = total.saturating_sub(len + 1);
    let last_meaningful = chars.nth(target_idx);
    matches!(
        last_meaningful,
        Some('.') | Some('!') | Some('?') | Some(':') | Some(';')
            | Some('…') | Some('—') | Some('–') | Some('-')
    )
}

/// Whether `line` (after any `<label>: ` prefix) begins with a
/// lowercase letter — the textual cue that this line is the
/// continuation of the previous line's clause rather than a fresh
/// sentence. Lines that start with a digit, opening quote, or
/// punctuation (parenthetical, em-dash) don't qualify; we want a
/// strong signal.
fn starts_lowercase(line: &str) -> bool {
    let body = strip_label_prefix(line);
    body.chars()
        .next()
        .map(|c| c.is_lowercase())
        .unwrap_or(false)
}

/// Maximum word count for a "boundary fragment" that the diarizer
/// likely mis-attributed across the speaker line. 1–3 words covers the
/// real-world failure mode (Whisper emitted "...way. It" and the next
/// chunk started "does pay off..."; or "...Brand" + "new. Hadn't done
/// anything..."). Going wider risks moving real content between
/// speakers.
const BOUNDARY_FRAGMENT_MAX_WORDS: usize = 3;

/// Split a transcript line into `(label_with_separator, body)`. The
/// label part includes the leading whitespace, the label text, the
/// `:`, and the space after it — so concatenating the two halves
/// reproduces the original line exactly. Returns `("", line)` when
/// there's no recognisable label prefix (defensive — production
/// transcripts always have one, but the merge passes also run on
/// hand-edited text).
fn split_label_prefix(line: &str) -> (&str, &str) {
    let trimmed = line.trim_start();
    let leading_ws_len = line.len() - trimmed.len();
    if let Some(colon) = trimmed.find(':') {
        let label = &trimmed[..colon];
        if !label.is_empty() && label.len() <= 40 && !label.contains('\n') {
            let after_colon = &trimmed[colon + 1..];
            let body_offset = colon + 1 + (after_colon.len() - after_colon.trim_start().len());
            let total_offset = leading_ws_len + body_offset;
            return (&line[..total_offset], &line[total_offset..]);
        }
    }
    ("", line)
}

/// Byte offset of the *last* sentence-terminator (`.!?…`) in `s`, or
/// `None` if there isn't one. Used to find the boundary between the
/// last finished sentence on a line and any trailing fragment.
/// Excludes `:;` (more often mid-clause than sentence-final) and
/// dash variants (those signal trailing-off, not a clean sentence
/// boundary we want to split on).
fn last_terminator_index(s: &str) -> Option<usize> {
    s.char_indices()
        .filter(|(_, c)| matches!(c, '.' | '!' | '?' | '…'))
        .map(|(i, _)| i)
        .next_back()
}

/// Byte offset of the *first* sentence-terminator (`.!?…`) in `s`,
/// or `None` if there isn't one.
fn first_terminator_index(s: &str) -> Option<usize> {
    s.char_indices()
        .find(|(_, c)| matches!(c, '.' | '!' | '?' | '…'))
        .map(|(i, _)| i)
}

/// Length in bytes of the character starting at byte index `i` in `s`.
/// Used to step *past* a terminator character we located via
/// `last_terminator_index` / `first_terminator_index`.
fn char_len_at(s: &str, i: usize) -> usize {
    s[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(0)
}

/// Detect "the diarizer attributed the *next* speaker's first 1–3
/// words to the *previous* speaker's turn" — a common Whisper-Word
/// alignment artefact when a sentence terminator lands mid-chunk and
/// the next sentence's opening words spill into the same chunk before
/// the speaker change actually occurs.
///
/// Pattern (after stripping speaker labels):
/// - `prev` body ends with `<sentence>. <1–3 capitalised words>`
///   where the trailing words contain *no* terminator of their own
///   (they're the start of a new sentence, not its end).
/// - `next` body starts with a *lowercase* letter (the continuation
///   that confirms those trailing words begin the same sentence).
///
/// Fix: trim the trailing words from `prev`, prepend them to `next`'s
/// body. Speaker labels are preserved on both lines.
fn forward_move_trailing(prev: &str, next: &str) -> Option<(String, String)> {
    let (prev_label, prev_body) = split_label_prefix(prev);
    let (next_label, next_body) = split_label_prefix(next);

    // Bail on empty / label-only lines.
    if prev_body.trim().is_empty() || next_body.trim().is_empty() {
        return None;
    }

    // The trailing fragment lives after the LAST `.!?…` in prev.
    let term_idx = last_terminator_index(prev_body)?;
    let after_term_byte = term_idx + char_len_at(prev_body, term_idx);
    let trailing = prev_body.get(after_term_byte..)?.trim();
    if trailing.is_empty() {
        return None;
    }

    let trailing_words: Vec<&str> = trailing.split_whitespace().collect();
    if trailing_words.is_empty() || trailing_words.len() > BOUNDARY_FRAGMENT_MAX_WORDS {
        return None;
    }

    // First trailing word must be a sentence start (capitalised). If
    // it's lowercase, this isn't a "next-speaker's-sentence-start"
    // pattern — the full merge below will catch it instead.
    let first_word = *trailing_words.first()?;
    let first_char = first_word.chars().next()?;
    if !first_char.is_uppercase() {
        return None;
    }

    // None of the trailing words may already contain a terminator —
    // that would mean the trailing fragment is itself a complete
    // sentence, which probably belongs to prev as the speaker's last
    // word, not to next.
    if trailing_words
        .iter()
        .any(|w| w.chars().any(|c| matches!(c, '.' | '!' | '?' | '…')))
    {
        return None;
    }

    // next must start lowercase — confirms the trailing fragment is
    // the start of a sentence the next speaker continues.
    if !starts_lowercase(next) {
        return None;
    }

    // Build new prev: keep through the terminator, drop the trailing
    // fragment.
    let kept = prev_body
        .get(..after_term_byte)?
        .trim_end()
        .to_string();
    let new_prev = format!("{prev_label}{kept}");

    // Build new next: prepend trailing fragment to existing body.
    let trailing_joined = trailing_words.join(" ");
    let new_next = format!("{next_label}{trailing_joined} {next_body}");

    Some((new_prev, new_next))
}

/// Detect "the diarizer attributed the *previous* speaker's last 1–3
/// words to the *next* speaker's turn" — the inverse of
/// `forward_move_trailing`.
///
/// Pattern (after stripping speaker labels):
/// - `prev` body ends *without* a sentence terminator (the speaker
///   was mid-clause when the chunk boundary fell).
/// - `next` body starts with `<1–3 words><terminator> <more
///   content>` — i.e. a short fragment that closes a sentence,
///   followed by genuinely new content that's clearly the next
///   speaker's turn.
///
/// Fix: detach the leading fragment (up to and including the first
/// terminator) from `next`, append it to `prev`. Speaker labels are
/// preserved on both lines.
fn backward_move_leading(prev: &str, next: &str) -> Option<(String, String)> {
    let (prev_label, prev_body) = split_label_prefix(prev);
    let (next_label, next_body) = split_label_prefix(next);

    if prev_body.trim().is_empty() || next_body.trim().is_empty() {
        return None;
    }

    // prev must be open (no terminator at end).
    if ends_sentence(prev) {
        return None;
    }

    // next's body must contain a terminator within the first 1–3
    // words.
    let term_idx = first_terminator_index(next_body)?;
    let term_end = term_idx + char_len_at(next_body, term_idx);
    let leading = next_body.get(..term_end)?.trim();
    let trailing = next_body.get(term_end..)?.trim_start();

    let leading_words: Vec<&str> = leading.split_whitespace().collect();
    if leading_words.is_empty() || leading_words.len() > BOUNDARY_FRAGMENT_MAX_WORDS {
        return None;
    }

    // There must be content after the terminator — otherwise we'd be
    // stripping `next` entirely, which is what `merge_run_on_sentences`
    // already does (and gets the speaker attribution wrong, which is
    // why this targeted move exists).
    if trailing.is_empty() {
        return None;
    }

    // Build new prev: append leading fragment to existing body.
    let new_prev = format!("{prev_label}{} {leading}", prev_body.trim_end());

    // Build new next: just the trailing portion, with its label.
    let new_next = format!("{next_label}{trailing}");

    Some((new_prev, new_next))
}

/// Detect a stutter / immediate-repetition signal at a line boundary.
///
/// When `prev` body has format `<earlier text>... <X> <terminator>
/// <Y>` and `next` body has format `<Z> <terminator> <rest>`, the
/// phrase `Y Z` may form a unit. If that exact phrase already appears
/// in `prev` *before* the trailing `Y`, the speaker stuttered or
/// repeated themselves — `Y Z` is the second occurrence, all on the
/// same speaker's line. That's the cue to prefer backward-move (pull
/// `Z` back to prev) over forward-move (push `Y` forward to next).
///
/// Without this signal, the AMC-style case ("AMC was a young company.
/// Brand new. Brand" + "new. Hadn't done anything...") and the "It
/// does pay off" case look identical to the structural rules — both
/// have a 1-word trailing in prev and a 1-word leading in next. Only
/// the repetition check separates them.
fn has_repetition_signal(prev: &str, next: &str) -> bool {
    let (_, prev_body) = split_label_prefix(prev);
    let (_, next_body) = split_label_prefix(next);

    let prev_term = match last_terminator_index(prev_body) {
        Some(i) => i,
        None => return false,
    };
    let after_prev_term = prev_term + char_len_at(prev_body, prev_term);
    let trailing = match prev_body.get(after_prev_term..) {
        Some(t) => t.trim(),
        None => return false,
    };
    if trailing.is_empty() {
        return false;
    }

    let next_term = match first_terminator_index(next_body) {
        Some(i) => i,
        None => return false,
    };
    let leading = match next_body.get(..next_term) {
        Some(l) => l.trim(),
        None => return false,
    };
    if leading.is_empty() {
        return false;
    }

    let search_zone = match prev_body.get(..after_prev_term) {
        Some(z) => z.to_lowercase(),
        None => return false,
    };
    let combined = format!("{trailing} {leading}").to_lowercase();
    search_zone.contains(&combined)
}

/// Heuristic chunk-boundary cleanup. Three operations applied at
/// every adjacent line pair, with the choice between forward and
/// backward moves disambiguated by a repetition-signal tiebreaker:
///
/// 1. **Forward-move** (`forward_move_trailing`): the next speaker's
///    sentence-opening words got attributed to the previous speaker.
///    Trim them off prev, prepend to next.
/// 2. **Backward-move** (`backward_move_leading`): the previous
///    speaker's sentence-closing words got attributed to the next
///    speaker. Trim them off next, append to prev.
/// 3. **Full merge**: prev ends without a terminator AND next starts
///    lowercase, but neither (1) nor (2) applied. Fuse next's body
///    into prev (its label is dropped). Common pattern: Whisper
///    chopped a single sentence across chunks and the diarizer
///    flipped speaker on the second half.
///
/// When forward and backward both qualify, the choice is made by
/// `has_repetition_signal`: if the trailing-of-prev + leading-of-next
/// phrase already appears earlier in prev, the speaker repeated
/// themselves and the words belong on prev (backward); otherwise the
/// trailing was misattributed and the words belong on next (forward).
///
/// All three preserve the textual signal as the safety check —
/// Whisper at a real speaker boundary almost always emits a
/// terminator AND capitalises the next sentence's first word, so
/// when neither holds the boundary is almost certainly a chunk
/// artefact rather than a real turn change.
///
/// Runs after `bridge_short_interjections` (which handles same-line
/// short interjections inside a single chunk), catching what the
/// structural sandwich rule can't.
fn merge_run_on_sentences(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut merged: Vec<String> = Vec::with_capacity(lines.len());
    for line in lines {
        if let Some(prev) = merged.last_mut() {
            let forward = forward_move_trailing(prev, line);
            let backward = backward_move_leading(prev, line);
            let chosen = match (forward, backward) {
                (Some(f), Some(b)) => {
                    if has_repetition_signal(prev, line) {
                        Some(b)
                    } else {
                        Some(f)
                    }
                }
                (Some(f), None) => Some(f),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            };
            if let Some((new_prev, new_next)) = chosen {
                *prev = new_prev;
                merged.push(new_next);
                continue;
            }
            if !ends_sentence(prev) && starts_lowercase(line) {
                let stripped = strip_label_prefix(line);
                prev.push(' ');
                prev.push_str(stripped);
                continue;
            }
        }
        merged.push(line.to_string());
    }
    merged.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mic(start_ms: u64, text: &str) -> ChunkRecord {
        ChunkRecord {
            source: ChunkSource::Mic,
            start_ms,
            text: text.to_string(),
            words: Vec::new(),
            detected_language: None,
        }
    }

    fn sys(start_ms: u64, text: &str) -> ChunkRecord {
        ChunkRecord {
            source: ChunkSource::Sys,
            start_ms,
            text: text.to_string(),
            words: Vec::new(),
            detected_language: None,
        }
    }

    fn sys_with_words(
        start_ms: u64,
        words: Vec<(&str, u64, u64)>,
    ) -> ChunkRecord {
        let text = words.iter().map(|(t, _, _)| *t).collect::<Vec<_>>().join(" ");
        ChunkRecord {
            source: ChunkSource::Sys,
            start_ms,
            text,
            words: words
                .into_iter()
                .map(|(t, s, e)| crate::recording::ChunkWord {
                    text: t.to_string(),
                    start_ms: s,
                    end_ms: e,
                })
                .collect(),
            detected_language: None,
        }
    }

    fn mic_with_words(start_ms: u64, words: Vec<(&str, u64, u64)>) -> ChunkRecord {
        let mut c = sys_with_words(start_ms, words);
        c.source = ChunkSource::Mic;
        c
    }

    fn whole_chunk_pieces<F: Fn(&ChunkRecord) -> Option<String>>(
        labeller: F,
    ) -> impl Fn(&ChunkRecord) -> Vec<LabelledPiece> {
        move |c: &ChunkRecord| single_piece(c, labeller(c))
    }

    #[test]
    fn ends_sentence_recognises_terminators() {
        assert!(ends_sentence("he said."));
        assert!(ends_sentence("really?"));
        assert!(ends_sentence("stop!"));
        assert!(ends_sentence("note: this matters;"));
        assert!(ends_sentence("\"done.\""));
        assert!(ends_sentence("(yes!)"));
        assert!(ends_sentence("trailing space.   "));
    }

    #[test]
    fn ends_sentence_rejects_open_endings() {
        assert!(!ends_sentence("he said"));
        assert!(!ends_sentence("I was thinking that"));
        assert!(!ends_sentence("comma, here"));
        assert!(!ends_sentence(""));
    }

    #[test]
    fn merge_glues_open_clause_to_lowercase_continuation() {
        let input = "Speaker 1: I was thinking that\nSpeaker 1: we should pivot.";
        assert_eq!(
            merge_run_on_sentences(input),
            "Speaker 1: I was thinking that we should pivot."
        );
    }

    #[test]
    fn merge_drops_label_prefix_from_absorbed_line() {
        // Even when the absorbed line is a different speaker, the
        // first speaker keeps the floor — Whisper's mid-utterance
        // chunk break or a borderline diarizer flip is what produced
        // this artefact, not a real speaker change.
        let input = "Speaker 1: anyway the point is\nSpeaker 2: that we are out of time.";
        assert_eq!(
            merge_run_on_sentences(input),
            "Speaker 1: anyway the point is that we are out of time."
        );
    }

    #[test]
    fn forward_move_relocates_long_fragment_trailing_word_to_next() {
        // Real-recording case: a 14-word line ends with "Not" (no
        // terminator) and the next 11-word line starts with "quite"
        // (lowercase). Whisper cut a chunk between "things." and "Not
        // quite 10". Forward-move detaches the trailing "Not" and
        // hands it to next as the start of next's sentence —
        // attribution is preserved on both sides instead of fusing
        // them into one mis-labelled line.
        let input = "Speaker 1: Like you were working probably for 10 years in a lot of different things. Not\nSpeaker 1: quite 10, but a solid six or seven years as a working actor.";
        let output = merge_run_on_sentences(input);
        assert_eq!(
            output,
            "Speaker 1: Like you were working probably for 10 years in a lot of different things.\nSpeaker 1: Not quite 10, but a solid six or seven years as a working actor."
        );
    }

    #[test]
    fn forward_move_relocates_trailing_sentence_start_to_next_speaker() {
        // Real-recording case from a tester transcript: the next
        // speaker's first word ("It") landed on the previous turn
        // because Whisper's chunk boundary fell mid-sentence. The
        // continuation ("does pay off") is lowercase, confirming
        // "It" starts a sentence the next speaker delivers.
        let input = "Speaker 1: starts to unravel in the perfect way. It\nSpeaker 2: does pay off. That's what's really nice.";
        let output = merge_run_on_sentences(input);
        assert_eq!(
            output,
            "Speaker 1: starts to unravel in the perfect way.\nSpeaker 2: It does pay off. That's what's really nice."
        );
    }

    #[test]
    fn forward_move_handles_multi_word_trailing_fragment() {
        // 1–3 trailing capitalised words still qualify; the first must
        // be a sentence start.
        let input = "Speaker 1: that closes the door. So we\nSpeaker 2: can move on now.";
        let output = merge_run_on_sentences(input);
        assert_eq!(
            output,
            "Speaker 1: that closes the door.\nSpeaker 2: So we can move on now."
        );
    }

    #[test]
    fn forward_move_skips_when_trailing_starts_lowercase() {
        // The trailing word "that" is lowercase — it's a continuation
        // of the previous clause, not the start of a new sentence.
        // Falls through to the full merge instead.
        let input = "Speaker 1: I think that\nSpeaker 2: we should do it.";
        let output = merge_run_on_sentences(input);
        assert_eq!(
            output,
            "Speaker 1: I think that we should do it."
        );
    }

    #[test]
    fn backward_move_pulls_leading_sentence_close_to_prev_speaker() {
        // Real-recording case: "AMC was a young company. Brand new.
        // Brand" + "new. Hadn't done anything..." — the diarizer cut
        // mid-utterance and gave "new." to the wrong speaker.
        let input = "Speaker 1: AMC was a young company. Brand new. Brand\nSpeaker 2: new. Hadn't done anything significant yet.";
        let output = merge_run_on_sentences(input);
        assert_eq!(
            output,
            "Speaker 1: AMC was a young company. Brand new. Brand new.\nSpeaker 2: Hadn't done anything significant yet."
        );
    }

    #[test]
    fn backward_move_skips_when_leading_has_no_trailing_content() {
        // If next is *just* the leading fragment (nothing after the
        // terminator), the backward-move would empty it. Fall through
        // to the full merge instead, which produces the cleaner
        // single-line result.
        let input = "Speaker 1: I think we should\nSpeaker 2: stop.";
        let output = merge_run_on_sentences(input);
        // Full merge fires (prev open + next lowercase): joined.
        assert_eq!(output, "Speaker 1: I think we should stop.");
    }

    #[test]
    fn backward_move_skips_when_leading_too_long() {
        // 4-word leading fragment exceeds BOUNDARY_FRAGMENT_MAX_WORDS.
        // The boundary stays put — wider moves risk shuffling real
        // content between speakers.
        let input = "Speaker 1: I was about to\nSpeaker 2: yes that is correct. Anyway moving on.";
        let output = merge_run_on_sentences(input);
        // Full merge fires (prev open + next lowercase "yes"). Acceptable
        // fallback — at least the sentence is intact even if attribution
        // collapses to prev.
        assert!(output.contains("Speaker 1: I was about to yes that is correct"), "got: {output}");
    }

    #[test]
    fn merge_keeps_em_dash_interruption_distinct() {
        // A speaker trailing off / interrupted ends with an em-dash —
        // the next speaker's lowercase reply must NOT fuse, or two
        // distinct turns become one nonsensical sentence.
        let input = "Speaker 1: But what about the —\nSpeaker 2: right but we have no time.";
        assert_eq!(merge_run_on_sentences(input), input);
    }

    #[test]
    fn merge_keeps_en_dash_interruption_distinct() {
        let input = "Speaker 1: I was going to say –\nSpeaker 2: yeah I know what you mean.";
        assert_eq!(merge_run_on_sentences(input), input);
    }

    #[test]
    fn merge_keeps_lines_that_already_end_in_terminator() {
        let input = "Speaker 1: hello there.\nSpeaker 2: yes hi.";
        assert_eq!(merge_run_on_sentences(input), input);
    }

    #[test]
    fn merge_keeps_lines_when_next_starts_uppercase() {
        // No terminator on prev, but next starts with a capital — most
        // likely a fresh sentence the speaker began. Keep separate.
        let input = "Speaker 1: I was thinking that\nSpeaker 2: We should pivot.";
        assert_eq!(merge_run_on_sentences(input), input);
    }

    #[test]
    fn merge_handles_norwegian_lowercase_continuation() {
        // The unicode lowercase check covers non-ASCII letters; common
        // Humla case for Norwegian recordings.
        let input = "Speaker 1: vi har snakket om\nSpeaker 1: økonomien lenge.";
        assert_eq!(
            merge_run_on_sentences(input),
            "Speaker 1: vi har snakket om økonomien lenge."
        );
    }

    #[test]
    fn merge_chains_multiple_consecutive_glues() {
        // Three lines, all open-ended, all start lowercase → collapse
        // into a single line. The merged line keeps growing under the
        // word-count guard because we measure prev's length on each
        // step (it's well past 8 words by line 3 — but next is short).
        let input = "Speaker 1: so I was thinking\nSpeaker 1: that we should\nSpeaker 1: pivot the strategy.";
        assert_eq!(
            merge_run_on_sentences(input),
            "Speaker 1: so I was thinking that we should pivot the strategy."
        );
    }

    #[test]
    fn merge_passes_through_unlabelled_lines() {
        // Defensive: lines without `Label:` prefix shouldn't gain one
        // from the merge pass, and the prefix-stripping helper must
        // not eat content that isn't actually a label.
        let input = "first fragment\nsecond piece.";
        assert_eq!(
            merge_run_on_sentences(input),
            "first fragment second piece."
        );
    }

    /// Build a piece directly without going through chunk splitting,
    /// with synthetic word timings. Each word is given a 200 ms span
    /// at `start_ms + i * 250 ms` so the caller can construct short
    /// (backchannel-shaped) or long pieces by varying word count.
    /// Pass `words = false` to produce a piece with no timing data,
    /// modelling the OpenAI / older-diagnostic path.
    fn piece(label: Option<&str>, text: &str, words: bool) -> LabelledPiece {
        let word_vec = if words {
            text.split_whitespace()
                .enumerate()
                .map(|(i, w)| crate::recording::ChunkWord {
                    text: w.to_string(),
                    start_ms: (i as u64) * 250,
                    end_ms: (i as u64) * 250 + 200,
                })
                .collect()
        } else {
            Vec::new()
        };
        LabelledPiece {
            label: label.map(str::to_string),
            text: text.to_string(),
            words: word_vec,
        }
    }

    /// Build a piece with explicit word timings. Useful when the test
    /// needs a specific acoustic span (e.g. a slow 1-word reply that
    /// shouldn't bridge).
    fn piece_with_span(
        label: Option<&str>,
        text: &str,
        start_ms: u64,
        end_ms: u64,
    ) -> LabelledPiece {
        let words: Vec<&str> = text.split_whitespace().collect();
        let n = words.len().max(1);
        let step = (end_ms - start_ms) / n as u64;
        let word_vec = words
            .iter()
            .enumerate()
            .map(|(i, w)| crate::recording::ChunkWord {
                text: (*w).to_string(),
                start_ms: start_ms + step * i as u64,
                end_ms: start_ms + step * (i as u64 + 1),
            })
            .collect();
        LabelledPiece {
            label: label.map(str::to_string),
            text: text.to_string(),
            words: word_vec,
        }
    }

    #[test]
    fn bridge_absorbs_short_sandwiched_interjection() {
        // The motivating case: Speaker 2 emits one word ("yeah") while
        // Speaker 1 is talking on either side. Should collapse.
        let mut pieces = vec![
            piece(Some("Speaker 1"), "so what I was saying about", true),
            piece(Some("Speaker 2"), "yeah", true),
            piece(Some("Speaker 1"), "the migration plan", true),
        ];
        bridge_short_interjections(&mut pieces);
        assert_eq!(pieces[0].label.as_deref(), Some("Speaker 1"));
        assert_eq!(pieces[1].label.as_deref(), Some("Speaker 1"));
        assert_eq!(pieces[2].label.as_deref(), Some("Speaker 1"));
    }

    #[test]
    fn bridge_keeps_long_reply_distinct() {
        // A seven-word reply exceeds BRIDGE_MAX_WORDS (6), so the
        // speaker boundary survives even when sandwiched.
        let mut pieces = vec![
            piece(Some("Speaker 1"), "do you have time", true),
            piece(Some("Speaker 2"), "no I really do not have time", true),
            piece(Some("Speaker 1"), "ok let me try later", true),
        ];
        bridge_short_interjections(&mut pieces);
        assert_eq!(pieces[1].label.as_deref(), Some("Speaker 2"));
    }

    #[test]
    fn bridge_keeps_short_but_slow_reply_distinct() {
        // Same speaker on either side, a sandwich, but the middle
        // piece's acoustic span is 4 s — exceeds BRIDGE_MAX_DURATION_MS
        // (3500). The duration gate is what protects real short turns
        // delivered slowly ("absolutely" with a long pause-filled
        // delivery) from getting fused into the surrounding speaker.
        let mut pieces = vec![
            piece(Some("Speaker 1"), "what do you think", true),
            piece_with_span(Some("Speaker 2"), "no", 0, 4_000),
            piece(Some("Speaker 1"), "fair enough", true),
        ];
        bridge_short_interjections(&mut pieces);
        assert_eq!(pieces[1].label.as_deref(), Some("Speaker 2"));
    }

    #[test]
    fn bridge_skips_pieces_without_word_timings() {
        // OpenAI transcribe and older diagnostic JSONs don't carry
        // per-word timings. We can't tell backchannel from real reply
        // without duration, so we conservatively leave the boundary.
        let mut pieces = vec![
            piece(Some("Speaker 1"), "first turn", false),
            piece(Some("Speaker 2"), "second turn", false),
            piece(Some("Speaker 1"), "third turn", false),
        ];
        bridge_short_interjections(&mut pieces);
        assert_eq!(pieces[1].label.as_deref(), Some("Speaker 2"));
    }

    #[test]
    fn bridge_does_not_fuse_distinct_neighbours() {
        // A short piece between two *different* speakers is a real
        // turn ("ok" between A and B in a back-and-forth), not noise.
        // The structural sandwich check is what blocks the collapse.
        let mut pieces = vec![
            piece(Some("Speaker 1"), "see you tomorrow", true),
            piece(Some("Speaker 2"), "ok", true),
            piece(Some("Speaker 3"), "bye both", true),
        ];
        bridge_short_interjections(&mut pieces);
        assert_eq!(pieces[1].label.as_deref(), Some("Speaker 2"));
    }

    #[test]
    fn bridge_skips_first_and_last_pieces() {
        // The boundary pieces have no "prev" or "next" with a different
        // label, so even short ones stay put. Otherwise the very first
        // utterance ("hi") of a meeting would get glued onto the
        // following speaker's first turn.
        let mut pieces = vec![
            piece(Some("Speaker 2"), "hi", true),
            piece(Some("Speaker 1"), "hello there how are you doing", true),
            piece(Some("Speaker 2"), "ok", true),
        ];
        bridge_short_interjections(&mut pieces);
        assert_eq!(pieces[0].label.as_deref(), Some("Speaker 2"));
        assert_eq!(pieces[2].label.as_deref(), Some("Speaker 2"));
    }

    #[test]
    fn continuation_chain_uses_surrounding_label_when_pre_and_post_agree() {
        // The "Men det jeg kunne / gjort, var jo..." case: the
        // 16-word mid-sentence fragment exceeds bridge limits but is
        // clearly the surrounding speaker's continuation. pre and
        // post both Speaker 1 → Speaker 1 wins despite Speaker 3's
        // word count dominance inside the chain.
        let mut pieces = vec![
            piece(Some("Speaker 1"), "Tidligere noe annet.", true),
            piece(Some("Speaker 1"), "Men det jeg kunne", true),
            piece(
                Some("Speaker 3"),
                "gjort, var jo å endra det til å komme opp i en slags pop-opp",
                true,
            ),
            piece(Some("Speaker 1"), "Sånn som den legges i lesninga her.", true),
        ];
        absorb_text_continuation_chains(&mut pieces);
        assert_eq!(pieces[1].label.as_deref(), Some("Speaker 1"));
        assert_eq!(pieces[2].label.as_deref(), Some("Speaker 1"));
    }

    #[test]
    fn continuation_chain_picks_longest_when_pre_and_post_disagree() {
        // "Ja, jeg" (S2) / "skjønner" (S1) / "det. Men jeg så ..."
        // (S3, dominant): the surrounding labels disagree (pre=S3,
        // post=S2), so the longest piece's label wins → S3.
        let mut pieces = vec![
            piece(
                Some("Speaker 3"),
                "Det er så mye rundt at vi så det ikke.",
                true,
            ),
            piece(Some("Speaker 2"), "Ja, jeg", true),
            piece(Some("Speaker 1"), "skjønner", true),
            piece(
                Some("Speaker 3"),
                "det. Men jeg så vi egentlig snakka om opprinnelig,",
                true,
            ),
            piece(Some("Speaker 2"), "altså, så det er ikke", true),
        ];
        absorb_text_continuation_chains(&mut pieces);
        // The chain extends through pieces 1..=4 (piece 0 ends with
        // "." → not chain head). pre=S3 (piece 0), post=None (chain
        // reaches the end → no post). Longest wins: S3 has 9+5=14
        // words, S2 has 2+5=7, S1 has 1. S3 wins.
        assert_eq!(pieces[1].label.as_deref(), Some("Speaker 3"));
        assert_eq!(pieces[2].label.as_deref(), Some("Speaker 3"));
        assert_eq!(pieces[3].label.as_deref(), Some("Speaker 3"));
        assert_eq!(pieces[4].label.as_deref(), Some("Speaker 3"));
        // Outside the chain: untouched.
        assert_eq!(pieces[0].label.as_deref(), Some("Speaker 3"));
    }

    #[test]
    fn continuation_chain_does_nothing_when_single_label() {
        // Chain spans pieces 0,1 but both already Speaker 1 — no-op.
        let mut pieces = vec![
            piece(Some("Speaker 1"), "Men jeg syns det er rart at", true),
            piece(Some("Speaker 1"), "vi gjør det sånn.", true),
        ];
        let before: Vec<_> = pieces.iter().map(|p| p.label.clone()).collect();
        absorb_text_continuation_chains(&mut pieces);
        let after: Vec<_> = pieces.iter().map(|p| p.label.clone()).collect();
        assert_eq!(before, after);
    }

    #[test]
    fn continuation_chain_breaks_at_sentence_terminator() {
        // Prev ends with "." → no chain. Speaker labels untouched.
        let mut pieces = vec![
            piece(Some("Speaker 1"), "End of one thought.", true),
            piece(Some("Speaker 2"), "starts another with lowercase.", true),
        ];
        absorb_text_continuation_chains(&mut pieces);
        assert_eq!(pieces[0].label.as_deref(), Some("Speaker 1"));
        assert_eq!(pieces[1].label.as_deref(), Some("Speaker 2"));
    }

    #[test]
    fn continuation_chain_breaks_at_uppercase_start() {
        // Prev ends mid-clause, next starts uppercase → next is a new
        // sentence (a real speaker change). Don't fuse.
        let mut pieces = vec![
            piece(Some("Speaker 1"), "He said", true),
            piece(Some("Speaker 2"), "Hello world.", true),
        ];
        absorb_text_continuation_chains(&mut pieces);
        assert_eq!(pieces[0].label.as_deref(), Some("Speaker 1"));
        assert_eq!(pieces[1].label.as_deref(), Some("Speaker 2"));
    }

    #[test]
    fn continuation_chain_treats_trailing_ellipsis_as_unfinished() {
        // "I was going to ..." ends with "..." — Whisper's trailing-off
        // marker, NOT a terminator. The chain should extend.
        let mut pieces = vec![
            piece(Some("Speaker 1"), "I was going to ...", true),
            piece(Some("Speaker 2"), "say something here.", true),
        ];
        absorb_text_continuation_chains(&mut pieces);
        // Both should share a label after absorption.
        assert_eq!(pieces[0].label, pieces[1].label);
    }

    #[test]
    fn continuation_chain_idempotent() {
        let mut pieces = vec![
            piece(Some("Speaker 1"), "Hvis du", true),
            piece(Some("Speaker 2"), "bare klikker på testkurs oppå", true),
            piece(Some("Speaker 2"), "den gule igjen, da.", true),
        ];
        absorb_text_continuation_chains(&mut pieces);
        let once: Vec<_> = pieces.iter().map(|p| p.label.clone()).collect();
        absorb_text_continuation_chains(&mut pieces);
        let twice: Vec<_> = pieces.iter().map(|p| p.label.clone()).collect();
        assert_eq!(once, twice);
    }

    #[test]
    fn piece_ends_terminator_recognises_typical_endings() {
        assert!(piece_ends_terminator("Hello world."));
        assert!(piece_ends_terminator("Really?"));
        assert!(piece_ends_terminator("Stop!"));
        assert!(piece_ends_terminator("see this:"));
        assert!(piece_ends_terminator("done.\""));
        assert!(!piece_ends_terminator("I was going to"));
        assert!(!piece_ends_terminator("I was going to ..."));
        assert!(!piece_ends_terminator("Hvis du"));
        assert!(!piece_ends_terminator("trailing comma,"));
    }

    #[test]
    fn piece_starts_continuation_recognises_typical_openings() {
        assert!(piece_starts_continuation("bare klikker..."));
        assert!(piece_starts_continuation("...continuation"));
        assert!(piece_starts_continuation("…og så"));
        assert!(!piece_starts_continuation("Hello world"));
        assert!(!piece_starts_continuation("Men hva nå?"));
    }

    #[test]
    fn hybrid_fallback_keeps_sys_chunks_distinct_from_mic() {
        // Reproduces the silent-merge bug: in the (mic+sys) branch when
        // diarize is unavailable for the sys stream, sys chunks must NOT
        // get a None label — that would glue their text onto the previous
        // `You:` line, hiding remote speech inside the user's transcript.
        // The single-speaker fallback labels them `Speaker 1` so the
        // boundary survives.
        let chunks = vec![
            mic(0, "Ok thanks."),
            sys(500, "You got it."),
            mic(2000, "See you tomorrow."),
        ];
        let labeller = |c: &ChunkRecord| match c.source {
            ChunkSource::Mic => Some("You".to_string()),
            ChunkSource::Sys => Some("Speaker 1".to_string()),
        };
        assert_eq!(
            build_labelled_transcript(&chunks, &whole_chunk_pieces(labeller)),
            "You: Ok thanks.\nSpeaker 1: You got it.\nYou: See you tomorrow."
        );
    }

    #[test]
    fn label_returning_none_glues_to_previous_label_dont_use_for_distinct_speakers() {
        // Documents the underlying behavior the fallback above protects
        // against. With the buggy labeller (sys → None) the remote text
        // appears inside the user's `You:` line — silent data loss for
        // the reader. Locked into a test so a future "simplification" of
        // the fallback that goes back to None gets caught here.
        let chunks = vec![
            mic(0, "Ok thanks."),
            sys(500, "You got it."),
        ];
        let buggy = |c: &ChunkRecord| match c.source {
            ChunkSource::Mic => Some("You".to_string()),
            ChunkSource::Sys => None,
        };
        let result = build_labelled_transcript(&chunks, &whole_chunk_pieces(buggy));
        // This is the pathological output we DO NOT want from the
        // production code; it's only here as a tripwire on the helper.
        assert_eq!(result, "You: Ok thanks. You got it.");
    }

    #[test]
    fn build_transcript_orders_by_start_ms_with_mic_priority_on_tie() {
        // Mic and sys chunks at the same start_ms — mic is emitted first.
        // Reflects the typical UX assumption that the user speaks before
        // they hear a response, and stabilises ordering on tie.
        let chunks = vec![
            sys(0, "From sys."),
            mic(0, "From mic."),
        ];
        let labeller = |c: &ChunkRecord| match c.source {
            ChunkSource::Mic => Some("You".to_string()),
            ChunkSource::Sys => Some("Speaker 1".to_string()),
        };
        assert_eq!(
            build_labelled_transcript(&chunks, &whole_chunk_pieces(labeller)),
            "You: From mic.\nSpeaker 1: From sys."
        );
    }


    #[test]
    fn dedup_drops_mic_chunk_echoing_overlapping_sys() {
        // The bug: speakers play remote voice, mic re-captures it, both
        // streams transcribe the same words. The mic version must drop.
        let chunks = vec![
            sys(0, "we should ship the migration on Friday before the freeze"),
            mic(200, "we should ship the migration on Friday"),
            sys(8000, "agreed sounds good to me"),
        ];
        let labeller = |c: &ChunkRecord| match c.source {
            ChunkSource::Mic => Some("You".to_string()),
            ChunkSource::Sys => Some("Speaker 1".to_string()),
        };
        let out = build_labelled_transcript(&chunks, &whole_chunk_pieces(labeller));
        // Mic echo dropped; only the sys content remains.
        assert_eq!(
            out,
            "Speaker 1: we should ship the migration on Friday before the freeze agreed sounds good to me"
        );
    }

    #[test]
    fn dedup_keeps_mic_chunk_with_distinct_user_speech() {
        // User actually talking over the remote — different words, low
        // containment, mic must survive.
        let chunks = vec![
            sys(0, "we should ship the migration on Friday before the freeze"),
            mic(200, "actually I want to push back on that timeline"),
        ];
        let labeller = |c: &ChunkRecord| match c.source {
            ChunkSource::Mic => Some("You".to_string()),
            ChunkSource::Sys => Some("Speaker 1".to_string()),
        };
        let out = build_labelled_transcript(&chunks, &whole_chunk_pieces(labeller));
        assert!(out.contains("actually I want to push back"));
        assert!(out.contains("Speaker 1: we should ship"));
    }

    #[test]
    fn dedup_keeps_short_mic_acks_even_if_words_appear_in_sys() {
        // "yeah" / "ok" alone shouldn't drop just because those tokens
        // appear in any sys window; they're valid backchannels. Sys
        // chunk ends with a terminator so the post-build heuristic
        // merge doesn't dissolve the speaker boundary.
        let chunks = vec![
            sys(0, "So the ship date is yeah ok confirmed for Friday."),
            mic(500, "yeah"),
            mic(1000, "ok"),
        ];
        let labeller = |c: &ChunkRecord| match c.source {
            ChunkSource::Mic => Some("You".to_string()),
            ChunkSource::Sys => Some("Speaker 1".to_string()),
        };
        let out = build_labelled_transcript(&chunks, &whole_chunk_pieces(labeller));
        assert!(out.contains("You: yeah ok"));
    }

    #[test]
    fn dedup_keeps_mic_when_sys_window_is_far_away() {
        // Mic chunk's text matches sys text 30 seconds later — outside the
        // overlap window, must NOT dedup. The remote echoing the user
        // later is genuine new content. Sentence terminators on each
        // chunk keep the post-build heuristic merge from dissolving the
        // speaker boundary (the merge rule needs prev to lack a
        // terminator, which it doesn't here).
        let chunks = vec![
            mic(0, "The proposal is to extend the deadline to Friday."),
            sys(30_000, "The proposal is to extend the deadline to Friday."),
        ];
        let labeller = |c: &ChunkRecord| match c.source {
            ChunkSource::Mic => Some("You".to_string()),
            ChunkSource::Sys => Some("Speaker 1".to_string()),
        };
        let out = build_labelled_transcript(&chunks, &whole_chunk_pieces(labeller));
        assert!(out.contains("You: The proposal is to extend"));
        assert!(out.contains("Speaker 1: The proposal is to extend"));
    }

    #[test]
    fn dedup_drops_all_mic_when_sys_mirrors_them() {
        // The intended remote-meeting / podcast-through-speakers case:
        // mic and sys carry near-identical text because the speakers
        // are leaking system audio into the mic. Dedup must drop every
        // mic chunk so the user doesn't see "You: foo / Speaker 1: foo"
        // duplicate-turn pairs in the rendered transcript.
        let chunks = vec![
            mic(0, "this is the first thing I am saying about politics"),
            sys(20, "this is the first thing I am saying about politics"),
            mic(3000, "and now the second thing about media coverage"),
            sys(3020, "and now the second thing about media coverage"),
            mic(6000, "third point on commentary today"),
            sys(6020, "third point on commentary today"),
            mic(9000, "fourth and final remark on the topic"),
            sys(9020, "fourth and final remark on the topic"),
        ];
        let labeller = |c: &ChunkRecord| match c.source {
            ChunkSource::Mic => Some("You".to_string()),
            ChunkSource::Sys => Some("Speaker 1".to_string()),
        };
        let out = build_labelled_transcript(&chunks, &whole_chunk_pieces(labeller));
        // No "You:" turns survive — only the sys content is shown.
        assert!(!out.contains("You:"));
        assert!(out.contains("Speaker 1: this is the first thing"));
    }

    #[test]
    fn dedup_noop_when_only_mic_chunks_present() {
        // In-person mode: only mic chunks. Dedup must not touch anything.
        let chunks = vec![
            mic(0, "this is the first thing I am saying"),
            mic(3000, "and now the second thing"),
        ];
        let labeller = |c: &ChunkRecord| match c.source {
            ChunkSource::Mic => Some("Speaker 1".to_string()),
            ChunkSource::Sys => Some("Speaker 2".to_string()),
        };
        let out = build_labelled_transcript(&chunks, &whole_chunk_pieces(labeller));
        assert_eq!(
            out,
            "Speaker 1: this is the first thing I am saying and now the second thing"
        );
    }

    #[test]
    fn dedup_drops_mic_echo_straddling_two_sys_chunks() {
        // Continuous speech cuts both streams at the 15 s cap, each on its own
        // phase: the mic chunk at 45–60 s echoes 45–54 s of the sys chunk at
        // 39–54 s and 54–60 s of the one at 54–69 s.
        fn one_word_a_second(words: &[String]) -> Vec<(&str, u64, u64)> {
            words
                .iter()
                .zip(0u64..)
                .map(|(w, i)| (w.as_str(), i * 1_000, i * 1_000 + 800))
                .collect()
        }
        let vocab: Vec<String> = (0..30).map(|i| format!("word{i}")).collect();
        let chunks = vec![
            sys_with_words(39_000, one_word_a_second(&vocab[0..15])),
            sys_with_words(54_000, one_word_a_second(&vocab[15..30])),
            mic_with_words(45_000, one_word_a_second(&vocab[6..21])),
        ];
        let kept = dedup_mic_against_sys(&chunks);
        assert!(
            kept.iter().all(|c| c.source == ChunkSource::Sys),
            "echo kept: {kept:?}"
        );
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn dedup_drops_straddling_mic_echo_without_word_timings() {
        // The same phases from a provider that returns no word timings, at 40
        // words per 15 s chunk: sys 39 s holds words 0–39, sys 54 s words
        // 40–79, and the mic chunk at 45 s echoes words 16–55.
        let vocab: Vec<String> = (0..80).map(|i| format!("word{i}")).collect();
        let text = |range: std::ops::Range<usize>| vocab[range].join(" ");
        let chunks = vec![
            sys(39_000, &text(0..40)),
            sys(54_000, &text(40..80)),
            mic(45_000, &text(16..56)),
        ];
        let kept = dedup_mic_against_sys(&chunks);
        assert!(
            kept.iter().all(|c| c.source == ChunkSource::Sys),
            "echo kept: {kept:?}"
        );
    }

    #[test]
    fn dedup_keeps_mic_speech_the_remote_repeats_after_it_ends() {
        // The mic chunk's speech is over at 3 s. The same words on sys from
        // 8 s can't be its echo.
        let chunks = vec![
            mic_with_words(
                0,
                vec![
                    ("We", 0, 400),
                    ("ship", 400, 900),
                    ("the", 900, 1_200),
                    ("migration", 1_200, 2_000),
                    ("on", 2_000, 2_300),
                    ("Friday.", 2_300, 3_000),
                ],
            ),
            sys(8_000, "We ship the migration on Friday."),
        ];
        assert_eq!(dedup_mic_against_sys(&chunks).len(), 2);
    }

    #[test]
    fn dedup_keeps_mic_speech_a_short_sys_reply_cannot_account_for() {
        // Every word of the remote's reply is in the user's next sentence,
        // but they are three of its thirteen.
        let chunks = vec![
            sys(10_000, "Ja, det er det."),
            mic(
                11_500,
                "Ja, det er det jeg tenker også, vi må ha budsjettet klart før fredag.",
            ),
        ];
        let kept = dedup_mic_against_sys(&chunks);
        assert!(
            kept.iter().any(|c| c.source == ChunkSource::Mic),
            "user speech dropped: {kept:?}"
        );
    }

    #[test]
    fn dedup_drops_mic_echo_of_a_short_sys_chunk_with_an_extra_word() {
        let chunks = vec![
            sys(10_000, "Sounds good, see you Friday."),
            mic(10_150, "Sounds good, see you on Friday."),
        ];
        let kept = dedup_mic_against_sys(&chunks);
        assert!(
            kept.iter().all(|c| c.source == ChunkSource::Sys),
            "echo kept: {kept:?}"
        );
    }

    #[test]
    fn token_containment_perfect_subset() {
        let a: Vec<String> = ["ship", "the", "migration"].iter().map(|s| s.to_string()).collect();
        let b: Vec<String> = ["we", "should", "ship", "the", "migration", "on", "friday"]
            .iter().map(|s| s.to_string()).collect();
        // a ⊆ b → containment 1.0
        assert!((token_containment(&a, &b) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn token_containment_no_overlap() {
        let a: Vec<String> = ["completely", "different", "words"].iter().map(|s| s.to_string()).collect();
        let b: Vec<String> = ["nothing", "in", "common"].iter().map(|s| s.to_string()).collect();
        assert!(token_containment(&a, &b) < 1e-6);
    }

    #[test]
    fn token_containment_is_the_share_of_a_found_in_b() {
        let a: Vec<String> = ["ja", "det", "er", "budsjettet", "klart", "fredag"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let b: Vec<String> = ["ja", "det", "er"].iter().map(|s| s.to_string()).collect();
        assert!((token_containment(&a, &b) - 0.5).abs() < 1e-6);
        assert!((token_containment(&b, &a) - 1.0).abs() < 1e-6);
    }
}
