//! Transcribing one VAD chunk and the guards around it: subtitle-attribution
//! tails, silence hallucinations, repetition loops, collapsed word timings on an
//! incidental stream, and the length-weighted vote on the recording's language.

use super::api_keys::read_provider_api_key;
use super::models::local_model_path;
use super::transcription_config::read_transcribe_config;
use super::transcript::{normalize_tokens, token_jaccard};
use super::diarization::INCIDENTAL_STREAM_CHUNK_SHARE;
use super::{DEFAULT_LANGUAGE, DEFAULT_SILENCE_RMS_THRESHOLD};
use crate::recording::{CaptureSink, ChunkRecord, ChunkSource, TranscriptPayload};
use crate::{db, wav, AppState};
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};

pub(super) async fn transcribe_chunk(
    app: AppHandle,
    note_id: String,
    source: ChunkSource,
    path: PathBuf,
    start_ms: u64,
    // This chunk's destination (#146). Passed in rather than read from
    // `state.recording`, so a deferred replay of an old take can run its
    // chunks through the same pipeline without occupying the live-capture
    // slot — and can't have its log cleared out from under it by a recording
    // the user starts meanwhile.
    sink: Arc<CaptureSink>,
) -> anyhow::Result<()> {
    // Resolve dispatch-time data: per-note-or-global language first
    // (drives per-language override resolution), then the resolved
    // provider config, then custom vocabulary + API key. The order
    // matters — provider_cfg depends on language for the resolve.
    let (language, vocabulary, org_terms) = {
        let state: State<AppState> = app.state();
        let conn = state.db.lock();
        let global_language = db::get_setting(&conn, "language")?
            .unwrap_or_else(|| DEFAULT_LANGUAGE.to_string());
        let note_language = db::get_note(&conn, &note_id)
            .map(|n| n.language)
            .unwrap_or_default();
        let language = if note_language.trim().is_empty() {
            global_language
        } else {
            note_language
        };
        let vocabulary = db::get_setting(&conn, "custom_vocabulary")?
            .unwrap_or_default();
        (language, vocabulary, crate::org_policy::vocabulary(&conn))
    };
    let provider_cfg = {
        let state: State<AppState> = app.state();
        read_transcribe_config(&state)?.resolve(&language).clone()
    };
    {
        let state: State<AppState> = app.state();
        let conn = state.db.lock();
        super::transcription_config::check(&conn, &provider_cfg)?;
    }
    // Each provider's key has its own Keychain slot; a custom endpoint may
    // take no key at all.
    let api_key = match provider_cfg.provider() {
        crate::providers::ProviderId::Local => None,
        provider @ crate::providers::ProviderId::Custom => {
            let state: State<AppState> = app.state();
            read_provider_api_key(&state, provider.as_str()).map_err(|e| anyhow::anyhow!("{e}"))?
        }
        provider => {
            let provider_id = provider.as_str();
            let state: State<AppState> = app.state();
            Some(
                read_provider_api_key(&state, provider_id)
                    .map_err(|e| anyhow::anyhow!("{e}"))?
                    .ok_or_else(|| {
                        anyhow::anyhow!("no API key stored for provider '{provider_id}'")
                    })?,
            )
        }
    };
    let custom_base_url = match &provider_cfg {
        crate::stt::ProviderConfig::Custom(_) => {
            let state: State<AppState> = app.state();
            let conn = state.db.lock();
            Some(crate::custom_endpoint::base_url(&conn)?)
        }
        _ => None,
    };

    // Skip near-silent chunks. Whisper and gpt-4o-transcribe both hallucinate
    // confident text (often in the wrong language) when fed silence. The WAV
    // chunks are 16kHz mono 16-bit PCM little-endian — read the data section
    // and compute RMS in [0, 1]. Threshold is user-tunable so noisy
    // environments (HVAC, mic hiss) can crank it up to drop borderline
    // chunks before they reach Whisper.
    let rms_floor = {
        let state: State<AppState> = app.state();
        let conn = state.db.lock();
        db::get_setting(&conn, "silence_rms_threshold")
            .ok()
            .flatten()
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or(DEFAULT_SILENCE_RMS_THRESHOLD)
    };
    if let Ok(rms) = wav::rms(&path).await {
        // Pure silence ~0.0001, room tone ~0.001, mic hiss ~0.003,
        // soft speech 0.005+. Threshold doc lives at the constant
        // (DEFAULT_SILENCE_RMS_THRESHOLD) — see top of file.
        if rms < rms_floor {
            return Ok(());
        }
    }

    // Serialize transcription per session: each chunk's initial_prompt must
    // see the *committed* trail of every prior chunk. With parallel
    // transcribes, two back-to-back chunks both grab the same stale snapshot
    // and the trail's quality benefit collapses. Sequential trades a little
    // throughput (chunks queue if inference is slow) for accurate context.
    let gate = {
        let state: State<AppState> = app.state();
        state.transcribe_gate.clone()
    };
    let _guard = gate.lock().await;

    // Whisper's `initial_prompt` slot conditions decoding on prior context.
    // We compose two parts: the user's custom vocabulary (proper-noun bias)
    // and a snapshot of the last ~150 committed words from THIS source's
    // stream. Per-source trails because the mic and system streams are
    // separate conversations — sharing one trail would pull a mic chunk's
    // decode toward remote-side vocabulary (or vice versa) and cause
    // language drift on bilingual calls.
    let trail_snapshot = sink.trail(source).lock().as_prompt();
    let vocab_owned = merge_vocabulary(&org_terms, &vocabulary, VocabCap::for_provider(provider_cfg.provider()));
    let vocab_terms: Vec<&str> = vocab_owned.iter().map(String::as_str).collect();

    // Build the right STT adapter for this chunk. Local Whisper needs
    // runtime state (shared model context, resolved model file path, GPU
    // flag) that can't live in `ProviderConfig`; we resolve it here and
    // pass it to the registry. Both providers feed the same `Word` shape
    // downstream so the timeline serialiser can rebase chunk-relative ms
    // onto the playback clock the same way regardless of source.
    let local_deps = if let crate::stt::ProviderConfig::Local(local_cfg) = &provider_cfg {
        let model_path = local_model_path(&app, &language, &local_cfg.model_id)
            .map_err(|e| anyhow::anyhow!(e))?;
        let shared = {
            let state: State<AppState> = app.state();
            state.whisper.clone()
        };
        Some(crate::stt::LocalDeps {
            shared,
            model_path,
            use_gpu: local_cfg.use_gpu,
        })
    } else {
        None
    };
    let adapter = crate::stt::build_adapter(&provider_cfg, local_deps);
    let ctx = crate::stt::TranscribeCtx {
        model: provider_cfg.model(),
        language: &language,
        bias_terms: &vocab_terms,
        prior_context: trail_snapshot.as_deref(),
        api_key: api_key.as_deref(),
        base_url: custom_base_url.as_deref().or(provider_cfg.base_url()),
    };
    let crate::stt::TranscribeResult { text, words, detected_language } =
        adapter.transcribe(ctx, &path).await?;
    if is_likely_hallucination(&text, &language) {
        return Ok(());
    }
    // NOTE: no word-timing collapse check here, deliberately. Degenerate
    // timings are common in real speech from the local Whisper models, so the
    // signal is only usable together with "this stream is statistically noise"
    // — and mid-recording we don't yet know each stream's share of the whole.
    // That filter therefore runs post-stop, in
    // `drop_incidental_stream_hallucinations`.
    // Drop chunks dominated by N-gram repetition (Whisper collapse). Letting
    // them land in the transcript is bad on its own, but worse: the trail-
    // prompt feeds the loop forward into the next chunk's `initial_prompt`
    // and the loop self-sustains for the rest of the recording.
    if is_repetition_collapse(&text) {
        eprintln!("transcribe: dropping repetition-collapsed chunk");
        return Ok(());
    }
    // Whisper was trained on closed-caption data and frequently appends
    // subtitle attribution ("Undertekster av Ai-Media", "Subtitles by Amara",
    // "Thanks for watching") at the end of real speech. Trim those tails.
    let text = strip_attribution_tail(&text);
    let trimmed = text.trim().to_string();
    if trimmed.is_empty() {
        return Ok(());
    }

    // Cross-chunk hallucination loop guard — MIC ONLY. Whisper
    // occasionally locks onto a confident-sounding phrase when the
    // audio is low-SNR (e.g. an internal MacBook mic during silence
    // between sentences). Each chunk on its own passes the per-chunk
    // repetition-collapse filter because the phrase appears only
    // once internally — but the same phrase comes back chunk after
    // chunk, fed forward by the trail context.
    //
    // We use Jaccard similarity (|A ∩ B| / |A ∪ B|) at a strict 0.85
    // threshold so legitimate continuations survive: a chunk with
    // even a few new unique words drops well below 0.85, while exact
    // / near-exact repeats from a hallucination loop score ≥0.95.
    // Earlier iteration used containment with min-denominator and a
    // 0.7 threshold; that caught loops but also dropped real content
    // when chunk N+1 was a strict superset of chunk N.
    //
    // Sys excluded entirely: clean source audio rarely hallucinates,
    // and consecutive sys chunks legitimately share lots of
    // vocabulary on continuing topics.
    if source == ChunkSource::Mic {
        let recent_same_source = {
            let log = sink.chunk_log.lock();
            log.iter()
                .rev()
                .find(|c| c.source == source)
                .map(|c| c.text.clone())
        };
        if let Some(prev) = recent_same_source {
            let new_tokens = normalize_tokens(&trimmed);
            if new_tokens.len() >= 3 {
                let prev_tokens = normalize_tokens(&prev);
                if token_jaccard(&new_tokens, &prev_tokens) >= 0.85 {
                    eprintln!(
                        "transcribe: dropping cross-chunk repeat (likely hallucination loop): {trimmed}"
                    );
                    return Ok(());
                }
            }
        }
    }

    // Speaker prefixes are added by the offline diarization pass on
    // recording_stop, not here. Per-chunk live diarization performed
    // poorly on long recordings (clustering drifts as speaker memory
    // accumulates), so chunks are appended as plain text and the full
    // transcript is rewritten with proper labels after stop, when
    // FluidAudio can cluster across the entire audio at once and we can
    // assign "You" to mic chunks vs diarized speakers to system chunks.
    //
    // The live-display transcript appends in arrival order regardless of
    // source. Mic and sys chunks may interleave slightly out of strict
    // wall-clock order during recording, but `diarize_and_apply` rebuilds
    // the transcript from the chunk log sorted by (source, start_ms) at
    // stop time, so the saved transcript ends up properly ordered.
    let state: State<AppState> = app.state();

    // Push to chunk_log unconditionally — even when the session has
    // been cleared by recording_stop. The post-stop chain's
    // diarize_and_apply rebuilds the transcript from chunk_log, so a
    // tail chunk that finished decoding after recording_stop still gets
    // included. Without this, the last 5–20 s of audio (any in-flight
    // transcribe completing after stop) would silently vanish — the
    // bug user reported.
    //
    // Words come from local Whisper only and arrive with chunk-
    // relative timestamps (whisper timed against this chunk's WAV,
    // which starts at t=0 from its own perspective). The playback
    // view adds chunk.start_ms back when it needs absolute time.
    let chunk_words: Vec<crate::recording::ChunkWord> = words
        .into_iter()
        .map(|w| crate::recording::ChunkWord {
            text: w.text,
            start_ms: w.start_ms,
            end_ms: w.end_ms,
        })
        .collect();
    sink.chunk_log.lock().push(ChunkRecord {
        source,
        start_ms,
        text: trimmed.clone(),
        words: chunk_words,
        detected_language,
    });

    // A deferred replay stops here (#146). Its text reaches the note only
    // through the diarize pass, which rewrites `note.transcript` from every
    // session's timeline — so a replay that dies half-way leaves no text
    // behind that no timeline accounts for (ADR-0004). The trail still
    // advances: the next chunk decodes better for having seen this one.
    if !sink.streams_to_note() {
        sink.trail(source).lock().push(&trimmed);
        return Ok(());
    }

    // Live-update guard. The provider call above (whisper / openai) can take
    // long enough that recording_stop fires while we're still awaiting it.
    // Through the stop's drain the append still happens — the tail of a
    // meeting arrives line by line instead of in one block minutes later
    // (#182). It stops at the `chunk_log` snapshot: from there the post-stop
    // chain owns `note.transcript` and rewrites the whole string with speaker
    // labels, so a late append would land on top of that. The chunk is in the
    // log either way, so nothing is lost by skipping.
    {
        let session = state.recording.lock();
        if !crate::recording::appends_live(session.note_id.as_deref(), session.stop, &note_id) {
            eprintln!(
                "transcribe: session inactive, chunk preserved in log for post-stop"
            );
            return Ok(());
        }
    }
    let updated_transcript = {
        let conn = state.db.lock();
        db::append_transcript(&conn, &note_id, &trimmed, " ")?
    };
    sink.trail(source).lock().push(&trimmed);
    let _ = app.emit(
        "transcript_replaced",
        TranscriptPayload {
            note_id: note_id.clone(),
            text: updated_transcript,
        },
    );
    Ok(())
}

