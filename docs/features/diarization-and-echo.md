# Diarization and speaker echo

Engine choice, fallback, the `speaker-diarize` sidecar, and the echo pass that runs before a mic diarize. See also [ADR-0005](../adr/0005-nemotron-3-is-the-default-diarization-engine.md) and `docs/research/stream-alignment-and-echo.md`.

## Engines (#193)

- **Nemotron 3** — FluidAudio's `Nemotron3Diarizer`, up to 8 speakers, counts them itself, takes no count hint. Default for new installs.
- **Community-1** — FluidAudio's `OfflineDiarizerManager` (community-1 segmentation + VBx clustering with PLDA). Takes the note's speaker count.

**The engine is chosen per note** by `diarize::choose_engine` (same answer for every take and the unify pass); `engine_preference` is the pure rule:
- A note set above 8 speakers → community-1 with the hint (keeping the hybrid split of that total).
- Otherwise → the engine `diarize_model` selects.
- A missing model falls back to the other engine when it is downloaded, including above 8 (Nemotron's labels capped at 8 beat none).

**A failed run is retried with the other engine** when downloaded, in `diarize_and_maybe_clean`, the one entry every diarize pass goes through. The pass's engine is a `&mut` its streams share, so after a successful retry the remaining streams use the fallback; each stream's voices are numbered in their own namespace off `build_hybrid_labels`' shared counter, so mixing is harmless.

- The failed engine is **demoted** only when the fallback succeeded on the same audio, and only for the launch: `choose_engine` skips it. A successful `diarize_download` clears it.
- A demoted Nemotron 3 is **re-checked** in the background (`diarize::recheck`: the download's warm-up inference, under the download lock). Only when preparing it fails is its warm marker removed (every identity's); the `.humla-warm-failed-` record stays, so the launch re-warm doesn't retry it.
- Toast text: `fell_back_message`, `recheck_failed_message` in `diarize.rs`; engine names from `Engine::label`, mirrored by `DIARIZE_ENGINE_LABEL` in `src/lib/diarizeEngine.ts` (pinned by a test reading it).
- `DEFAULT_DIARIZE_MODEL` / `Engine::from_setting` / `NEMOTRON_MAX_SPEAKERS` are mirrored by `DEFAULT_DIARIZE_ENGINE` / `selectedDiarizeEngine` / `NEMOTRON_MAX_SPEAKERS` in `src/lib/diarizeEngine.ts` (pinned by its test reading the Rust files). `enginesToDownload` there fetches what `engine_preference` falls back to. Change both sides.
- The speaker chip and the import dialog offer Auto and 1–12 from one list, `src/lib/speakerCount.ts`.
- `Engine::from_setting` is lenient (unknown stored value → community-1); `Engine::parse` is strict (an unknown IPC argument is refused, or deleting it would delete community-1).
- Installs from before #193 were pinned to community-1 (`db::migrate_diarize_engine`, keyed on whether `notes.sqlite` existed before this launch) and offered the switch by a Home card (`nemotron_offer`, `src/lib/nemotronOffer.ts`).
- Sortformer is removed: a stored `sortformer` reads as community-1, and its model folder is deleted once (`diarize::remove_retired_models`).

## Hybrid labels

Both streams are diarized when both have content; the mic is never assumed to hold one person. `You:` is applied only when the mic diarize resolves to exactly one voice. Don't infer capture topology from a stream merely being present: one stray system chunk used to collapse an in-person meeting onto `You:`.

## From segments to turns

Each chunk is split into per-speaker pieces by its words' midpoints against the diarizer's segments (`split_by_labels`). The pieces then go through one sequence, `refine_pieces` in `commands/transcript.rs`, which `build_labelled_transcript` (the transcript string), `serialize_timeline` (`timeline.jsonl`, what the reader draws) and the diagnostics dump all run. Never correct speakers in only one of them: the boundary moves used to run on the transcript string alone, so the reader never showed them and the first rebuild from the timeline undid them.

1. `bridge_short_interjections`: an A-B-A sandwich whose B is at most 6 words and 3.5 s becomes A.
2. `realign_sentence_boundaries`: a sentence's first or last 1–3 words that landed across a speaker change move to the side they belong to, with their word timings. Forward when a finished sentence is followed by a capitalised fragment and the next piece opens lowercase ("…the perfect way. It" / "does pay off."); backward when an open piece is followed by a lowercase `<1–3 words>.` and more ("…Brand" / "new. Hadn't done…"). Only within one stream, both pieces timed or both untimed, and within 2 s when timed. When both fit, the smaller move wins, and a repetition picks backward. Scripts without case never match.
3. `absorb_text_continuation_chains`: pieces that are still one sentence (no terminator or dash at the end, lowercase next) get one speaker; a tie goes to the label that spoke first.

Then a model checks what case can't tell (`commands/boundaries.rs`): every speaker change that cuts a sentence within one stream, where the earlier side ends with neither a terminator (`piece_ends_terminator`) nor a pause mark (after a pause, the next person usually took over), shown with 30 words either side. It finds the sentence the change cuts and gives it to whoever said it, the side holding more of it when the text doesn't tell. It answers a move per change: words from the start of the later run back to the earlier speaker, or from the end of the earlier run forward. At most 8 words, said within 6 s of the change; an entry whose words all move changes speaker whole, a partly moved one gives its words (with their timings) to its neighbour. The moves are written into `timeline.jsonl` and the transcript is rebuilt from it, so every reader of the note agrees. A judged change is recorded by the words around it in the take's `boundaries.json` and not asked again; "Clean again" asks anew. Nothing is written for a take whose timeline changed while the model worked (a per-turn edit wins), nor when a recording started on the note meanwhile. Runs on the note's summary provider's light model (`clean::light_provider`, on OpenAI with `reasoning_effort: "low"`), before the clean pass: after a recording, a deferred transcription and a re-diarize (which rebuilds the timeline from the diarizer and drops earlier moves), never automatically on Copilot.

Word texts come from `display_words`, which spells each timed word the way the chunk's transcript does: Deepgram (`punctuated_word` is read too), whisper-1 and Groq return bare words, and both the reader and the rules above need the punctuation and capitals.

## The `speaker-diarize` sidecar

- **FluidAudio pinned exactly at 0.17.4.** It changes diarizer output in patch releases and `Package.resolved` is gitignored. 0.17.4 is Nemotron 3's floor (#203): it types preallocated output buffers from the model description (macOS 27 enforces this) and loads the `monolithic/v2` fp16 re-exports that compile for the Neural Engine on M3-class chips.
- The re-export keeps the original weights version, so `diarize::prepare_nemotron_at_launch` moves an install off the old bundle: on Nemotron 3 it fetches and prepares the new one in the background and removes the old one once that succeeds; otherwise it only removes the old one.
- `traits: []` drops its text-normalization engine, which is why `Package.swift` is swift-tools 6.2 with `swiftLanguageModes: [.v5]`.

### Nemotron 3

- fp16 **`fast128`** preset on `.cpuAndNeuralEngine`; speaker ids are `S<slot>` in arrival order.
- **Never a split W8A8 preset**: `c128-split-w8a8` outputs ~0.5 for every speaker on M1-class Neural Engines. Check any preset change on M1–M3.
- The first Neural Engine load compiles the model (~100 s on an M1 Max). `download` loads on CPU first (fetching files), then reports `phase: "warming"`, loads on the Neural Engine and runs one short inference. CoreML caches the result under `~/Library/Caches/<bundle id>/com.apple.e5rt.e5bundlecache/<macOS build>/`.
- `status` counts the model as downloaded only when **ready**: FluidAudio's weights-version marker is current *and* Humla's `.humla-warm-<bundle id or binary name>` marker exists, holding `nemotronWarmStamp` (weights + bundle, macOS build from `kern.osversion`, hash of the sidecar binary). Until ready, `choose_engine` falls back to community-1, keeping the compile out of a post-stop chain.
- A stale stamp reports `needsWarmUp`; `prepare_nemotron_at_launch` re-runs the warm-up through `download` (no fetch when files exist). The hash covers the code signature, so every release re-warms once (under a second while CoreML's cache holds). It re-warms whatever engine is selected.
- A failed warm-up of files on disk writes `.humla-warm-failed-<identity>` (stamp, then error). `status` reports it as `warmUpError` while the stamp is unchanged; the launch re-warm skips it (`wants_rewarm`). Settings offers **Try again** (a plain `download`); success removes the marker.
- `download` returns at once for a model that is already ready. The diarize path refuses only missing files; an unwarmed run is slow, never wrong.

### Community-1

- `clusteringThreshold: 0.5` (community default 0.6).
- **Polarity of the stored `community1_threshold`: higher → more speakers, lower → fewer.** FluidAudio 0.15.6+ reads the threshold the other way, so the sidecar converts with `fluidAudioClusteringThreshold` = √(2 − 2t) (0.5 → 1.0) in `speaker-diarize/Sources/DiarizeCore`, pinned by `swift test`. Passing the stored value raw over-splits.
- **The speaker-count hint is the control that works, not the threshold.** With no hint (`Auto`, no `expected_speakers`) VBx often collapses a multi-speaker meeting to one voice; `withSpeakers(exactly: n)` fixes it. Don't add an automatic retry (it would split real voice memos) and don't re-tune `clusteringThreshold`: across 0.5–0.7 the output is identical, at 0.8 it overshoots.

### CLI

- `speaker-diarize <wav> [--engine community1|nemotron3] [--num-speakers N] [--threshold T]` → JSON array of `{start_ms, end_ms, speaker_id}`. Hint and threshold apply to community-1 only. No `--engine` → community-1; an unknown engine is a usage error (exit 2).
- `speaker-diarize status [--engine …]` → `{downloaded, needsWarmUp, warmUpError, sizeBytes, path}` (the warm-up fields only for Nemotron 3).
- `speaker-diarize download [--engine …]` → `{event:"progress", fraction, phase}` (`listing` / `downloading` / `compiling`, then `warming` for Nemotron 3), then `{event:"done"}`. Failure prints `{event:"failed", step}` (`prepare` when files are on disk, `download` otherwise) and exits 1 with a `humla-error:` line. `DownloadStep` in `diarize.rs` words the error. Downloads run one at a time.
- `speaker-diarize delete [--engine …]` wipes the engine's cache directory.
- Short-lived (`diarize_file(path)` in `diarize.rs`): spawned per pass over a full WAV, no speaker state across recordings.

## Speaker echo (#196)

When a call plays through the speakers, the mic records every remote voice again and a mic diarize counts the echo as people. Wherever the mic is diarized beside a system stream — `diarize_and_apply`'s hybrid branch, `rediarize_apply_to_chunks` (they mirror each other) and `unify_apply` per take — `echo.rs` runs first as the step `removing_echo`:

1. **Lag**: the take's own lag from `echo-probe`'s GCC-PHAT track. A window counts only when its neighbours find the same lag. No lag → neither part runs.
2. **Cancel**: `echo-probe`'s MDF canceller and suppressor cancel the echo from the mic. The mic diarize reads the cancelled mic from `humla-echo-<uuid>.wav` in the **system temp dir**, deleted once read — never under `recordings/` or `diagnostics/`, which would put audio outside `keep_audio`.
3. **Drop echo voices** before numbering, so their words fall to the nearest remaining voice. A voice is echo when its share of frames in (lag-shifted, energy-detected) sys silence is under half the take's, **or** the linear canceller removed ≥ 4.5 dB of its energy. The energy only adds to the ratio, never overrules it.

Guards: under 5 s of sys silence judges nothing; a take never loses every voice. The unify pass judges each hybrid take inside its own span of the joined mic, and does so before the system stream's count hint is taken from the mic's voice count.

- Code: `analyze_take`, `TakeAnalysis::check`, `check_joined` in `echo.rs` (pure); `commands/diarization.rs`'s `EchoPass` reads the WAVs, runs it off the async runtime and writes the temp WAV.
- The thresholds were fitted on two podcast takes from one Mac; the diarize dump (`echo`, and `<engine>-unify.json`) keeps per-take lag and echo removed and per-voice seconds, ratio, energy removed and verdict. `echo_ms` / `unify_echo_ms` time it.
- Performance (#200): ~7–8 s and 470 MB per hour on an M1 Max. Lag windows run on threads, both streams stay 16-bit samples, and the suppressor and report run block by block inside the canceller (`aec::cancel_threaded`).
- Takes up to ten minutes are cancelled exactly as two whole passes would. Longer takes converge on their last three minutes of far-end signal (ending on the last block where the far end sounds), then split into ranges of at least three minutes on up to eight threads, each started 20 s ahead of its range. Past ten minutes output is no longer bit-identical and diarizers flip on borderline 7–15 s fragments, so judge changes there against several baselines, never one.
- `echo-probe`'s `cancel` keeps the two whole passes, for the CLI. The transcript keeps its text dedup, because live chunks transcribe before the full streams exist.
