# OpenAI STT launch video — `gpt-transcribe` + `gpt-live-transcribe`

**Source:** <https://www.youtube.com/watch?v=WeP9VUf1OoE> — "Introducing gpt-transcribe and gpt-live-transcribe" (~2 min).

**Provenance:** automated retrieval failed (the watch page returns only footer HTML; third-party transcript services returned HTTP 403). The transcript was supplied manually by Michael on 2026-07-31 and the notes below are taken from it directly. An earlier revision of this file reconstructed the launch from secondary reporting and carried several wrong hedges; it has been replaced.

Companion doc, covering the API/pricing/parameter detail: [`openai-stt-2026-07.md`](./openai-stt-2026-07.md).

## The split between the two models

Stated framing from the video:

- **`gpt-transcribe`** — takes a completed file, returns the full transcript. Pitched for call archives, podcasts and larger batch jobs, *"where you can wait for a complete file and optimize for accuracy and throughput."*
- **`gpt-live-transcribe`** — holds a live open connection and returns text as the audio arrives. Pitched for captions, dictation and voice interfaces, *"where latency meaningfully impacts the experience."*

This is an explicit **accuracy-vs-latency positioning by OpenAI**, and it is the single most decision-relevant thing in the video. See the note on Humla below.

## Capability claims

Both models:

- **57 languages.**
- Better at the parts of transcription that "tend to break": **accents, multilingual speech, very short answers, names, and numbers.**
- **Vocabulary biasing** — developers can supply a list of words, proper nouns or code terms that matter. The demo passes a prompt plus domain terms (`phishing`, `ARR`, `A1C`) drawn from cybersecurity, sales and healthcare as examples of easily-missed vocabulary.
- **Better background-noise rejection** — the claim is that a side conversation or ambient music is less likely to be picked up as speech, with a loud cafe / busy conference hall / chatty coworker given as the target scenarios.

### Automatic language following

Language hints are **optional**. By default the model follows multilingual speech automatically. The presenter demonstrates this live, switching English → Spanish → English inside a single session, with the transcript following in both directions.

This is demonstrated on the **live** model, so realtime does not forfeit multilingual capability.

## Throughput datapoint

A meeting recording of about half an hour is processed by `gpt-transcribe` in "a little less than a minute" — roughly **30× realtime**.

## What the video does *not* provide

- **No latency figures.** No millisecond numbers for `gpt-live-transcribe` anywhere.
- **No WER or benchmark numbers.** This confirms that the ~40.37% → ~19.27% Common Voice figures circulating in secondary reporting are **not** from this launch and are misattributed to an earlier model generation. Do not cite them for these models.

## Relevance to Humla

- The **automatic language-following** behaviour bears directly on [#125](https://github.com/michaelwilhelmsen/humla/issues/125): `openai_compat::transcribe` currently always sends a language, which may suppress exactly this behaviour on mixed Norwegian/English meetings.
- The claimed gains on **names, numbers and very short answers** target our weakest transcript output.
- Better **noise rejection** may reduce the load carried by `is_likely_hallucination`, the `silence_rms_threshold` gate and the repetition-collapse guards — measure before loosening any of them.
- The **accuracy-vs-latency positioning weakens the case for [#126](https://github.com/michaelwilhelmsen/humla/issues/126)**. Humla's transcript is an archival record that also feeds the summary; it is not a caption track. Moving to the model OpenAI positions as the latency-optimised (rather than accuracy-optimised) option, at 3.8× the cost and without word timestamps, needs stronger justification than faster on-screen text.