// Whisper's training data contained millions of subtitle files, so it
// regularly appends "Subtitles by …" / "Undertekster av …" / "Thanks for
// watching" at the end of real speech. If we see one of these markers
// anywhere in the text, strip it back to the preceding sentence boundary.
/// How much vocabulary a provider takes. Deepgram counts terms; Whisper-shaped
/// providers keep only the tail of a ~224-token prompt, which would drop the
/// front of the list, so the list is cut to a length that fits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VocabCap {
    Terms(usize),
    Chars(usize),
}

impl VocabCap {
    fn for_provider(id: crate::providers::ProviderId) -> Self {
        match id {
            crate::providers::ProviderId::Deepgram => VocabCap::Terms(100),
            _ => VocabCap::Chars(500),
        }
    }
}

/// The terms sent to the transcriber: the organisations' first, then the
/// user's `custom_vocabulary` (newline- or comma-separated), without duplicates
/// regardless of case, cut to `cap` so the user's terms go before the
/// organisation's. Terms under three characters are dropped: every provider's
/// biaser turns them into false positives. Without organisation terms the
/// user's list is used exactly as stored.
fn merge_vocabulary(org: &[String], user: &str, cap: VocabCap) -> Vec<String> {
    let usable = |t: &&str| t.len() >= 3;
    let user = user.split(['\n', ',']).map(str::trim).filter(usable);
    let org: Vec<&str> = org.iter().map(|t| t.trim()).filter(usable).collect();
    if org.is_empty() {
        return user.map(String::from).collect();
    }
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::new();
    let mut chars = 0usize;
    for term in org.into_iter().chain(user) {
        if !seen.insert(term.to_lowercase()) {
            continue;
        }
        let added = term.chars().count() + if out.is_empty() { 0 } else { 2 };
        let fits = match cap {
            VocabCap::Terms(n) => out.len() < n,
            VocabCap::Chars(n) => chars + added <= n,
        };
        if !fits {
            break;
        }
        chars += added;
        out.push(term.to_string());
    }
    out
}

