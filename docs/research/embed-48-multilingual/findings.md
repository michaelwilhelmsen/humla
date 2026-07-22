# embeddinggemma multilingual sanity check (issue #48)

**Status: harness ready; empirical run pending a local `embeddinggemma` pull.**

This is the *non-blocking multilingual sanity check* the #48 acceptance criteria
ask for. It is deliberately a sanity check, not a benchmark: `embeddinggemma` is
Humla's **sole** local embedder, with **no fallback model**. If a language looks
weak, the lever is retrieval tuning (chunk boundaries, RRF `k`, query
formatting) — **never a model swap**. So the goal here is only to confirm the
embedder is "good enough" across the languages Humla users actually write in,
not to rank it against alternatives.

## Method

`probe.py` embeds query + passage sets via Ollama's OpenAI-compatible
`/v1/embeddings` (the same wire path the app uses) and checks, per case, that
the intended passage is the top cosine match over plausible distractors. Cases
are **semantic, not lexical** — the correct passage shares meaning but not the
query's keywords, so a pass demonstrates something keyword search alone can't do.

Coverage: **Norwegian (Bokmål)** and **English** as the primary targets, plus a
spot of **German** and **Spanish**, and one **cross-lingual** case (English
query → Norwegian passage) since real notes mix languages.

## How to run

```
ollama pull embeddinggemma       # ~600 MB, one-time
python3 docs/research/embed-48-multilingual/probe.py
```

It writes `findings-run.json` (per-case cosine of the correct passage vs the
best distractor) and exits non-zero if any case fails.

## Why this wasn't auto-run in the implementing session

`embeddinggemma` was not present on the dev machine, and the check is explicitly
**non-blocking** for the slice — semantic retrieval degrades gracefully to
keyword-only when the model is absent, so shipping #48 does not depend on it.
Pulling a 600 MB model onto the user's machine without asking wasn't warranted
for a sanity check. Run the command above to record results (paste the
`findings-run.json` summary into the "Results" section below).

## Results

_Pending — run `probe.py` and record the per-language pass/fail + cosine
margins here._

## Decision framework (fixed regardless of results)

- **All languages pass** → ship as-is; retrieval is already good enough.
- **A language is weak** (correct passage not top, or thin margin over
  distractors) → tune retrieval, in this order: (1) query formatting (e.g. an
  instruction prefix if the model card recommends one), (2) chunk boundaries
  (smaller/overlapping chunks surface a specific passage), (3) RRF `k` and pool
  size in `hybrid_search_chunks`. Keyword (BM25) already backstops semantic
  misses via RRF, so a weak language still returns literal matches.
- **Never** add a second embedding model — that reintroduces the fixed-dim-
  per-index and re-embed-on-switch complexity #48 deliberately avoids.
