# Spike #45 — local Ollama tool-calling reliability

Empirical probe for the flagged go/no-go risk in PRD [humla#42](https://github.com/michaelwilhelmsen/humla/issues/42) (story 12, "fully local, just as good as cloud"): does a **local** model driven through Ollama's native `/api/chat` reliably execute the retrieval tool-calling loop the agentic chat feature (#47) depends on?

- **Date:** 2026-07-22
- **Method:** `probe.py` (in this dir) drives `/api/chat` with the three planned retrieval tools (`search_notes` / `get_note` / `list_notes`, with `folder_id`+`client_id` filters) over a fixed 7-note corpus, running the real tool-calling loop (execute tools → feed results back → continue), step-capped at 6 with tools dropped on the final step. 8 queries: single-lookup, multi-step synthesis, and a deliberately empty/nonexistent-topic query.
- **Scored per query (all four must hold to be "clean"):** (1) well-formed tool calls, (2) right tool + sane args, (3) tool results consumed into a grounded answer, (4) terminated without hitting the step cap mid-loop. Full transcript: `transcript.json`.

## Verdict: **GO** ✅

Local tool-calling is reliable enough to build #47 on. Both models emit well-formed tool calls, pick the right tool with sane arguments, consume results into grounded, note-cited answers, and terminate on normal queries. The "fully local, just as good" promise is viable — with two required loop mitigations (below).

## Scorecard

| Model | Clean | Wall time | Notes |
|---|---|---|---|
| **gemma4:12b-mlx** | **7 / 8** | ~103 s | Strongest. Handles open-ended synthesis ("connect the dots") in 2–3 steps. MLX build is faster than the 9B GGUF despite being larger. |
| **qwen3.5:9b** | **6 / 8** | ~160 s | Solid on lookups; **loops to the step cap on open-ended synthesis** ("connect the dots") as well as the empty query. |

(Deterministic-ish at `temperature 0`; scores reproduced across separate and combined runs.)

## The one shared failure mode — giving up on empty results

**Both models fail the same query**: asked about a topic that doesn't exist in the notes ("the Zephyr quantum protocol"), they **keep issuing search variations until the step cap (6)** instead of concluding "I couldn't find it," and on the forced final step gemma returned an **empty** answer. qwen additionally loops on broad synthesis.

This is a loop-control problem, not a tool-calling-competence problem — and it's fixable in #47's loop design:

1. **System-prompt the give-up rule:** "If a search returns no matches, do not retry variations — conclude you couldn't find it." Loop-reluctance is the root cause.
2. **Make the final-step wrap-up mandatory-text:** the forced-text step must explicitly elicit an answer ("You can no longer search; answer from what you have, or state you couldn't find it") — gemma returned empty because the wrap-up didn't compel text.
3. **Consider a lower step cap (4–5)** and short-circuiting after two consecutive empty searches, so failure is fast and cheap.

These are cheap, and they're the difference between 6–7/8 and near-perfect.

## Tiered model recommendation

- **High-RAM pick (≥24–32 GB Macs): `gemma4:12b-mlx`.** 7/8, fastest, best at synthesis. This is the current dev machine's model and the recommended default for capable hardware.
- **16 GB-friendly default: UNRESOLVED — needs a follow-up test.** The current `RECOMMENDED_OLLAMA_MODEL` is `qwen3.5:4b`, which was **not installed** and so **not tested here**. `qwen3.5:9b` (a size *up* from the 4B default) already shows synthesis loop-weakness at 6/8, so the 4B tier is likely at or below that and must be validated empirically before we name a confident 16 GB default. Re-run `probe.py qwen3.5:4b` once it's pulled.

## Reproduce

```
ollama pull qwen3.5:4b   # to close the 16 GB-tier gap
python3 docs/research/spike-45-ollama-tool-calling/probe.py qwen3.5:4b qwen3.5:9b gemma4:12b-mlx
```

## Bottom line for #47

Build the agentic loop — local tool-calling is proven. Bake in the empty-result give-up rule + a mandatory-text final step from day one (both models need it). Recommend `gemma4:12b-mlx` for capable Macs; keep `qwen3.5:4b` as the 16 GB default only after the pending empirical check confirms it clears the bar.
