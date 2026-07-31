# `gpt-transcribe` — do `prompt` and `keywords` do anything?

Resolves [T6](https://github.com/michaelwilhelmsen/humla/issues/133) on [the gpt-transcribe map](https://github.com/michaelwilhelmsen/humla/issues/127).

> **Redacted.** Probes ran against a real personal meeting recording. Audio-derived words are replaced by placeholders: **`⟨MÅL⟩`** is the key content noun the test scores (a two-syllable Norwegian financial term), and **`⟨MÅL-feil⟩`** is any phonetically-adjacent misrecognition of it. `⟨STED⟩` is a place name. Counts, arm structure and every error *pattern* are unmodified.

## Why status codes couldn't answer this

[T1](https://github.com/michaelwilhelmsen/humla/issues/128) established that this endpoint returns **HTTP 200 for an invented parameter** — unknown multipart fields are silently discarded. So `prompt` and `keywords` returning 200 proved nothing, and only a differential test could.

## Building a test with power

Two designs failed before one worked. Both failures are worth recording, because both would have produced a confident wrong answer:

1. **Misspelling a proper noun** (`prompt=Sverrige…`, audio says `⟨STED⟩`). `whisper-1` — which *definitely* reads `prompt` — ignored it 3/3. A well-known place name is too strongly anchored acoustically for prompt bias to move.
2. **Style transfer and language drift** (lowercase-unpunctuated prompt; Spanish prompt). `whisper-1` was **byte-identical** across all arms on the clean clip. The clip was too easy: a fully confident model cannot be biased by anything.

**The design that worked: silence.** On a near-silent chunk, `whisper-1` with no prompt hallucinates the notorious Norwegian subtitle credit `"Teksting av Nicolai Winther"`, and with a prompt it emits **the prompt's own last sentence verbatim, 3/3**. That is decisive proof the field is read, so the test has power.

Applied to `gpt-transcribe` on the same silence, all arms returned **empty** — no hallucination, no prompt echo. A real safety advantage over `whisper-1` (see below), but silence *cannot* separate "ignored" from "read but correctly declined", since empty is the right answer. So the final test degrades real speech with pink noise (≈3 dB SNR) to make the model uncertain but non-empty.

## Results — 6 arms × 5 runs, same degraded clip, `languages=no` held constant

Scored on recovery of `⟨MÅL⟩`:

| Arm | Bias payload | `⟨MÅL⟩` recovered | False term injected |
|---|---|---|---|
| A | none (baseline) | **2/5** | — |
| B | `prompt`, misleading (names two terms absent from the audio) | 1/5 | **0/5** |
| C | `prompt`, contains the target phrase verbatim | **5/5** | — |
| D | `prompt`, real topical context, target phrase absent | 1/5 | — |
| E | `keywords`, true terms | **5/5** | — |
| F | `keywords`, misleading terms | 3/5 | **0/5** |

## What this establishes

**`keywords` is read, works, and is safe.** True terms took recovery from 2/5 to **5/5**; misleading terms neither helped nor injected anything false (3/5, within baseline noise, 0/5 injection). This is the documented slot for exactly Humla's custom-vocabulary use, and it behaves.

**`prompt` is read, but as lexical priming — not as topical context.** Arm C's 5/5 against a 2/5 baseline is ~1% likely by chance if the field were discarded, so it is read. But Arm D is the informative one: *genuine topical context with the phrase withheld gave no benefit at all* (1/5). Only the arm whose prompt literally contained the target phrase helped. So OpenAI's documented framing — *"free-form context about the recording, such as its topic or setting"* — describes something the parameter does **not** appear to deliver. Describing the meeting buys nothing; supplying the actual words does.

**It does not parrot.** The misleading arms never injected their false terms (0/5 both), and no prompt echo appeared on silence. So `prompt` behaves as an acoustic prior rather than a copy source — materially safer than `whisper-1`, which echoed its prompt verbatim on silence 3/3.

## Consequences for the PRD

1. **Custom vocabulary should move from the merged `build_whisper_prompt` string into `keywords`.** It is the documented slot, it demonstrably works, and it fails safe.
2. **`prior_context` survives, and for a better reason than expected.** The trailing ~150 committed words are not topical description — they are *the actual recent words*, which is precisely the lexical-priming shape Arm C shows to be the one that works. Arm D says a summarised or topical prompt would have been useless.
3. **The `prompt` hallucination risk that motivated this ticket did not materialise** on this model. `gpt-transcribe` returned empty on silence in every arm, where `whisper-1` produced both a fabricated subtitle credit and a verbatim prompt echo. Humla's `is_likely_hallucination` and the RMS silence gate remain useful, but this model is not the failure mode they were built for.
4. Since `prompt` *is* accepted here, **`skip_prompt_for_model` does not need to become a list** for this model. It stays a single `Option<&str>` for `gpt-4o-transcribe-diarize`.

## Limits — read before relying on this

- **n=5 per arm, one 8 s clip, one language, one speaker.** The 5/5-vs-2/5 contrasts are strong; the 3/5-vs-2/5 ones are not distinguishable from noise.
- Degraded-audio behaviour may not equal clean-audio behaviour. The test *required* uncertainty to detect anything, so it necessarily measured the model off its normal operating point.
- Run-to-run variation is real on this model (T1 saw detection flip across identical calls), which is why every arm was repeated.