#[cfg(test)]
mod vocabulary_tests {
    use super::*;

    fn org(terms: &[&str]) -> Vec<String> {
        terms.iter().map(|t| t.to_string()).collect()
    }

    #[test]
    fn without_organisation_terms_the_users_list_is_unchanged() {
        let user = "Humla, Tauri\nTauri, ab, humla";
        assert_eq!(
            merge_vocabulary(&[], user, VocabCap::Terms(1)),
            ["Humla", "Tauri", "Tauri", "humla"],
            "no cap and no dedup: what transcribe_chunk sent before"
        );
        assert_eq!(merge_vocabulary(&org(&["ab"]), user, VocabCap::Chars(5)).len(), 4);
    }

    #[test]
    fn organisation_terms_come_first_without_duplicates() {
        assert_eq!(
            merge_vocabulary(&org(&["Northwind", "Fjord"]), "fjord, Humla\nNORTHWIND, Humla", VocabCap::Terms(100)),
            ["Northwind", "Fjord", "Humla"]
        );
    }

    #[test]
    fn a_term_cap_cuts_the_users_terms_first() {
        let many: Vec<String> = (0..98).map(|i| format!("org{i}")).collect();
        let merged = merge_vocabulary(&many, "alpha, beta, gamma", VocabCap::Terms(100));
        assert_eq!(merged.len(), 100);
        assert_eq!(&merged[98..], ["alpha", "beta"]);
    }

