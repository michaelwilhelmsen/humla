//! Assembling the labelled transcript from transcribed chunks: per-speaker
//! pieces split on word timings, mic echo dropped against the system stream,
//! short interjections folded into the surrounding speaker, sentence
//! fragments handed across a speaker change to the side they belong to, and
//! continuation chains given one speaker. `refine_pieces` is that sequence;
//! the transcript string and `timeline.jsonl` both come out of it.

use super::diarization::assign_speaker;
use crate::diarize;
use crate::recording::{ChunkRecord, ChunkSource, ChunkWord};

/// One contiguous run of words within a chunk that all belong to the
/// same speaker. A 15-second VAD chunk that opens with one voice and
/// ends with another becomes two pieces; a single-voice chunk stays
/// one piece.
///
/// `words` are relative to `base_ms` (the chunk's start, matching
/// `ChunkWord`'s convention) so the timeline serialiser can rebase them to
/// stream-absolute, and may be empty when the underlying chunk had no word
/// data and we fell back to whole-chunk labelling.
#[derive(Clone, Debug)]
pub(super) struct LabelledPiece {
    pub(super) label: Option<String>,
    pub(super) text: String,
    pub(super) words: Vec<ChunkWord>,
    /// The stream the piece was heard on. Boundary repair only moves words
    /// between pieces of one stream: a mic word is never a system speaker's.
    pub(super) source: ChunkSource,
    /// Stream-absolute time `words` are relative to: the chunk's start, until
    /// a boundary move hands the piece words from an earlier chunk and the
    /// base drops to that chunk's start.
    pub(super) base_ms: u64,
}

/// Wrap a chunk's full text + words as a single labelled piece. Used by
/// the paths that don't split (mic = "You" in hybrid mode, single-
/// speaker fallbacks when diarize returned nothing).
pub(super) fn single_piece(c: &ChunkRecord, label: Option<String>) -> Vec<LabelledPiece> {
    vec![LabelledPiece {
        label,
        text: c.text.clone(),
        words: display_words(c),
        source: c.source,
        base_ms: c.start_ms,
    }]
}

/// Most words `display_words` looks ahead in the chunk's text for the one a
/// timed word matches. Enough to step over a token the provider split or
/// merged differently from its transcript; small enough that a common word
/// can't latch onto a later sentence.
const DISPLAY_WORD_LOOKAHEAD: usize = 4;

