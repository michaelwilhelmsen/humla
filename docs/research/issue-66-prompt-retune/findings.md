# #66 / humla-cloud#20 — does the prompt retune change behaviour?

**Date:** 2026-07-26 · **Model:** `gemma4:12b-mlx` via Ollama · **Reps:** 3 · `temperature=0`
**Harness:** `probe.py` (this directory). Corpus and tool schema imported from the spike #45 probe, unmodified.

The retune's two claims — bounded search retries, and licensed labeled inference — are claims about *model behaviour*. Unit tests on the prompt string can't confirm them, so this ran the real OLD (`main`) and NEW (`feat/chat-prompt-retune-66`) prompt strings head-to-head.

## Results

Aggregated over 3 reps. Both prompts run in the same process, same order, per rep.

| Probe | old | new | Reproducible difference |
|---|---|---|---|
| **A-recall** — Norwegian query, answer only reachable in English | 3/3 pass · **0 searches** · never caps | 3/3 pass · **2 searches** · **caps every rep** | new retries, then burns the step budget |
| **B-inference** — answer not in any note; requires weighing two clients | 0/3 labeled | **1/3** labeled | not reliable |
| **C-factual** — "what did we decide about Acme's pricing?" | 3/3 pass | 3/3 pass | none — no regression |
| **D-truly-empty** — a topic genuinely absent from the corpus | 0/3 · **4 searches** | 3/3 · **3 searches** | new searches slightly less |

## What the data supports

**1. The old ban did not do the job it was kept for.** This is the load-bearing finding. The sentence "do not keep retrying with tweaked queries" existed to stop small models spinning — but under it, `gemma4:12b-mlx` issued **four** searches on a genuinely-empty query, in all three reps (`Zephyr quantum protocol` → `Zephyr` → `quantum protocol` → `protocol`). It never obeyed. Removing it therefore costs approximately nothing on this model, and the new prompt actually settles at three. The premise that the ban bounded the spin was false here.

**2. No factual regression.** Probe C passed 3/3 under both. The extra ~40 words did not degrade straightforward grounded recall, which was the main risk given the prompt's own warning that small thinking models re-litigate long constraint lists.

**3. Labeled inference is NOT reliably produced on a 12B local model — 1/3.** When it landed, it landed well: explicit `Dokumentert status:` versus `Inferred risk:` / `Inferred opportunity:` / `Inferred strategy:` sections, across three ranked clients, in 3 steps rather than hitting the cap. That is exactly what humla-cloud#20's probe B asks for. But two of three reps produced an unlabelled recommendation instead. **cloud#20's probe B is not satisfied for the local provider.**

<details>
<summary>The one rep where it worked (rep 1, new prompt, abridged)</summary>

> ### 1. Initech (Høyeste prioritet - Retensjon)
> * **Dokumentert status:** Kunden er misfornøyd på grunn av nylig nedetid og er nå identifisert som en **churn-risiko**.
> * **Inferred risk:** Hvis dette ikke håndteres umiddelbart, risikerer dere å miste en eksisterende kunde.
>
> ### 2. Globex (Vekstmulighet)
> * **Dokumentert status:** Sikkerhetsgjennomgangen er bestått, og budsjett for 40 plasser er godkjent.
> * **Inferred opportunity:** … dette er en "varm" lead med klart budsjett …

Compare the old prompt on the same question, every rep: a two-line recommendation naming Initech, no labelling, no comparison, at the step cap.

</details>

**4. A measurable step-budget cost.** On probe A the new prompt consistently issues a second search and then hits `MAX_STEPS`, where the old prompt went straight to `list_notes` + `get_note` and finished in 4 steps. Both reach the right answer, so it's cost rather than breakage — and #81 is already raising the step budget for the note-less scope. Worth re-measuring after that lands.

**5. Both prompts hit the cap on hard queries.** Pre-existing, not caused by this change, and visible on `main`.

## Recommendation

**Ship the retune as-is; do not condition it on model class yet.** The rationale in cloud#20 was always about GPT-5-class behaviour ("on a GPT-5-class model it forbids the one cheap retry that recovers recall"), and this probe only tests the local provider. What it establishes is that the change is *safe* locally — no factual regression, and the ban it removes was being ignored anyway.

#66 offers conditioning the sentence on provider/model class as the fallback "if a bounded retry proves too loose". The data doesn't support pulling that lever: retries got *fewer*, not more. If anything the local weakness is the inference labelling (1/3), and conditioning wouldn't fix that — a bigger local model or an explicit output-shape instruction would, and the latter contradicts the terse-prompt discipline.

## Limitations

- One model, one 7-note corpus, 3 reps. Ollama is not bit-deterministic even at `temperature=0`.
- `probe.py` replaces the spike's substring matcher with whole-word matching. Without that, an empty search is nearly unreachable — `"om"` matches inside `"compliance"` — and probe A was vacuous in the first draft.
- The harness must inject `MAX_STEPS_PROMPT` on the final step, as the real loop does. Omitting it produced **empty answers** that looked like model failure and were the harness's fault. Fixed; noted because it would mislead anyone re-running this.
- Probe B's `labeled` check is keyword-based, so it can undercount a correctly-reasoned answer that labels its inference in other words. Read the transcripts before trusting a FAIL.
- **The cloud-side probes remain unrun.** They need the retune deployed to a chat-service plus a real GPT-5-class model, which is where the benefit is actually claimed. Nothing here substitutes for that.