    #[test]
    fn a_length_cap_cuts_the_users_terms_first() {
        let merged = merge_vocabulary(&org(&["Northwind", "Fjord"]), "Humla, Tauri", VocabCap::Chars(25));
        assert_eq!(merged, ["Northwind", "Fjord", "Humla"], "Northwind, Fjord, Humla is 23 characters");
        assert_eq!(merged.join(", ").len(), 23);
    }

    #[test]
    fn deepgram_counts_terms_and_whisper_counts_characters() {
        use crate::providers::ProviderId;
        assert_eq!(VocabCap::for_provider(ProviderId::Deepgram), VocabCap::Terms(100));
        for id in [ProviderId::OpenAi, ProviderId::Local, ProviderId::Groq, ProviderId::Custom] {
            assert_eq!(VocabCap::for_provider(id), VocabCap::Chars(500));
        }
    }
}

fn strip_attribution_tail(text: &str) -> String {
    // Triggers are ASCII so to_ascii_lowercase keeps byte offsets aligned
    // with the original string for slicing.
    let lower = text.to_ascii_lowercase();
    const TRIGGERS: &[&str] = &[
        // Norwegian/Scandinavian subtitle credits. Whisper memorised whole
        // sign-off phrases from broadcast subtitles, so each verb form needs
        // its own trigger — past-participle ("tekstet"), gerund ("teksting"),
        // and noun form ("tekster") all show up in the wild.
        "undertekster av",
        "undertekstet av",
        "tekstet av",
        "tekster av",
        "teksting av",
        "norske tekster",
        "oversatt av",
        "oversettelse av",
        // English subtitle credits
        "subtitles by",
        "subtitled by",
        "captions by",
        "captioning by",
        "closed captions",
        "translation by",
        "translated by",
        "transcribed by",
        "amara.org",
        "ai-media",
        // YouTube-style sign-offs
        "thanks for watching",
        "thank you for watching",
        "subscribe to",
        "like and subscribe",
        "see you next time",
        "see you in the next",
    ];
    let mut cut: Option<usize> = None;
    for trigger in TRIGGERS {
        if let Some(pos) = lower.rfind(trigger) {
            // Back up to the nearest sentence boundary before the trigger so
            // we drop the whole offending phrase, not just the trigger word.
            let start = text[..pos]
                .rfind(['.', '!', '?', '\n'])
                .map(|p| p + 1)
                .unwrap_or(pos);
            cut = Some(cut.map_or(start, |c| c.min(start)));
        }
    }
    match cut {
        Some(c) => text[..c].trim_end().to_string(),
        None => text.to_string(),
    }
}

// Whisper produces a small set of stock English phrases when fed silence
// regardless of the `language` parameter. Drop them when:
//   - the chunk is short (≤120 chars, typical of a hallucinated standalone
//     phrase) AND
//   - the chosen target language is not English (so we don't eat a real
//     English meeting that happens to say "thanks for watching this demo").
// We err on the side of keeping content; the silence gate above is the
// primary defense.
fn is_likely_hallucination(text: &str, _language: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return true;
    }
    // Punctuation-only output is always Whisper hallucinating on
    // silence: ".", "...", "*", standalone emoji, etc.
    if !t.chars().any(|c| c.is_alphanumeric()) {
        return true;
    }
    // Long output is real speech (or a real loop, which the
    // repetition-collapse filter handles separately).
    if t.len() > 120 {
        return false;
    }
    let lower = t.to_lowercase();
    // Caption-attribution patterns. Substring match because these
    // sometimes appear glued to the tail of real speech.
    const ATTRIBUTION_FRAGMENTS: &[&str] = &[
        "thanks for watching",
        "thank you for watching",
        "subscribe to",
        "subtitles by",
        "subtitled by",
        "amara.org",
        "transcribed by",
    ];
    if ATTRIBUTION_FRAGMENTS.iter().any(|f| lower.contains(f)) {
        return true;
    }
    // Short single-utterance silence hallucinations across major
    // languages. Whisper falls back to high-prior greeting / thanks
    // tokens when fed low-SNR audio; the user reported "Hei.",
    // "Takk!", "Hi." appearing repeatedly between real speech with
    // no audio actually containing those words. Match the
    // punctuation-stripped, whitespace-collapsed lowercase form
    // exactly so longer sentences containing these words pass
    // through unaffected.
    //
    // Deliberately NOT in the drop list: "yes/no/ja/nei/oui/non/ok"
    // — those are common real one-word answers, and dropping them
    // is more disruptive than the rare hallucination they'd catch.
    let normalized: String = t
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    const SHORT_HALLUCINATIONS: &[&str] = &[
        // EN greetings / thanks
        "hi", "hello", "bye", "thanks", "thank you", "okay", "yeah",
        // EN backchannels Whisper hallucinates on noise
        "mhm", "mhmm", "uh huh", "uhhuh", "mm", "mmm", "hm", "hmm",
        "ah", "oh", "right", "you",
        // NO/DA/SV
        "hei", "hej", "hallo", "takk", "tak", "tack", "ha det",
        // NO backchannels
        "ja vel", "mhm",
        // DE
        "danke", "tschuss", "tschüss",
        // FR
        "merci", "bonjour", "au revoir", "salut",
        // ES
        "hola", "gracias", "adios",
        // JA
        "ありがとう", "こんにちは", "さようなら",
    ];
    SHORT_HALLUCINATIONS.contains(&normalized.as_str())
}

