# gpt-transcribe schema and pricing research

## Source 1: llms.txt → schema for POST /v1/audio/transcriptions

`https://developers.openai.com/llms.txt` points to the per-resource reference page
`https://developers.openai.com/api/reference/resources/audio/subresources/transcriptions/methods/create.md`
[DOK] (fetched directly; llms.txt itself did not paste the full schema inline, it links out to this per-method markdown doc, which does contain the schema).

Findings, general endpoint documentation, NOT a gpt-transcribe-specific matrix — the docs page does not break parameters down per model except where noted:

- **response_format**: doc text states the endpoint "Returns a transcription object in `json`, `diarized_json`, or `verbose_json` format, or a stream of transcript events." [DOK, general — not gpt-transcribe-specific]. `text`, `srt`, `vtt` are **not mentioned** in this page's enum. NOTE: those three formats existed historically for whisper-1; the fetched page gives no explicit statement that they were removed, and no per-model carve-out — treat "text/srt/vtt still valid for whisper-1" as **[IKKE FUNNET]**, only confirmed that the page's headline enum for the endpoint is json/diarized_json/verbose_json.
- **timestamp_granularities**: IS listed on the endpoint. Documented examples use it only with `whisper-1` (`timestamp_granularities[]=segment`, `=word`, combined with `verbose_json`). No explicit statement that it also works with gpt-transcribe or gpt-4o-transcribe. **[IKKE FUNNET]** whether gpt-transcribe supports it — the page gives no per-model compatibility matrix.
- **language vs languages**: the fetched page does **not document a request-time `language` or `languages` input parameter at all**. It mentions `languages` only as a field on the **response object**, described as an "optional array of TranscriptionLanguage" produced by `gpt-transcribe`. So: no evidence found of a plural `languages` *request* param, type/shape, or optionality — **[IKKE FUNNET — søkte i methods/create.md for a request-side language param]**.
- **keywords**: **not listed** on this batch endpoint per the fetched page. [DOK — absence noted; endpoint doc has no `keywords` field]. This is consistent with keywords being a realtime-endpoint-only parameter, but the fetched page does not itself make that comparison.
- **temperature / prompt**: **not listed as accepted request parameters.** The word "temperature" appears only inside the **response's** `TranscriptionSegment` object (a field reporting what temperature was used for that segment), not as an input you can set. `prompt` was not found anywhere on the page. [DOK for temperature-is-response-only; prompt = IKKE FUNNET].
- **per-model support matrix**: **none exists on this page.** It references `gpt-4o-transcribe`, `gpt-4o-transcribe-diarize`, `gpt-4o-mini-transcribe`, and `whisper-1` in scattered examples, but gives no table of which params each model accepts. `gpt-transcribe` itself is only mentioned in the context of the response-side `languages` field, not in the model list used in the request examples. **[IKKE FUNNET]** — a genuine matrix does not appear to exist on this specific page; may exist in a different llms.txt sub-page not yet fetched (out of remaining budget).

⚠️ Caveat: this was one fetch of one markdown-rendered page via an intermediate summarizing tool, not a raw curl of the primary source — treat the quoted phrases as reliable but the completeness of "not listed" claims as bounded by what that one fetch surfaced. No page timestamp was visible to check staleness.

## Source 2: Pricing

The primary source (`openai.com/api/pricing` and `openai.com/pricing`) returned **HTTP 403 Forbidden** on both attempted fetches — could not be reached directly within budget. **[IKKE FUNNET — primary pricing page inaccessible, blocked at 403]**.

Fallback: web search surfaced secondary/aggregator sources (Holori, Gate.AI, diyai.io, costgoat.com, spokenly.app, tokenmix.ai) converging on:

- **gpt-transcribe**: $0.0045/min [SANNSYNLIG — multiple aggregators agree, none is OpenAI itself; released per these sources on 2026-07-28]
- **gpt-4o-transcribe**: $0.006/min [SANNSYNLIG — aggregator consensus; one source frames it as $2.50/1M input + $10/1M output tokens converting to ~$0.006/min "as of July 2026", i.e. an estimate, not a quoted flat per-minute price]
- **whisper-1**: $0.006/min [SANNSYNLIG — aggregator consensus]

These numbers match your believed figures ($0.0045 / $0.006 / $0.006) but **none is confirmed against OpenAI's own pricing page** — that page was unreachable (403) within the 6-operation budget.

## Hull og usikkerhet

- Could not open `openai.com/api/pricing` or `openai.com/pricing` directly (403 both times) — pricing figures rest on secondary aggregators only, not primary confirmation.
- No per-model parameter support matrix found anywhere in the fetched llms.txt sub-page for gpt-transcribe specifically.
- Whether `text`/`srt`/`vtt` response formats still exist for any model (e.g. whisper-1) was not confirmed or denied — the page's headline enum (json/diarized_json/verbose_json) may describe only the newer models' behavior; not enough evidence to say those legacy formats were removed.
- No request-side `language`/`languages` parameter found in the fetched doc at all — only a response-side `languages` field for gpt-transcribe. Could not determine input parameter naming/type/optionality.
- `timestamp_granularities` compatibility with gpt-transcribe/gpt-4o-transcribe specifically: not found (only whisper-1 shown in examples).
- Did not check whether llms.txt has additional per-resource sub-pages beyond the transcriptions "create" method page (e.g. a realtime/keywords-specific page) — budget exhausted before that could be explored.