/// A word folded to what two spellings of it share: alphanumerics,
/// lowercased. `"Hello,"` and `"hello"` fold alike.
fn fold_word(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// The chunk's timed words, each spelled the way the chunk's own transcript
/// spells it where the two agree.
///
/// Several providers return word timings without the transcript's
/// punctuation or casing: Deepgram's `word`, and the `verbose_json` words of
/// whisper-1 and Groq. The reader shows word texts, and the sentence rules in
/// `realign_sentence_boundaries` and `absorb_text_continuation_chains` read
/// terminators and capitals, so a bare `hello` where the transcript says
/// `Hello.` loses both. Local Whisper's words already carry their
/// punctuation, which makes this a no-op there. A word that matches no
/// nearby transcript token keeps its own text, so nothing is ever dropped or
/// invented.
fn display_words(c: &ChunkRecord) -> Vec<ChunkWord> {
    let tokens: Vec<&str> = c.text.split_whitespace().collect();
    let mut next = 0usize;
    c.words
        .iter()
        .map(|w| {
            let folded = fold_word(&w.text);
            if folded.is_empty() {
                return w.clone();
            }
            let end = (next + DISPLAY_WORD_LOOKAHEAD).min(tokens.len());
            match (next..end).find(|&k| fold_word(tokens[k]) == folded) {
                Some(k) => {
                    next = k + 1;
                    ChunkWord { text: tokens[k].to_string(), ..w.clone() }
                }
                None => w.clone(),
            }
        })
        .collect()
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
    for word in display_words(c) {
        let mid = word.start_ms.saturating_add(word.end_ms.saturating_sub(word.start_ms) / 2);
        let abs = c.start_ms.saturating_add(mid);
        let label = label_for_time(abs);
        match pieces.last_mut() {
            Some(last) if last.label == label => {
                last.text.push(' ');
                last.text.push_str(&word.text);
                last.words.push(word);
            }
            _ => {
                pieces.push(LabelledPiece {
                    label,
                    text: word.text.clone(),
                    words: vec![word],
                    source: c.source,
                    base_ms: c.start_ms,
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

/// Whether `text`'s last meaningful character ends the piece's thought: a
/// sentence terminator (`.!?:;`) or a dash (`—`, `–`, a final `-`), which
/// marks a speaker breaking off or being interrupted. Fusing across a dash
/// joins two distinct turns into nonsense. Trailing ellipsis ("..." or
/// Unicode "…") is stripped first because Whisper uses them to mark
/// trailing-off mid-utterance, not sentence end — a distinction
/// `ends_sentence` deliberately ignores since its caller (the backward
/// boundary move) wants to be conservative about joining across any pause
/// cue.
pub(super) fn piece_ends_terminator(text: &str) -> bool {
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
            | Some('—') | Some('–') | Some('-')
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
            // Word count per label, in the order the labels first appear in
            // the chain, so a tie goes to the label that spoke first on every
            // run.
            let mut word_count_by_label: Vec<(String, usize)> = Vec::new();
            for piece in &pieces[i..=end] {
                if let Some(label) = &piece.label {
                    let wc = piece.text.split_whitespace().count();
                    match word_count_by_label.iter_mut().find(|(l, _)| l == label) {
                        Some((_, count)) => *count += wc,
                        None => word_count_by_label.push((label.clone(), wc)),
                    }
                }
            }
            if word_count_by_label.len() >= 2 {
                let chosen = pre_label
                    .as_ref()
                    .filter(|l| post_label.as_ref() == Some(l))
                    .filter(|l| word_count_by_label.iter().any(|(k, _)| k == *l))
                    .cloned()
                    .or_else(|| {
                        word_count_by_label
                            .iter()
                            .fold(None::<&(String, usize)>, |best, cur| match best {
                                Some(b) if b.1 >= cur.1 => Some(b),
                                _ => Some(cur),
                            })
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

/// The pieces before `refine_pieces`: echo dedup, a sort by
/// `(start_ms, source)` with mic first on a tie, and the per-chunk split.
/// Each stream's `start_ms` counts from its own first frame, a few hundred ms
/// apart, which a reader doesn't notice.
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
        for mut piece in split_chunk(chunk) {
            if piece.text.trim().is_empty() {
                continue;
            }
            // Word timings are relative to this chunk's own start, whatever
            // copy of it the splitter looked labels up against.
            piece.base_ms = chunk.start_ms;
            piece.source = chunk.source;
            all_pieces.push(piece);
        }
    }
    all_pieces
}

/// Every correction the labelled pieces get after the diarizer's split, in
/// order. The one sequence `build_labelled_transcript`, `serialize_timeline`
/// and the diagnostic dump all run, so the transcript string and the timeline
/// can't disagree about who said what. Boundary moves run before continuation
/// chains: both fire on an open piece before a lowercase one, and moving the
/// stray fragment first closes the sentence, so the chain rule doesn't hand a
/// whole turn to the other speaker for two words.
pub(super) fn refine_pieces(pieces: &mut [LabelledPiece]) {
    bridge_short_interjections(pieces);
    realign_sentence_boundaries(pieces);
    absorb_text_continuation_chains(pieces);
}

/// Rebuild the transcript by walking chunks in chronological order and
/// emitting each one prefixed with its assigned label. Same-label runs
/// get a single space between chunks (continuation); label changes get
/// a newline + new prefix. Chunks the labeller declines to label
/// (returns `None`) get joined to whatever came before them with a
/// space, no prefix change — typically only happens when diarize
/// produces zero segments and we're degrading gracefully.
///
/// Cross-stream echo dedup runs first via `dedup_mic_against_sys` —
/// see that function for details — then `refine_pieces`.
pub(super) fn build_labelled_transcript(
    chunks: &[ChunkRecord],
    split_chunk: &(dyn Fn(&ChunkRecord) -> Vec<LabelledPiece> + Send),
) -> String {
    let mut all_pieces = build_pieces_unbridged(chunks, split_chunk);
    refine_pieces(&mut all_pieces);

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
    output
}

/// Whether `text` ends with a sentence-terminating punctuation mark,
/// allowing trailing close-quote/close-paren after the terminator
/// (`he said.` / `(yes!)` / `"done."`). Trailing whitespace is ignored.
///
/// Em-dash (`—`), en-dash (`–`), and a final ASCII hyphen are also
/// treated as terminators. They mark a speaker trailing off mid-thought
/// or being interrupted — joining across them would fuse two distinct
/// turns into nonsense.
fn ends_sentence(text: &str) -> bool {
    let trimmed = text.trim_end();
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

/// Maximum word count for a "boundary fragment" that the diarizer
/// likely mis-attributed across the speaker line. 1–3 words covers the
/// real-world failure mode (Whisper emitted "...way. It" and the next
/// chunk started "does pay off..."; or "...Brand" + "new. Hadn't done
/// anything..."). Going wider risks moving real content between
/// speakers.
const BOUNDARY_FRAGMENT_MAX_WORDS: usize = 3;

/// Longest silence a moved fragment may sit from the words it joins. A
/// sentence the diarizer cut in two runs on without a pause; a fragment
/// two seconds away from the other side is more likely its own speaker's
/// words. Only checked when both pieces carry word timings.
const BOUNDARY_MOVE_MAX_GAP_MS: u64 = 2_000;

/// Whether `word` contains a sentence terminator anywhere (`.!?…`). Colons,
/// semicolons and dashes don't count: they are more often mid-clause than a
/// clean sentence boundary to split on.
fn word_has_terminator(word: &str) -> bool {
    word.chars().any(|c| matches!(c, '.' | '!' | '?' | '…'))
}

/// Whether `word` closes a sentence: its last character, after any closing
/// quote or bracket, is `.!?…`. Unlike `word_has_terminator` a decimal
/// (`3.5`) or an abbreviation's inner dot doesn't count.
fn word_ends_sentence(word: &str) -> bool {
    let core = word.trim_end_matches(['"', '\'', ')', ']', '»', '”', '’']);
    matches!(core.chars().last(), Some('.' | '!' | '?' | '…'))
}

fn starts_lowercase(word: &str) -> bool {
    word.chars().next().is_some_and(char::is_lowercase)
}

fn starts_uppercase(word: &str) -> bool {
    word.chars().next().is_some_and(char::is_uppercase)
}

/// A piece's text as whitespace tokens, when they can move in step with its
/// words: either the piece has no word timings at all, or exactly one token
/// per timed word. `None` when the two disagree (a provider's words that
/// don't line up with its transcript), and such a piece is left alone.
fn piece_tokens(piece: &LabelledPiece) -> Option<Vec<String>> {
    let tokens: Vec<String> = piece.text.split_whitespace().map(str::to_string).collect();
    if tokens.is_empty() {
        return None;
    }
    if piece.words.is_empty() || piece.words.len() == tokens.len() {
        Some(tokens)
    } else {
        None
    }
}

/// Stream-absolute start of a piece's first word / end of its last.
fn first_word_start_abs(piece: &LabelledPiece) -> Option<u64> {
    piece.words.first().map(|w| piece.base_ms.saturating_add(w.start_ms))
}

fn last_word_end_abs(piece: &LabelledPiece) -> Option<u64> {
    piece.words.last().map(|w| piece.base_ms.saturating_add(w.end_ms))
}

/// Whether two pieces' words, if they have timings, sit close enough for a
/// fragment to move between them (`BOUNDARY_MOVE_MAX_GAP_MS`). Untimed pieces
/// pass: there is nothing to measure, and the text cues alone decided before
/// timings existed.
fn within_move_gap(prev: &LabelledPiece, next: &LabelledPiece) -> bool {
    match (last_word_end_abs(prev), first_word_start_abs(next)) {
        (Some(end), Some(start)) => start.saturating_sub(end) <= BOUNDARY_MOVE_MAX_GAP_MS,
        _ => true,
    }
}

/// Re-express a piece's word timings against an earlier base, so words
/// from an earlier chunk can join it without a negative offset.
fn lower_base(piece: &mut LabelledPiece, new_base: u64) {
    if new_base >= piece.base_ms {
        return;
    }
    let shift = piece.base_ms - new_base;
    for w in &mut piece.words {
        w.start_ms += shift;
        w.end_ms += shift;
    }
    piece.base_ms = new_base;
}

/// `words` moved from a piece based at `from_base` to one based at
/// `to_base` (which `lower_base` has already made `<= from_base`).
fn rebased(words: Vec<ChunkWord>, from_base: u64, to_base: u64) -> Vec<ChunkWord> {
    let shift = from_base - to_base;
    words
        .into_iter()
        .map(|w| ChunkWord { start_ms: w.start_ms + shift, end_ms: w.end_ms + shift, ..w })
        .collect()
}

/// Move `prev`'s last `n` tokens (and their timed words) to the front of
/// `next`.
fn move_tail_forward(
    prev: &mut LabelledPiece,
    next: &mut LabelledPiece,
    prev_tokens: &[String],
    next_tokens: &[String],
    n: usize,
) {
    let keep = prev_tokens.len() - n;
    prev.text = prev_tokens[..keep].join(" ");
    next.text = prev_tokens[keep..]
        .iter()
        .chain(next_tokens.iter())
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    if !prev.words.is_empty() {
        let moved = prev.words.split_off(keep);
        lower_base(next, prev.base_ms);
        let mut words = rebased(moved, prev.base_ms, next.base_ms);
        words.append(&mut next.words);
        next.words = words;
    }
}

/// Move `next`'s first `n` tokens (and their timed words) to the end of
/// `prev`.
fn move_head_backward(
    prev: &mut LabelledPiece,
    next: &mut LabelledPiece,
    prev_tokens: &[String],
    next_tokens: &[String],
    n: usize,
) {
    prev.text = prev_tokens
        .iter()
        .chain(next_tokens[..n].iter())
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    next.text = next_tokens[n..].join(" ");
    if !next.words.is_empty() {
        let rest = next.words.split_off(n);
        let moved = std::mem::replace(&mut next.words, rest);
        lower_base(prev, next.base_ms);
        let mut words = rebased(moved, next.base_ms, prev.base_ms);
        prev.words.append(&mut words);
    }
}

/// Size of the fragment a forward move would hand from `prev` to `next`:
/// `prev` ends `<sentence>. <1–3 words>` whose first word is capitalised and
/// none of which closes a sentence, and `next` opens lowercase. Those
/// trailing words are the next speaker's sentence start.
fn forward_fragment(prev_tokens: &[String], next_tokens: &[String]) -> Option<usize> {
    let last_end = prev_tokens.iter().rposition(|t| word_ends_sentence(t))?;
    let trailing = &prev_tokens[last_end + 1..];
    if trailing.is_empty() || trailing.len() > BOUNDARY_FRAGMENT_MAX_WORDS {
        return None;
    }
    if !starts_uppercase(&trailing[0]) || trailing.iter().any(|t| word_has_terminator(t)) {
        return None;
    }
    if !starts_lowercase(next_tokens.first()?) {
        return None;
    }
    Some(trailing.len())
}

/// Size of the fragment a backward move would hand from `next` to `prev`:
/// `prev` is open (no terminator or dash at its end), and `next` opens
/// lowercase with `<1–3 words>.` followed by more of its own. Those leading
/// words close the previous speaker's sentence.
fn backward_fragment(prev: &LabelledPiece, next_tokens: &[String]) -> Option<usize> {
    if ends_sentence(&prev.text) || !starts_lowercase(next_tokens.first()?) {
        return None;
    }
    let first_end = next_tokens.iter().position(|t| word_ends_sentence(t))?;
    let n = first_end + 1;
    if n > BOUNDARY_FRAGMENT_MAX_WORDS || n >= next_tokens.len() {
        return None;
    }
    Some(n)
}

/// When both moves fit, whether the speaker was repeating themselves: the
/// phrase `prev`'s trailing words + `next`'s leading words already occurs in
/// `prev` before the trailing words ("Brand new. Brand" + "new. Hadn't…").
/// Then the leading words are the repeat's end and belong on `prev`.
fn repeats_across(
    prev_tokens: &[String],
    next_tokens: &[String],
    tail: usize,
    head: usize,
) -> bool {
    let fold = |t: &String| fold_word(t);
    let phrase: Vec<String> = prev_tokens[prev_tokens.len() - tail..]
        .iter()
        .chain(next_tokens[..head].iter())
        .map(fold)
        .collect();
    let earlier: Vec<String> = prev_tokens[..prev_tokens.len() - tail].iter().map(fold).collect();
    !phrase.is_empty() && earlier.windows(phrase.len()).any(|w| w == phrase.as_slice())
}

/// Hand a sentence's stray first or last words across a speaker change to the
/// side they belong to ("…the perfect way. It" / "does pay off."), with their
/// timings: neighbouring pieces of one stream, both timed or both untimed, at
/// most `BOUNDARY_FRAGMENT_MAX_WORDS` words within `BOUNDARY_MOVE_MAX_GAP_MS`.
/// Letter case licenses the move, so scripts without case never match. When
/// both directions fit, the smaller move wins; a repetition (`repeats_across`)
/// picks backward, an even split forward.
pub(super) fn realign_sentence_boundaries(pieces: &mut [LabelledPiece]) {
    for i in 0..pieces.len().saturating_sub(1) {
        let (left, right) = pieces.split_at_mut(i + 1);
        let prev = &mut left[i];
        let next = &mut right[0];
        match (&prev.label, &next.label) {
            (Some(a), Some(b)) if a != b => {}
            _ => continue,
        }
        if prev.source != next.source || prev.words.is_empty() != next.words.is_empty() {
            continue;
        }
        let (Some(prev_tokens), Some(next_tokens)) = (piece_tokens(prev), piece_tokens(next)) else {
            continue;
        };
        if !within_move_gap(prev, next) {
            continue;
        }
        let forward = forward_fragment(&prev_tokens, &next_tokens);
        let backward = backward_fragment(prev, &next_tokens);
        match (forward, backward) {
            (Some(tail), Some(head)) => {
                if head < tail || repeats_across(&prev_tokens, &next_tokens, tail, head) {
                    move_head_backward(prev, next, &prev_tokens, &next_tokens, head);
                } else {
                    move_tail_forward(prev, next, &prev_tokens, &next_tokens, tail);
                }
            }
            (Some(tail), None) => move_tail_forward(prev, next, &prev_tokens, &next_tokens, tail),
            (None, Some(head)) => move_head_backward(prev, next, &prev_tokens, &next_tokens, head),
            (None, None) => {}
        }
    }
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
            source: ChunkSource::Sys,
            base_ms: 0,
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
            source: ChunkSource::Sys,
            base_ms: 0,
        }
    }

    /// A timed piece whose words run 300 ms apart from `start_ms`, which is
    /// stream-absolute: the piece is based at `base_ms` (its chunk's start)
    /// and its words are stored relative to that, the way `split_by_labels`
    /// builds them.
    fn timed(label: &str, text: &str, base_ms: u64, start_ms: u64) -> LabelledPiece {
        let words = text
            .split_whitespace()
            .enumerate()
            .map(|(i, w)| crate::recording::ChunkWord {
                text: w.to_string(),
                start_ms: start_ms - base_ms + 300 * i as u64,
                end_ms: start_ms - base_ms + 300 * i as u64 + 250,
            })
            .collect();
        LabelledPiece {
            label: Some(label.to_string()),
            text: text.to_string(),
            words,
            source: ChunkSource::Sys,
            base_ms,
        }
    }

    /// Stream-absolute (text, start) of a piece's words, for asserting that a
    /// move kept every word's timing.
    fn abs_words(p: &LabelledPiece) -> Vec<(String, u64)> {
        p.words.iter().map(|w| (w.text.clone(), p.base_ms + w.start_ms)).collect()
    }

    fn labels(pieces: &[LabelledPiece]) -> Vec<Option<String>> {
        pieces.iter().map(|p| p.label.clone()).collect()
    }

    fn texts(pieces: &[LabelledPiece]) -> Vec<String> {
        pieces.iter().map(|p| p.text.clone()).collect()
    }

    #[test]
    fn forward_move_hands_sentence_start_to_next_speaker() {
        // Real-recording case from a tester transcript: the next
        // speaker's first word ("It") landed on the previous turn
        // because the diarizer's boundary fell a word late. The
        // continuation ("does pay off") is lowercase, confirming "It"
        // starts a sentence the next speaker delivers.
        let mut pieces = vec![
            timed("Speaker 1", "starts to unravel in the perfect way. It", 0, 1_000),
            timed("Speaker 2", "does pay off. That's what's really nice.", 0, 3_200),
        ];
        let it_at = abs_words(&pieces[0]).last().unwrap().1;
        realign_sentence_boundaries(&mut pieces);
        assert_eq!(
            texts(&pieces),
            vec![
                "starts to unravel in the perfect way.",
                "It does pay off. That's what's really nice."
            ]
        );
        assert_eq!(labels(&pieces), vec![Some("Speaker 1".into()), Some("Speaker 2".into())]);
        // The word moved with its timing, so playback still lights it when
        // it is spoken.
        assert_eq!(abs_words(&pieces[1])[0], ("It".to_string(), it_at));
        assert_eq!(pieces[0].words.len(), 7);
        assert_eq!(pieces[1].words.len(), 8);
    }

    #[test]
    fn forward_move_handles_multi_word_fragment() {
        let mut pieces = vec![
            timed("Speaker 1", "that closes the door. So we", 0, 0),
            timed("Speaker 2", "can move on now.", 0, 1_800),
        ];
        realign_sentence_boundaries(&mut pieces);
        assert_eq!(texts(&pieces), vec!["that closes the door.", "So we can move on now."]);
    }

    #[test]
    fn forward_move_skips_when_trailing_starts_lowercase() {
        // "that" continues the previous clause; it isn't a new sentence's
        // start. No move here; the chain rule decides the speaker instead.
        let mut pieces = vec![
            timed("Speaker 1", "I think that", 0, 0),
            timed("Speaker 2", "we should do it.", 0, 900),
        ];
        let before = texts(&pieces);
        realign_sentence_boundaries(&mut pieces);
        assert_eq!(texts(&pieces), before);
    }

    #[test]
    fn backward_move_pulls_sentence_close_to_prev_speaker() {
        // Real-recording case: "AMC was a young company. Brand new.
        // Brand" + "new. Hadn't done anything..." — the diarizer cut
        // mid-utterance and gave "new." to the wrong speaker. Both moves
        // fit; the repetition ("Brand new" already said) picks backward.
        let mut pieces = vec![
            timed("Speaker 1", "AMC was a young company. Brand new. Brand", 0, 0),
            timed("Speaker 2", "new. Hadn't done anything significant yet.", 0, 2_400),
        ];
        realign_sentence_boundaries(&mut pieces);
        assert_eq!(
            texts(&pieces),
            vec![
                "AMC was a young company. Brand new. Brand new.",
                "Hadn't done anything significant yet."
            ]
        );
        assert_eq!(abs_words(&pieces[0]).last().unwrap(), &("new.".to_string(), 2_400));
    }

    #[test]
    fn backward_move_skips_when_nothing_follows_the_fragment() {
        // Moving "stop." would empty the next piece; that's the chain
        // rule's case (one sentence, one speaker), not a boundary move.
        let mut pieces = vec![
            timed("Speaker 1", "I think we should", 0, 0),
            timed("Speaker 2", "stop.", 0, 1_200),
        ];
        let before = texts(&pieces);
        realign_sentence_boundaries(&mut pieces);
        assert_eq!(texts(&pieces), before);
    }

    #[test]
    fn backward_move_skips_a_capitalised_reply() {
        // "Yes." after an open question is the other speaker answering,
        // not the end of the question. Only a lowercase opening licenses
        // the backward move.
        let mut pieces = vec![
            timed("Speaker 1", "do you think we should", 0, 0),
            timed("Speaker 2", "Yes. I agree with that.", 0, 1_500),
        ];
        let before = texts(&pieces);
        realign_sentence_boundaries(&mut pieces);
        assert_eq!(texts(&pieces), before);
    }

    #[test]
    fn backward_move_skips_when_fragment_too_long() {
        // A 4-word leading fragment exceeds BOUNDARY_FRAGMENT_MAX_WORDS.
        let mut pieces = vec![
            timed("Speaker 1", "I was about to", 0, 0),
            timed("Speaker 2", "yes that is correct. Anyway moving on.", 0, 1_200),
        ];
        let before = texts(&pieces);
        realign_sentence_boundaries(&mut pieces);
        assert_eq!(texts(&pieces), before);
    }

    #[test]
    fn boundary_move_skips_same_speaker_other_stream_long_gap_and_mixed_timing() {
        let pair = || {
            vec![
                timed("Speaker 1", "in the perfect way. It", 0, 0),
                timed("Speaker 2", "does pay off.", 0, 1_800),
            ]
        };

        let mut same = pair();
        same[1].label = Some("Speaker 1".into());
        realign_sentence_boundaries(&mut same);
        assert_eq!(same[0].text, "in the perfect way. It");

        let mut streams = pair();
        streams[1].source = ChunkSource::Mic;
        realign_sentence_boundaries(&mut streams);
        assert_eq!(streams[0].text, "in the perfect way. It");

        // Five seconds of silence between "It" and "does": not one sentence.
        let mut gap = vec![
            timed("Speaker 1", "in the perfect way. It", 0, 0),
            timed("Speaker 2", "does pay off.", 0, 6_500),
        ];
        realign_sentence_boundaries(&mut gap);
        assert_eq!(gap[0].text, "in the perfect way. It");

        let mut mixed = pair();
        mixed[1].words.clear();
        realign_sentence_boundaries(&mut mixed);
        assert_eq!(mixed[0].text, "in the perfect way. It");
    }

    #[test]
    fn boundary_move_without_word_timings_moves_text_only() {
        // OpenAI's gpt-4o family returns no word timings; the text cue is
        // still there and the fix still applies to the transcript.
        let mut pieces = vec![
            piece(Some("Speaker 1"), "in the perfect way. It", false),
            piece(Some("Speaker 2"), "does pay off.", false),
        ];
        realign_sentence_boundaries(&mut pieces);
        assert_eq!(texts(&pieces), vec!["in the perfect way.", "It does pay off."]);
        assert!(pieces[0].words.is_empty() && pieces[1].words.is_empty());
    }

    #[test]
    fn boundary_move_across_chunks_keeps_absolute_timings() {
        // The fragment sits in an earlier chunk (based at 0) than the
        // piece it joins (based at 15 s). The receiving piece's base drops
        // to the earlier chunk's start and every word keeps its time.
        let mut pieces = vec![
            timed("Speaker 1", "we close it here. It", 0, 12_000),
            timed("Speaker 2", "does pay off now.", 15_000, 15_100),
        ];
        let before: Vec<(String, u64)> = abs_words(&pieces[0])
            .into_iter()
            .chain(abs_words(&pieces[1]))
            .collect();
        realign_sentence_boundaries(&mut pieces);
        assert_eq!(texts(&pieces), vec!["we close it here.", "It does pay off now."]);
        assert_eq!(pieces[1].base_ms, 0);
        let after: Vec<(String, u64)> = abs_words(&pieces[0])
            .into_iter()
            .chain(abs_words(&pieces[1]))
            .collect();
        assert_eq!(before, after);
    }

    #[test]
    fn boundary_move_prefers_the_smaller_correction() {
        // Both moves fit: three words forward ("Men hva med" opens Speaker
        // 2's turn) or one word backward ("budsjettet?" closes Speaker 1's
        // question). The diarizer is usually off by a word, not three, and
        // a question answered by the other speaker is the likelier reading.
        let mut pieces = vec![
            timed("Speaker 1", "Vi gjør det sånn. Men hva med", 0, 0),
            timed("Speaker 2", "budsjettet? Det er ganske stramt i år, synes jeg.", 0, 2_400),
        ];
        realign_sentence_boundaries(&mut pieces);
        assert_eq!(
            texts(&pieces),
            vec!["Vi gjør det sånn. Men hva med budsjettet?", "Det er ganske stramt i år, synes jeg."]
        );
    }

    #[test]
    fn refine_moves_the_fragment_instead_of_merging_the_turns() {
        // The case behind "the first words of a sentence belong to the
        // previous speaker": before the boundary move ran ahead of the
        // chain rule, "It" left Speaker 1's piece open, the lowercase
        // "does pay off" continued it, and the chain rule handed the whole
        // of one turn to the other speaker.
        let mut pieces = vec![
            timed("Speaker 1", "Det løser seg på den perfekte måten. Det", 0, 0),
            timed("Speaker 2", "lønner seg til slutt, og det er det fine med det.", 0, 2_400),
            timed("Speaker 1", "Ja, det vet jeg.", 0, 6_500),
        ];
        refine_pieces(&mut pieces);
        assert_eq!(
            texts(&pieces),
            vec![
                "Det løser seg på den perfekte måten.",
                "Det lønner seg til slutt, og det er det fine med det.",
                "Ja, det vet jeg."
            ]
        );
        assert_eq!(
            labels(&pieces),
            vec![Some("Speaker 1".into()), Some("Speaker 2".into()), Some("Speaker 1".into())]
        );
    }

    #[test]
    fn display_words_take_the_transcripts_punctuation_and_casing() {
        // Deepgram's `word` and whisper-1's verbose words come bare; the
        // chunk's transcript has the punctuation and capitals.
        let mut c = sys_with_words(
            0,
            vec![("hello", 0, 300), ("world", 300, 600), ("how", 900, 1_100), ("are", 1_100, 1_300), ("you", 1_300, 1_500)],
        );
        c.text = "Hello, world. How are you?".into();
        let shown: Vec<String> = display_words(&c).into_iter().map(|w| w.text).collect();
        assert_eq!(shown, vec!["Hello,", "world.", "How", "are", "you?"]);
    }

    #[test]
    fn display_words_keep_a_word_the_transcript_spells_differently() {
        // Smart formatting can turn "four point two" into "4.2" in the
        // transcript; those words have no match and keep their own text,
        // and the words after them still line up.
        let mut c = sys_with_words(
            0,
            vec![("churn", 0, 200), ("is", 200, 300), ("four", 300, 500), ("point", 500, 700), ("two", 700, 900), ("percent", 900, 1_200), ("now", 1_200, 1_400)],
        );
        c.text = "Churn is 4.2 percent now.".into();
        let shown: Vec<String> = display_words(&c).into_iter().map(|w| w.text).collect();
        assert_eq!(shown, vec!["Churn", "is", "four", "point", "two", "percent", "now."]);
    }

    #[test]
    fn display_words_leave_local_whisper_words_alone() {
        let c = sys_with_words(0, vec![("Hei,", 0, 200), ("du.", 200, 400)]);
        let shown: Vec<String> = display_words(&c).into_iter().map(|w| w.text).collect();
        assert_eq!(shown, vec!["Hei,", "du."]);
    }

    #[test]
    fn transcript_from_bare_words_keeps_punctuation() {
        let mut c = sys_with_words(0, vec![("hello", 0, 300), ("world", 300, 600)]);
        c.text = "Hello world.".into();
        let segments = vec![diarize::Segment { start_ms: 0, end_ms: 1_000, speaker_id: "a".into() }];
        let labels: std::collections::HashMap<String, String> =
            [("a".to_string(), "Speaker 1".to_string())].into();
        let split = move |ch: &ChunkRecord| split_by_labels(ch, &segments, &labels);
        assert_eq!(build_labelled_transcript(&[c], &split), "Speaker 1: Hello world.");
    }

    #[test]
    fn continuation_chain_stops_at_a_dash() {
        // A speaker trailing off or interrupted ends with a dash; the next
        // speaker's lowercase reply is their own turn, not the rest of
        // the sentence.
        for cut in ["But what about the —", "I was going to say –", "Jeg ska-"] {
            let mut pieces = vec![
                piece(Some("Speaker 1"), cut, true),
                piece(Some("Speaker 2"), "right but we have no time.", true),
            ];
            absorb_text_continuation_chains(&mut pieces);
            assert_eq!(pieces[1].label.as_deref(), Some("Speaker 2"), "{cut}");
        }
    }

    #[test]
    fn continuation_chain_breaks_a_tie_by_who_spoke_first() {
        // Two labels with the same word count and no agreeing neighbours:
        // the first one in the chain wins, every run.
        for _ in 0..20 {
            let mut pieces = vec![
                piece(Some("Speaker 2"), "Ja, jeg", true),
                piece(Some("Speaker 1"), "skjønner det", true),
            ];
            absorb_text_continuation_chains(&mut pieces);
            assert_eq!(labels(&pieces), vec![Some("Speaker 2".into()), Some("Speaker 2".into())]);
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