/// Detect a chunk whose output is dominated by N-gram repetition — Whisper's
/// well-known low-SNR failure mode where one phrase decodes ≥3 consecutive
/// times. The contaminated chunk should be dropped; if it lands in the
/// transcript, the trail-prompt mechanism then feeds the loop into the next
/// chunk and the recording's tail becomes unrecoverable.
///
/// Heuristic: scan phrase lengths 1..=7. For each, look for the longest
/// run of consecutive identical (case-insensitive, punctuation-stripped)
/// occurrences. Flag the chunk when:
///   - some phrase repeats ≥4 times in a row, OR
///   - some phrase repeats ≥3 times AND covers ≥60% of the chunk's words.
///
/// The double rule keeps "yes yes yes" or "ja ja ja" mid-conversation from
/// being dropped — a 3-rep tiny chunk is plausibly real speech, but a 3-rep
/// run dominating a longer chunk is collapse.
/// Minimum run of consecutive words pinned to a single zero-length instant for
/// `is_timing_collapse` to fire.
///
/// **This is a weak signal on its own — never use it alone to drop content.**
/// Measured against a real 45-minute Norwegian recording (local NB Whisper):
/// four separate mic chunks of perfectly ordinary speech contain pinned runs of
/// 3, 4, 4 and **7**, longer than the 4 in the hallucinated chunk this was
/// written to catch. Degenerate timings are just how this model reports short
/// words and trailing tokens. Only meaningful in conjunction with a prior that
/// already suggests hallucination — see
/// `drop_incidental_stream_hallucinations`.
const TIMING_COLLAPSE_RUN: usize = 3;

/// Whether a chunk's word timings show the aligner giving up: consecutive words
/// landing on the *same* instant with zero duration, which is what happens when
/// the decoder emits high-prior text it has no audio to align against.
///
/// Corroborating evidence, not proof. See `TIMING_COLLAPSE_RUN` for the measured
/// false-positive rate on real speech.
fn is_timing_collapse(spans: &[(u64, u64)]) -> bool {
    let mut run = 1usize;
    for pair in spans.windows(2) {
        let (prev, cur) = (pair[0], pair[1]);
        let pinned_together = prev.0 == prev.1 && cur.0 == cur.1 && prev.0 == cur.0;
        if pinned_together {
            run += 1;
            if run >= TIMING_COLLAPSE_RUN {
                return true;
            }
        } else {
            run = 1;
        }
    }
    false
}

/// Drop hallucinated chunks from a stream that is statistically noise.
///
/// Two weak signals in conjunction, because neither is sufficient alone:
///
///  1. **The stream is incidental** — it contributed a negligible share of the
///     recording's chunks. One sys chunk in 157 is a notification chime or a few
///     seconds of video during an in-person meeting, not a side of a
///     conversation.
///  2. **The chunk's word timings collapsed** — see `is_timing_collapse`. On its
///     own this fires on real speech (runs of 7 measured on genuine mic chunks),
///     which is why it's only consulted for a stream already known to be noise.
///
/// Restricting to the incidental stream is what makes this safe: the stream
/// carrying the meeting is never touched, so no amount of timing weirdness in
/// real speech can delete it.
///
/// Runs *before* the capture-mode decision, because a hallucinated chunk's
/// damage isn't limited to its own line — a single one on an otherwise silent
/// system stream flips the recording into the hybrid branch and then invents a
/// phantom speaker for itself. Removing it lets an in-person recording be
/// recognised as one.
///
/// Chunks without word timings pass through untouched: some providers (the
/// current OpenAI path) don't return them, and absence of evidence isn't
/// evidence of collapse.
pub(super) fn drop_incidental_stream_hallucinations(chunks: Vec<ChunkRecord>) -> Vec<ChunkRecord> {
    let total = chunks.len();
    if total == 0 {
        return chunks;
    }
    let mic_count = chunks.iter().filter(|c| c.source == ChunkSource::Mic).count();
    let sys_count = total - mic_count;
    let is_incidental = |source: ChunkSource| -> bool {
        let n = match source {
            ChunkSource::Mic => mic_count,
            ChunkSource::Sys => sys_count,
        };
        n > 0 && (n as f32 / total as f32) < INCIDENTAL_STREAM_CHUNK_SHARE
    };
    chunks
        .into_iter()
        .filter(|c| {
            if c.words.is_empty() || !is_incidental(c.source) {
                return true;
            }
            let spans: Vec<(u64, u64)> = c.words.iter().map(|w| (w.start_ms, w.end_ms)).collect();
            if is_timing_collapse(&spans) {
                eprintln!(
                    "diarize: dropping hallucinated {:?} chunk at {}ms from incidental stream: {:?}",
                    c.source,
                    c.start_ms,
                    c.text.chars().take(60).collect::<String>()
                );
                return false;
            }
            true
        })
        .collect()
}

