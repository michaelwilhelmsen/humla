# embeddinggemma multilingual sanity check (issue #48)

**Status: run 2026-07-22 — 6/6 pass across en/nb/de/es + cross-lingual. Ship as-is.**

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

## Results

Run 2026-07-22, `embeddinggemma:latest` via Ollama `/v1/embeddings` (768 dims).
**6/6 cases pass** — in every language the intended (semantic, non-lexical)
passage is the top cosine match over its distractors. Raw output in
`findings-run.json`.

| Lang     | Correct sim | Best distractor | Margin |
|----------|-------------|-----------------|--------|
| en       | 0.581       | 0.241           | 0.340  |
| nb       | 0.476       | 0.381           | 0.095  |
| nb       | 0.440       | 0.324           | 0.116  |
| de       | 0.811       | 0.289           | 0.522  |
| es       | 0.613       | 0.264           | 0.349  |
| en→nb    | 0.512       | 0.362           | 0.150  |

**Verdict: acceptable, ship as-is.** Norwegian and English both retrieve
correctly, and cross-lingual (English query → Norwegian passage) works — so a
bilingual note set is served by a single embedder. Observations:

- **Norwegian margins are the thinnest** (~0.10 vs English's 0.34, German's
  0.52). Still a clean win over distractors, but Norwegian passages sit closer
  together in the space than the Latinate languages do. Not weak enough to act
  on — RRF's keyword half backstops it, and absolute cosine isn't comparable
  across languages anyway (only the ranking matters).
- Per the fixed decision framework: **no model swap**. If Norwegian recall ever
  disappoints in real use, tune retrieval first (smaller/overlapping chunks so a
  specific passage isn't diluted; an instruction prefix on the query if the
  embeddinggemma card recommends one; RRF `k`).

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
