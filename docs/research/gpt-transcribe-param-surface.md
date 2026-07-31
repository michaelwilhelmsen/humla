# `gpt-transcribe` parameter surface — live probe results

Resolves [T1](https://github.com/michaelwilhelmsen/humla/issues/128) on [the gpt-transcribe map](https://github.com/michaelwilhelmsen/humla/issues/127).

Probed against `POST https://api.openai.com/v1/audio/transcriptions` on 2026-07-31. Every row below is an observed HTTP status from a real request, not a doc reading. Raw transcripts: `gpt-transcribe-probe-raw.txt`. Schema-doc findings: `gpt-transcribe-schema.md`.

> **Redacted.** The probes ran against a real personal meeting recording, so the transcript text quoted throughout this file and in the raw log has been paraphrased — nouns swapped, sentence shape and every transcription *error* preserved verbatim. The `De`/`Vi` pronoun divergence, the mid-utterance `…`, and the `viktige`/`viktig` wobble are all exactly as the models returned them. Only the subject matter is synthetic.

**Audio**: an 8 s mono 16 kHz Int16 chunk cut from a real Humla recording (`2aabed0c-…/mic.wav`, offset 58 s) — same format the sidecar emits. Norwegian speech. `whisper-1` on the identical file returns:

> De har gjort to ting som er viktige. De fjernet gebyret, altså prosjektet var på vei mot stopp, og da gjorde de det.

The internal model id surfaces in error messages as **`gpt-transcribe-api-ev3`**.

## Read this first: HTTP 200 does not mean "accepted"

A deliberately invented parameter (`bogus_param=xyz`) returns **HTTP 200**. This endpoint **silently ignores unknown multipart fields**.

So a 200 proves only that a request *succeeded* — never that a parameter was read. Any claim that this model "accepts" `prompt`, `keywords` or `temperature` on the strength of a 200 is unfounded, including the claims in the earlier research pass. Only parameters with an *observable differential effect* (`languages`) or an explicit rejection (`response_format`) are established below.

## The findings

| Parameter | Status | Established behaviour |
|---|---|---|
| `response_format=verbose_json` | **400** | Rejected. `"response_format 'verbose_json' is not compatible with model 'gpt-transcribe-api-ev3'. Use 'json' or 'text' instead."` |
| `timestamp_granularities[]=word` | **400** | Unreachable — requires `verbose_json`, which is rejected. **Word timestamps are impossible on this model.** |
| `response_format=srt` / `vtt` / `diarized_json` | **400** | All rejected, same message shape. |
| `response_format=json` / `text` | 200 | The only two valid values. Confirmed by the error message's own advice. |
| `language` (singular) | 200 | **Silently ignored.** `language=de` on Norwegian audio returned Norwegian text and reported detection `nb`. No error, no effect — the worst failure mode. |
| `languages` (plural) | 200 | **Genuinely read**, and validated against an allowlist (see below). Optional — omitting it succeeds. |
| `temperature=0` | 200 | Unproven — indistinguishable from the ignored-unknown-param case. |
| `prompt` | 200 | Unproven — same caveat. |
| `keywords` | 200 | Unproven — same caveat. |
| `stream=true` | 200 | Works. Emits `data: {"type":"transcript.text.delta","delta":"…"}` then `data: [DONE]`. |

### `languages` wire shape

| Form | Result |
|---|---|
| `languages=no` (single, bare) | 200, effective |
| `languages[]=no` repeated | 200, effective |
| `languages=no` repeated (no brackets) | 200, effective |
| `languages=["no","en"]` (JSON array) | **400** `invalid_value` |
| `languages=no,en` (comma-joined) | **400** `invalid_value` |

Multi-value requires a **repeated field**. Neither a JSON array nor a comma-joined string works.

### `languages` accepts only 63 of Humla's 101 language options

`languages` is validated against an allowlist — an out-of-list code is a hard **400**, i.e. a failed transcription, not a degraded one. Swept all 100 non-`auto` codes in `src/lib/languages.ts`:

**Accepted (63):** `af ar hy az be bn bs bg yue ca zh hr cs da nl en et fi fr gl ka de el gu he hi hu is id it ja kn kk ko lv lt mk ms ml mi mr ne no fa pl pt ro ru sr sk sl es sw sv tl ta te th tr uk ur vi cy`

**Rejected (37):** `sq am as ba eu br my fo ht ha haw jw km lo la ln lb mg mt mn nn oc ps pa sa sn sd si so su tg tt bo tk uz yi yo`

Humla's list is Whisper's; `gpt-transcribe`'s is narrower. Note **`nn` (Nynorsk) is rejected** — a live concern for a Norwegian-first app.

## Behavioural findings the ticket didn't ask for but that bear on the map

1. **Norwegian is misdetected as Danish without a hint.** With no `languages`, detection returned `da` on **5/5** identical calls. The un-hinted transcript also opens `"Vi har gjort"` where `whisper-1` (correctly) has `"De har gjort"`.

2. **Any hint fixes that opening — even the wrong hint.** `languages=no` and `languages=en` both produced the correct `"De har gjort"`. This is direct evidence *against* the launch video's implication that omitting the hint is best, at least on monolingual Norwegian audio. It is one 8 s sample; it is not a quality verdict. That is [T2](https://github.com/michaelwilhelmsen/humla/issues/129)'s job.

3. **The response echoes rather than detects.** The response body carries a `languages` array, but it returns *the hint you supplied* when you supply one (`languages=en` → `[{"code":"en"}]` on Norwegian audio) and detection only when you don't. It is not a trustworthy language detector.

4. **It emits a literal `…` (U+2026) when audio cuts mid-utterance.** Byte-verified. `whisper-1` instead completes the sentence. Humla's chunks are VAD-bounded but still cut mid-utterance at `maxSeconds`, so a large fraction of chunks would land an ellipsis in the transcript — which interacts with `strip_attribution_tail`, the repetition-collapse guards and `prior_context`. Newly surfaced; not covered by any existing ticket.

## Pricing

**Not confirmed from primary source.** `openai.com/api/pricing` and `openai.com/pricing` both returned **HTTP 403**. Six aggregators agree on $0.0045/min (`gpt-transcribe`) vs $0.006/min (`gpt-4o-transcribe`, `whisper-1`), so the 25%-cheaper claim is *probable, not verified*. The `usage` object does report `{"type":"duration","seconds":N}`, so per-call duration billing is confirmed even though the rate is not.

## Consequence for the map

`whisper-1` is the only OpenAI model returning word timestamps, and `gpt-transcribe` cannot return them at any setting. Under #125's own rule — *"default flipped only if word timestamps survive"* — **the flip is off the table on fact.**