fn is_repetition_collapse(text: &str) -> bool {
    let words: Vec<String> = text
        .split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect();
    let n = words.len();
    if n < 6 {
        return false;
    }
    for phrase_len in 1..=7 {
        if n < phrase_len * 3 {
            continue;
        }
        let mut start = 0;
        while start + phrase_len <= n {
            let mut reps = 1;
            let mut pos = start + phrase_len;
            while pos + phrase_len <= n
                && words[pos..pos + phrase_len] == words[start..start + phrase_len]
            {
                reps += 1;
                pos += phrase_len;
            }
            if reps >= 4 {
                return true;
            }
            if reps >= 3 && (phrase_len * reps) * 5 >= n * 3 {
                return true;
            }
            start = pos.max(start + 1);
        }
    }
    false
}

/// Minimum share of the weighted vote the winning language must hold for
/// `majority_language` to answer at all.
const LANGUAGE_VOTE_MIN_SHARE: f64 = 0.6;

/// Decide what language a recording was actually in, from the per-chunk
/// detections the STT provider handed back (issue #167).
///
/// Votes are weighted by chunk text length, not counted one-per-chunk: a
/// 40-word chunk is far better evidence than a 3-word one, and unweighted
/// counting is exactly how a handful of "mm-hm" fillers — which detect as
/// anything — would outvote the meeting. Chunks with no detection are
/// ignored rather than counted as a language.
///
/// Returns `None` when nothing clears `LANGUAGE_VOTE_MIN_SHARE`. A
/// genuinely bilingual recording should decline to answer rather than pick
/// a side; the caller then falls back to the transcript-anchored `auto`
/// directive, which is the right behaviour for mixed audio anyway.
fn majority_language(chunks: &[ChunkRecord]) -> Option<String> {
    let mut weights: std::collections::HashMap<&str, u64> = std::collections::HashMap::new();
    let mut total: u64 = 0;
    for c in chunks {
        let Some(lang) = c.detected_language.as_deref() else { continue };
        if lang.is_empty() {
            continue;
        }
        // Length in words, floored at 1 so a detection on a short-but-real
        // chunk still carries some weight.
        let weight = c.text.split_whitespace().count().max(1) as u64;
        *weights.entry(lang).or_insert(0) += weight;
        total += weight;
    }
    if total == 0 {
        return None;
    }
    let (lang, weight) = weights.into_iter().max_by_key(|&(_, w)| w)?;
    if (weight as f64) / (total as f64) >= LANGUAGE_VOTE_MIN_SHARE {
        Some(lang.to_string())
    } else {
        None
    }
}

/// Persist the recording's detected language on the note, if this capture
/// produced one and the note doesn't already carry one (issue #167).
///
/// First detection wins — a resumed recording appends to an existing note
/// and shouldn't overwrite what the original take established.
///
/// The adapters already suppress a detection whenever we named a language
/// ourselves, so in principle a `Some(code)` chunk can only come from an
/// `auto` capture. The note's own language is re-checked here anyway: the
/// invariant lives in four separate adapters, and the cost of one of them
/// drifting is that we silently persist our own request echoed back and
/// then summarise against it.
pub(super) fn record_detected_language(app: &AppHandle, note_id: &str, chunks: &[ChunkRecord]) {
    let Some(lang) = majority_language(chunks) else { return };
    let state: State<AppState> = app.state();
    let conn = state.db.lock();
    let note = match db::get_note(&conn, note_id) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("record_detected_language: {e}");
            return;
        }
    };
    if note.detected_language.is_some() {
        return;
    }
    // Same resolution rule as transcription and summary: the note's own
    // language wins, empty falls back to the global setting.
    let resolved = if note.language.trim().is_empty() {
        db::get_setting(&conn, "language")
            .ok()
            .flatten()
            .unwrap_or_else(|| DEFAULT_LANGUAGE.to_string())
    } else {
        note.language.clone()
    };
    if resolved != "auto" {
        return;
    }
    if let Err(e) = db::set_detected_language(&conn, note_id, &lang) {
        eprintln!("record_detected_language: {e}");
    } else {
        eprintln!("[stt] detected recording language: {lang}");
    }
}

#[cfg(test)]
mod language_vote_tests {
    use super::*;

    fn chunk(text: &str, lang: Option<&str>) -> ChunkRecord {
        ChunkRecord {
            source: ChunkSource::Mic,
            start_ms: 0,
            text: text.to_string(),
            words: Vec::new(),
            detected_language: lang.map(str::to_string),
        }
    }

    fn words(n: usize) -> String {
        vec!["ord"; n].join(" ")
    }

    #[test]
    fn no_chunks_and_no_detections_both_decline() {
        assert_eq!(majority_language(&[]), None);
        assert_eq!(majority_language(&[chunk("hello there", None)]), None);
    }

    #[test]
    fn unanimous_detection_wins() {
        let chunks = vec![chunk(&words(20), Some("en")), chunk(&words(30), Some("en"))];
        assert_eq!(majority_language(&chunks), Some("en".to_string()));
    }

    // The regression test for the whole feature: an English meeting with one
    // stray filler chunk misdetected as Norwegian must still resolve to `en`.
    #[test]
    fn a_short_stray_chunk_does_not_outvote_the_meeting() {
        let chunks = vec![
            chunk(&words(60), Some("en")),
            chunk(&words(40), Some("en")),
            chunk("mm", Some("no")),
            chunk("ja", Some("cy")),
        ];
        assert_eq!(majority_language(&chunks), Some("en".to_string()));
    }

    // Unweighted counting would call this 3–1 for Norwegian. Weighted, the
    // English body holds ~96% and wins — that's the point of the weighting.
    #[test]
    fn many_tiny_chunks_lose_to_one_long_one() {
        let mut chunks = vec![chunk(&words(200), Some("en"))];
        for _ in 0..8 {
            chunks.push(chunk("hm", Some("no")));
        }
        assert_eq!(majority_language(&chunks), Some("en".to_string()));
    }

