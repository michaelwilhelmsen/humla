# Settings PRD triage

Triage of the settings-related PRDs in GitHub issues (`michaelwilhelmsen/humla`), verified claim-by-claim against the codebase on `main` (post-v0.31.1). Date: 2026-07-08.

**PRDs in scope:**

| # | Title | Status | Size (1–2h evenings) | Verdict |
|---|---|---|---|---|
| [#13](https://github.com/michaelwilhelmsen/humla/issues/13) | Settings refactor (1/2): modal shell, IA, and control kit | ready | 7–10 | **Build now** |
| [#15](https://github.com/michaelwilhelmsen/humla/issues/15) | Settings refactor (2/2): provider & model surfaces | ready (sequenced behind #13) | 6–8 | **Build after #13** |
| [#14](https://github.com/michaelwilhelmsen/humla/issues/14) | Cleanup: retire legacy + unsurfaced settings keys | ready, but scope overstated | ~1 | **Build with/after #15, after scope correction** |

Non-settings PRDs (#16–#21) were located but not triaged here.

**Recommended order: #13 → #15 → #14.** Total ≈ 14–19 evenings. Nothing should be dropped or merged; the three-way split is well-drawn and each issue explicitly references the boundary with the others. No duplicate PRDs exist — the "two settings PRDs" from the roadmap are a deliberate 1/2 + 2/2 split, plus #14 as the severed backend cleanup.

---

## #13 — Modal shell, IA, and control kit

**Status: ready. No hard blockers — buildable today.**

The PRD is complete (goals, non-goals, resolved decisions, tab-id compat mapping, migration order, acceptance criteria) and its codebase claims verify down to line numbers. Critically, its stated dependency — `design/REFACTOR.md` Phase 1 tokens — **already shipped**: the redesign merged as v0.30.0 (Hanken via `@fontsource` in `src/main.tsx`, tokens in `src/styles/globals.css`).

**Scope:** convert Settings from a full-screen route (`src/pages/Settings.tsx`, 7 flat tabs) to a route-backed modal (`/settings` + `?tab=` stay canonical); reorganize into 5 sections (Recording · Transcription · Summaries · Account · General) with legacy tab-id redirects; build a reusable control kit (`Row`, `Toggle`, `Segmented`, `ValuePill`, `Disclosure`, restyled `Section`/`Select`/`Btn`); rebuild Recording, Account(+Organization), General(+About); wrap Transcription/Summaries bodies as-is (rebuilt in #15).

**Corrections needed against the codebase:**

- **"Ink accent" is stale.** The shipped redesign uses gold (`--color-accent: #ffdc6c`, commit `17e3e71`); `--color-ink` is only an alias of `--color-text`. Implement using shipped token vocabulary — this repo has had multiple button-invisibility bugs from token-name drift.
- `Row` does not exist in `src/pages/settings/components/` despite the PRD saying "upgrade existing" — it must be created. `Modal.tsx` (51 lines) does exist and is the right base for the dialog shell.
- The PRD implies but doesn't define the fallback background when `/settings` is opened directly (relaunch, Toaster link right after load) — there's no "previous view" to dim behind the modal. Decide up front: fall back to Home.

**Riskiest chunk:** the Account + Organization merge (594 + 257 lines) — it carries cloud-sync/billing UI states (pending sync, view-only, recording lock). The acceptance criteria do call these out.

**Milestones (7–10 evenings):** dialog shell + focus trap + entry points (2) → control kit (2) → Recording + General(+About) (1–2) → Account+Org merge (2) → tab-id mapping, wrap legacy bodies, light/dark + recording-lock QA (1–2).

---

## #15 — Provider & model surfaces

**Status: ready in content; hard-sequenced behind #13's kit. Zero backend work required.**

Every claim verifies: the standalone keys tab exists (`src/pages/settings/tabs/ApiKeys.tsx`); the duplication with onboarding is real (`src/pages/onboarding/steps/Transcription.tsx` reimplements key save/test and model download; `steps/Summary.tsx` reimplements the Ollama probe); and the backend already supports everything — `provider_key_get/set/test` in `src-tauri/src/commands/api_keys.rs` with the `"stored"` sentinel fits a masked shared card as-is.

**Scope:** rebuild Transcription + Summaries bodies with the #13 kit; kill the keys tab and inline keys under each provider picker; extract shared `ProviderKeyCard` / `ModelDownloadCard` / `OllamaConnect`; put per-language routing, model managers, and thresholds behind an Advanced disclosure; relocate default language + default summary preset out of General. Onboarding migration is explicitly a fast-follow, not in scope.

**Key risks and decisions:**

- **Extraction must copy, not move.** Onboarding (`src/pages/onboarding/steps/`) just went through fresh-install bug rounds #9–#12 and must stay byte-identical until the fast-follow migration.
- **`ModelDownloadCard` must derive completion/failure from events** (`local_whisper_progress` / `local_whisper_download_error`), never the invoke promise — the promise dies with the mount. Same for diarize download. And it must never call `setupStatus`-style sidecar checks per progress tick — diarize status spawns a `speaker-diarize` process per call; per-engine cards must not multiply polling.
- **Kit placement:** the PRD says `src/pages/settings/components/`, but a kit onboarding will later consume shouldn't live under `pages/settings/` — prefer `src/components/provider/`. Cosmetic; decide at implementation.
- **`?tab=keys` deep links** need a redirect once the tab dies (interim mapping is defined in #13).
- **File the onboarding-migration fast-follow issue when #15 lands** — it's referenced but doesn't exist yet.

**Independent head-start:** the three shared cards don't need the modal shell and can be extracted before #13 finishes if evenings free up — this also de-risks #13's estimate.

**Milestones (6–8 evenings after #13):** ProviderKeyCard + ModelDownloadCard (1–2) → OllamaConnect (1) → Transcription body (2) → Summaries body + General relocations + delete keys tab (1) → end-to-end verification, both themes, local + cloud paths (1–2).

---

## #14 — Retire legacy + unsurfaced settings keys

**Status: ready, but materially smaller than written. Needs a scope-correction comment before anyone picks it up.**

Verified key-by-key, the issue overstates the work in one direction and understates risk in another:

- **Six of the seven "legacy" keys are already fully retired.** `transcribe_provider`, `transcribe_model`, `deepgram_model`, `groq_model`, `local_whisper_model`, `whisper_preset` (plus `local_whisper_use_gpu`, which the issue omits) are referenced only by the one-shot `migrate_transcribe_config` in `src-tauri/src/db.rs` — which the issue correctly says to keep for old-DB upgraders — and by test fixtures. **Zero product code to remove.** (The `LocalModelManager.tsx` hit is an HTML radio-group `name` attribute, not a settings key.)
- **`summary_prompt` is NOT dead.** Live read at `src-tauri/src/commands/summary.rs:126` — it's the fallback prompt for legacy `"custom"`-sentinel notes and for notes whose `custom:<id>` prompt row was deleted. Removing it silently downgrades those notes to `DEFAULT_SUMMARY_PROMPT`. Probably acceptable, but it's a behavior decision, not a mechanical delete. Note `migrate_summary_prompts` deliberately leaves the row for rollback safety (documented in db.rs) — removing readers is fine; deleting the row contradicts that rationale.
- **`sync_audio` deserves UI, not deletion.** Sole read is `src-tauri/src/commands/cloud.rs:990` (`upload_note_audio`); nothing ever writes it, so meeting audio uploads to the workspace by default with no visible opt-out. That's privacy-adjacent for cloud/teams users — surface a toggle in the Account section (needs #13's kit) rather than folding to a constant.
- **`diarize_clean_segments`** is a deliberate SQL-only dev A/B toggle (`src-tauri/src/commands.rs:805–815`, documented in a code comment). Keep or fold to a constant — a 15-minute call either way.
- Migration flags (`summary_prompts_migrated`, `migrated_transcribe_config_v3`) and both migration functions must survive; orphaned rows in user DBs are harmless.

**Sequencing:** after #13 (the `sync_audio` toggle wants the kit + Account section), alongside or just after #15 (which reshapes `useSettings.ts` and the Summaries body — doing #14 first creates merge churn). The purely backend slice (summary.rs fallback + `SettingsKey` union trim) is independent and could land any time.

**Size: ~1 evening**, possibly under an hour if the `sync_audio` UI rides with #15.

---

## Cross-PRD findings

- **No contradictions between the three.** Boundaries (shell/kit vs. bodies vs. backend keys) are drawn consistently in all three bodies; #13 promises no backend changes, #15 defers keys work to #14, #14 says frontend refactors shouldn't carry migration risk.
- **Shared inaccuracy:** #13 (and `design/REFACTOR.md`) use "ink accent" terminology that predates the shipped gold accent. Whoever builds should treat `globals.css` on `main` as truth, not the PRD's colour words.
- **Soft future conflict:** #21 (menu bar + hotkey) will add settings to the Recording section — #13's IA table already anticipates this; keep the section layout roomy.
- **Labels:** all three still carry `needs-triage`. Suggested: #13 → `ready-for-agent` (or `ready-for-human`); #15 → same, noted as blocked-by #13; #14 → `needs-info` until the scope-correction comment lands, then `ready-for-agent`.

## Recommended plan (solo, 1–2h weekday evenings)

1. **Start #13 immediately** — shell + kit first (highest-leverage milestone; everything else consumes it). ~7–10 evenings.
2. **Optionally interleave** the #15 shared-card extraction (copy-only, onboarding untouched) on evenings when #13's Account merge feels too big to open.
3. **#15 after #13 merges.** ~6–8 evenings. File the onboarding-migration fast-follow when it lands.
4. **#14 last**, ~1 evening, after posting a scope-correction comment (six keys already dead; `summary_prompt` is a live fallback; `sync_audio` gets a toggle, not deletion).

**Kill list: nothing.** All three survive triage; #14 just shrinks.