    #[test]
    fn an_even_bilingual_split_declines_to_answer() {
        let chunks = vec![chunk(&words(50), Some("en")), chunk(&words(50), Some("no"))];
        assert_eq!(majority_language(&chunks), None);
    }

    #[test]
    fn chunks_without_a_detection_are_ignored_not_counted() {
        // The undetected bulk must not dilute the winner below the threshold —
        // otherwise a provider that only reports sometimes would never answer.
        let chunks = vec![chunk(&words(500), None), chunk(&words(10), Some("en"))];
        assert_eq!(majority_language(&chunks), Some("en".to_string()));
    }

    #[test]
    fn a_bare_majority_below_the_threshold_declines() {
        // 55/45 is a bilingual recording, not an English one.
        let chunks = vec![chunk(&words(55), Some("en")), chunk(&words(45), Some("no"))];
        assert_eq!(majority_language(&chunks), None);
    }
}

#[cfg(test)]
mod incidental_stream_tests {
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

    #[test]
    fn timing_collapse_catches_the_real_hallucinated_chunk() {
        // Verbatim word timings from the K2 recording's single sys chunk —
        // "Enhver likhet med virkelige hendelser og personer er tilfeldig."
        // hallucinated over a two-second notification chime. Four words pinned
        // to 3050-3050.
        let spans = vec![
            (760, 760),
            (760, 1200),
            (1500, 1500),
            (1570, 2260),
            (2260, 3050),
            (3050, 3050),
            (3050, 3050),
            (3050, 3050),
            (3050, 3050),
        ];
        assert!(is_timing_collapse(&spans));
    }

    #[test]
    fn timing_collapse_ignores_clean_timings() {
        // Monotonic, non-degenerate timings — ordinary transcribed speech.
        let spans = vec![(0, 260), (260, 520), (520, 860), (890, 1380), (1400, 1560)];
        assert!(!is_timing_collapse(&spans));
        // A lone zero-duration word (quantisation) is not collapse.
        assert!(!is_timing_collapse(&[(0, 260), (300, 300), (400, 900)]));
        // Two adjacent zeros at the same instant still isn't enough.
        assert!(!is_timing_collapse(&[(0, 260), (300, 300), (300, 300), (400, 900)]));
        // Zeros at *different* instants aren't a pinned run.
        assert!(!is_timing_collapse(&[(100, 100), (200, 200), (300, 300)]));
        assert!(!is_timing_collapse(&[]));
    }

    #[test]
    fn timing_collapse_alone_would_flag_real_speech_so_never_use_it_alone() {
        // Tripwire. This pattern is from a genuine mic chunk in the K2
        // recording — "Det var den. Det var den, ja." — whose local-Whisper
        // timings contain a pinned run of SEVEN, longer than the hallucinated
        // chunk's four. `is_timing_collapse` fires on it, which is exactly why
        // `drop_incidental_stream_hallucinations` consults it only for a stream
        // that is already statistically noise. Anyone tempted to promote this
        // into a standalone hallucination filter deletes real speech.
        let real_speech_with_pinned_run = vec![
            (0, 300),
            (300, 620),
            (900, 900),
            (900, 900),
            (900, 900),
            (900, 900),
            (900, 900),
            (900, 900),
            (900, 900),
        ];
        assert!(
            is_timing_collapse(&real_speech_with_pinned_run),
            "documents the false positive this signal has on its own"
        );
    }

    #[test]
    fn dominant_stream_keeps_its_chunks_however_odd_the_timings() {
        // The safety property. 156 mic chunks to 1 sys chunk: the mic carries
        // the meeting, so even a mic chunk with a pinned run of 7 survives.
        // Without this the K2 note would have lost four chunks of real speech.
        let mut chunks: Vec<ChunkRecord> = (0..156)
            .map(|i| {
                mic_with_words(
                    i * 15_000,
                    vec![("Det", 0, 300), ("var", 900, 900), ("den", 900, 900), ("ja", 900, 900)],
                )
            })
            .collect();
        chunks.push(sys_with_words(
            110_019,
            vec![
                ("Enhver", 760, 760),
                ("og", 3050, 3050),
                ("personer", 3050, 3050),
                ("er", 3050, 3050),
            ],
        ));

        let kept = drop_incidental_stream_hallucinations(chunks);
        assert_eq!(kept.len(), 156, "every mic chunk survives");
        assert!(kept.iter().all(|c| c.source == ChunkSource::Mic));
    }

    #[test]
    fn a_stream_carrying_real_share_is_not_pruned() {
        // A genuine two-sided call: sys is well above the incidental share, so
        // its chunks are never dropped on timing evidence alone.
        let chunks = vec![
            mic_with_words(0, vec![("me", 0, 300)]),
            sys_with_words(
                1_000,
                vec![("a", 500, 500), ("b", 500, 500), ("c", 500, 500), ("d", 500, 500)],
            ),
            mic_with_words(2_000, vec![("me", 0, 300)]),
            sys_with_words(3_000, vec![("them", 0, 400)]),
        ];
        assert_eq!(drop_incidental_stream_hallucinations(chunks).len(), 4);
    }

    #[test]
    fn dropping_the_collapsed_chunk_makes_the_recording_mic_only_again() {
        // The whole point of filtering before the capture-mode decision: with
        // the hallucinated sys chunk gone, this is an in-person recording, so it
        // takes the mic-only branch and the note's head-count goes straight to
        // the mic instead of being split with a phantom remote side.
        // 40 clean mic chunks to 1 collapsed sys chunk — a 2.4% sys share, well
        // inside "incidental", matching the real 1-in-157.
        let mut chunks: Vec<ChunkRecord> = (0..40)
            .map(|i| mic_with_words(i * 15_000, vec![("Jeg", 0, 260), ("snakker", 260, 900)]))
            .collect();
        chunks.push(sys_with_words(
            110_019,
            vec![
                ("Enhver", 760, 760),
                ("likhet", 760, 1200),
                ("og", 3050, 3050),
                ("personer", 3050, 3050),
                ("er", 3050, 3050),
                ("tilfeldig.", 3050, 3050),
            ],
        ));
        assert!(chunks.iter().any(|c| c.source == ChunkSource::Sys));

        let kept = drop_incidental_stream_hallucinations(chunks);
        assert_eq!(kept.len(), 40, "the collapsed sys chunk is gone");
        assert!(
            !kept.iter().any(|c| c.source == ChunkSource::Sys),
            "no sys chunks left → mic-only branch, no phantom speaker"
        );
    }

    #[test]
    fn chunks_without_word_timings_survive_the_filter() {
        // Providers that don't return word timings (the current OpenAI path)
        // must not have their chunks dropped for lack of evidence.
        let chunks = vec![mic(0, "no word timings here"), sys(5_000, "nor here")];
        assert_eq!(drop_incidental_stream_hallucinations(chunks).len(), 2);
    }
}

#[cfg(test)]
mod repetition_tests {
    use super::*;

    #[test]
    fn collapse_detects_long_phrase_loop() {
        // The exact pattern from the user-reported screenshot: "Er det en
        // bok?" repeated dozens of times.
        let s = "Er det en bok? ".repeat(20);
        assert!(is_repetition_collapse(&s));
    }

    #[test]
    fn collapse_detects_single_word_loop() {
        let s = "yes yes yes yes yes yes yes yes";
        assert!(is_repetition_collapse(s));
    }

    #[test]
    fn collapse_passes_normal_speech() {
        // Real-world Norwegian sample with no repetition collapse.
        let s = "Vi har en avtale i morgen klokken ti om prosjektet vi diskuterte forrige uke.";
        assert!(!is_repetition_collapse(s));
    }

    #[test]
    fn collapse_passes_natural_three_rep_short() {
        // Three short reps in a 6-word total chunk are below the dominance
        // threshold (need ≥4 reps OR ≥60% coverage). 6 words, 3 reps × 1
        // word = 50% coverage — passes.
        let s = "ja ja ja det stemmer mhm";
        assert!(!is_repetition_collapse(s));
    }

    #[test]
    fn collapse_detects_partial_loop_dominating_chunk() {
        // 12-word chunk, 4 reps of a 2-word phrase = 8 words = 66% coverage.
        // Should be flagged.
        let s = "noe annet skjedde okay test test test test okay noe annet";
        assert!(is_repetition_collapse(s));
    }

    #[test]
    fn collapse_handles_punctuation_and_case() {
        // Same phrase but mixed case + punctuation differences should still
        // be matched as identical reps.
        let s = "Er det en bok? er det en bok! Er Det En Bok? er det en bok.";
        assert!(is_repetition_collapse(s));
    }
}

#[cfg(test)]
mod hallucination_tests {
    use super::*;

    #[test]
    fn drops_punctuation_only() {
        assert!(is_likely_hallucination(".", "no"));
        assert!(is_likely_hallucination("...", "no"));
        assert!(is_likely_hallucination(" . ", "no"));
        assert!(is_likely_hallucination("***", "en"));
    }

    #[test]
    fn drops_norwegian_silence_greetings() {
        assert!(is_likely_hallucination("Hei.", "no"));
        assert!(is_likely_hallucination("Hei!", "no"));
        assert!(is_likely_hallucination("Takk!", "no"));
        assert!(is_likely_hallucination("Hallo.", "no"));
        assert!(is_likely_hallucination("Ha det.", "no"));
    }

    #[test]
    fn drops_english_silence_greetings() {
        assert!(is_likely_hallucination("Hi.", "en"));
        assert!(is_likely_hallucination("Hello!", "en"));
        assert!(is_likely_hallucination("Thanks!", "en"));
        assert!(is_likely_hallucination("Thank you.", "en"));
    }

    #[test]
    fn keeps_real_one_word_answers() {
        // Yes / no / ja / nei are real responses too often to drop.
        assert!(!is_likely_hallucination("Yes.", "en"));
        assert!(!is_likely_hallucination("No.", "en"));
        assert!(!is_likely_hallucination("Ja.", "no"));
        assert!(!is_likely_hallucination("Nei.", "no"));
        assert!(!is_likely_hallucination("OK", "en"));
    }

    #[test]
    fn keeps_real_speech_containing_greeting_words() {
        assert!(!is_likely_hallucination(
            "Hei, hvordan går det med deg i dag?",
            "no"
        ));
        assert!(!is_likely_hallucination(
            "I just wanted to say thanks for the help yesterday.",
            "en"
        ));
    }

    #[test]
    fn drops_caption_attribution_inside_real_speech() {
        // Substring match — caption attribution glued to a real
        // sentence should still trigger the drop.
        assert!(is_likely_hallucination(
            "And that was the meeting. Subtitles by Amara.org community.",
            "en"
        ));
    }
}
