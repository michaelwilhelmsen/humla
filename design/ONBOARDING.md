# Humla onboarding — decision record

Product spec for the first-run onboarding experience. Every decision below was made deliberately
(grilling session, 2026-07-04); rationale is recorded so future changes argue against the reasoning,
not just the conclusion. Target release: **v0.31.0**, built in the redesign language
(`design/REFACTOR.md` — Hanken, ink accent, inset cards).

## Why this exists

A fresh install today lands on an empty home screen with zero guidance, and the default
`transcribe_config` fallback points at OpenAI `gpt-4o-transcribe` with no API key — the first
record attempt fails. Permissions aren't checked upfront; they surface as errors mid-recording,
and screen recording additionally requires an **app restart** after being granted. Since the first
real use of a meeting-notes app is likely *during a live meeting* — the single worst moment to
discover missing setup, and a failed first recording is unrecoverable — onboarding must be
**complete-before-first-meeting**, not minimal-gate-with-deferred-setup.

## Core decisions

| # | Decision | Choice |
|---|----------|--------|
| 1 | Philosophy | Full setup wizard before first use; downloads parallelized in background |
| 2 | Trigger / escape | Skippable with persistent nag chip; existing installs grandfathered; resumable |
| 3 | Step order | Welcome → Permissions → Language → Transcription → AI Summary → Cloud → Summary screen |
| 4 | Transcription default | On-device (local Whisper) recommended; two-card fork; Intel Macs flip recommendation to cloud |
| 5 | Permissions | Mic required; screen recording skippable ("in-person only" framing); wizard owns the restart |
| 6 | Cloud depth | Full funnel (signup → workspace → Stripe checkout) with free-path default; cloud = teams only |
| 7 | Finale | Status summary screen (test recording was considered and **rejected** — see below) |
| 8 | Safety net | Live audio meter + "no audio detected" warning in the recording bar, every recording |
| 9 | Presentation | Full-window takeover route; soft progress bar (not dots, not "step N of M") |
| 10 | Nag chip | Sidebar, gold, "Finish setup"; shows download progress when that's the only gap |
| 11 | Copy | English-only wizard; single Welcome screen with the privacy line |
| 12 | Phasing | Wizard + grandfathering + nag chip + audio meter + re-run link ship together in v0.31.0 |

## Trigger, escape, and resume semantics

- **Trigger**: wizard renders when the `onboarding_completed` setting is unset **and** the DB looks
  fresh (zero notes, no configured provider beyond the migration default).
- **Grandfathering**: a startup migration writes `onboarding_completed=true` for existing installs —
  any notes exist, or any API key / downloaded model is configured. Nobody already using Humla ever
  sees the wizard.
- **Skippability**: every step has "Skip for now"; the wizard has an exit. Skipping is never silent —
  the nag chip (below) persists until the recording pipeline is functional.
- **Resume**: wizard progress persists in an `onboarding_step` setting (not React state), so the
  mid-wizard screen-recording restart resumes at the right step. All steps **write through to
  settings immediately** (no final commit) — quitting mid-wizard loses nothing, and re-entry
  pre-fills from live state for free.
- **Re-entry**: "Run setup again" link in Settings → About. Opens the wizard reading live state;
  doubles as the dev test harness for the wizard itself.

## Presentation

Full-window takeover: a guard in `App.tsx` renders `<Onboarding />` instead of `<Layout />` while
onboarding is incomplete. No sidebar, no top bar. **Soft progress bar** across the top (7 screens,
but three are near-instant; numbered steps make short flows feel long). Back navigation allowed.
"Skip setup" tucked in a corner. Rejected alternatives: modal-over-app (shows the empty app behind
a dialog, invites dismiss-and-poke-around, recreating the broken-default trap) and a separate Tauri
window (fights the restart-resume flow, doubles window management).

## The steps

### 1. Welcome

One screen, no carousel. Wordmark, one value-prop line, three quiet feature glyphs
(Record — mic + meeting audio · Transcribe — on your Mac or via API · Summarize — your notes + the
transcript, fused). Then the line that earns the next step's scary permission prompts:

> Private by default — your notes and audio stay on your Mac unless you choose otherwise.

Single CTA: "Get started."

### 2. Permissions

**First real step deliberately** — if screen recording forces a relaunch, better on step 2 than
step 6. One screen, two rows (Microphone / System audio), each with a status pill and action
button; reuse the guts of `src/components/Permissions.tsx` (which already re-polls on window
focus — the user returns from System Settings and the pill has flipped).

- **Microphone — required.** `permissions_request("microphone")` fires the native prompt.
- **System audio — skippable.** No native macOS prompt exists; deep-link to System Settings via
  `permissions_open_settings("screen")`. Skip button carries its meaning: *"Skip — I only record
  in-person meetings"* (mic-only is a fully supported mode: the diarizer runs on the mic stream).
  Consequence stated: remote call audio won't be captured; can be enabled later in Settings.
- **Trust copy** (screen recording is alarming for a notes app):
  > macOS bundles system-audio capture under "Screen Recording." Humla uses it to hear the other
  > side of your calls — it never captures your screen.
- **Restart owned by the wizard**: persist `onboarding_step` *before* showing a "Restart Humla"
  button (Tauri process relaunch API). Resume lands on step 3. The user must never discover the
  restart requirement on their own.

### 3. Meeting language

"What language are your meetings mostly in?" Writes the existing global `language` setting. Its
real job is upstream: the answer drives the step-4 model recommendation. **Not** the UI language —
see Copy below.

### 4. Transcription

The local-vs-cloud fork. **Two cards, not a provider list**:

- **Card A — "On-device" (recommended)**: private, free forever, works offline; honest size on the
  card. Selecting it starts the background download of the model chosen by the step-3 answer:
  Norwegian → `nb-whisper-large-q5` (~1.16 GB), everything else → `large-v3-turbo-q5` (~602 MB).
  Download runs through steps 5–7 (`whisper_model_download` + `whisper_download_progress` event).
- **Card B — "Cloud API"**: faster setup, no download, needs a key. Provider **dropdown inside the
  card** (OpenAI / Deepgram / Groq — the distinction is meaningless to a first-run user; don't
  force it to the top level), key field + existing `provider_key_test` Test button inline.
- **Intel check**: on non-Apple-Silicon, flip the "recommended" badge to Card B with a one-liner
  ("On-device transcription is slow on Intel Macs").
- **Diarize model**: silently kick off `diarize_download` (~30 MB + ANE compile) when this step
  completes, regardless of choice. Small, universally needed; asking would be noise.
- Whatever is chosen is written to `transcribe_config.default`.

**Out of scope**: per-language overrides (the routing table). Power-user feature; a closing line on
the summary screen points to Settings → Transcription.

### 5. AI Summary

Fork with asymmetric depth:

- **OpenAI** — if a key was entered in step 4, **preselect this** and make it one click
  ("Use the same key"). Otherwise the key field appears inline with Test.
- **Local (Ollama)** — guided sub-flow *within* the step (not a top-level step — most users reuse
  the OpenAI key and would see it as noise, but this is the highest-friction path in the wizard and
  must not fail silently in Settings later):
  1. **Detect first, instruct second**: on selection, probe `localhost:11434`. Already running →
     skip straight to model selection; never show install instructions to someone who doesn't need
     them.
  2. Not detected → short instructions: ollama.com download link + copy-button `ollama pull`
     command for the recommended model (the Qwen 3.5 variant the sampling profile is tuned for).
     A polling "Waiting for Ollama…" indicator flips to a check when the server appears — no retry
     button to mash.
  3. Detected → list installed models (existing `list_models` path), preselect the recommended one,
     else show its pull command with the same poll-until-present behavior.
- **Skip** — summaries stay unconfigured. Summary is *optional* in the nag logic (notes +
  transcripts work without it).
- If step 4 chose local Whisper: show both options **neutrally** with a one-line trade-off
  ("Local: private, needs Ollama + ~6 GB RAM · OpenAI: better summaries, needs API key"). Don't
  steer a privacy-minded user with a weak Mac into the heaviest setup.

### 6. Humla Cloud

**Cloud = teams only. Every cloud feature sits behind the workspace subscription.** Pitch line:
*"Humla is free on your Mac — Cloud is for teams: $7/mo after a 14-day trial."*

Fork screen; the **free path is the preselected default** (this is what keeps the step from
smelling like a paywall):

- **"Just me, on this Mac"** — preselected, one click onward. No account, nothing phoned home.
- **"Set up a team workspace"** — full funnel: inline signup (`cloud_signup`) → name workspace
  (`cloud_create_workspace`) → "Start free trial" opens Stripe Checkout in the browser
  (`billingCheckout`), wizard **polls `cloud_status`** until the subscription is live, then shows
  the workspace active. Rationale for going all the way through checkout: a workspace without a
  trial is **read-only** — an account-but-no-trial state is a dead end, not a milestone, so
  stopping at signup strands the user.
- **"I already have an account"** — sign in + select workspace (the joining-a-team case).
- Small **"self-hosted server"** text link (`cloud_configure`) — present but not a card.

Skippable at every sub-step; abandoned funnels degrade gracefully to the existing Organization tab
billing UI. **Cloud never nags** — the nag chip is strictly about the recording pipeline.

### 7. "You're all set" — status summary

*(A test recording was designed and rejected: it verified the real pipeline but made onboarding
feel bloated, and produced a throwaway note. The residual risk it covered — "permission granted but
no frames flow" — is covered better by the always-on audio meter below.)*

Checklist of rows, each with a status pill, each **clickable to fix**:

- **Microphone** — granted ✓
- **System audio** — granted ✓ / "Skipped — in-person only" (+ enable link)
- **Transcription** — provider + model; if the download is still running, a live progress bar in
  the row (user may finish the wizard; the nag chip covers the gap)
- **AI Summary** — provider, or "Not set up — add this later"
- **Humla Cloud** — workspace name, or "Local only"
- **Language** — e.g. "Norsk"

This screen is the wizard's receipt: it evaluates the **same condition function as the nag chip**
(one source of truth — "all green here" and "no nag" must be the same predicate).

Primary CTA: **"Create your first note"** — lands in a fresh note with the record button visible,
not on the empty home screen.

## Nag chip

- **Placement**: bottom of the sidebar, above Settings. Gold/warning token. Label: "Finish setup."
  Not a toast (vanishes), not a banner (annoys).
- **Trigger** (shared predicate with the summary screen): nag while
  `mic not granted OR no working STT path`, where working STT = local model fully downloaded, or a
  cloud provider whose key passed `provider_key_test` at save time.
  **Not** in the predicate: screen recording (skippable), summary (optional), cloud (never).
- **Download-progress variant**: while the only gap is an in-flight model download, the chip shows
  "Downloading model — 62%" instead of "Finish setup" (the user did everything right; nagging
  mid-download reads as a false accusation).
- **Click**: reopens the wizard at the **first incomplete step**, pre-filled from live state.
- **Record button is not hard-disabled** while the chip is live: hitting record with no working STT
  surfaces the same "Finish setup" path inline. A disabled button hides *why*; a click can explain.

## Audio meter + no-audio warning (the safety net)

In the recording bar, for **every** recording — this is what made dropping the test recording safe:

- Live audio-level indicator fed by the sidecar's existing `heartbeat` events (frame counts +
  peaks) — mostly frontend work.
- "No audio detected" warning if mic frames stay at zero for the first ~10 seconds of a recording.
- Continuous verification beats one-shot: it protects every meeting, not just the first.

## Copy

- **English-only wizard**, matching the app. Step 3 sets *meeting* language — never couple it to UI
  language (Norwegian-meetings + English-UI users exist; half-localized UI is worse than none). If
  Humla ever localizes, it's a whole-app effort keyed off `navigator.language`, separate project.

## Explicitly deferred / out of scope

- Contextual "no system audio during what looks like a call" one-time hint (post-v1 enhancement).
- Per-language routing configuration in the wizard (Settings-only, forever).
- Any UI localization.
- Summary verification in onboarding (summary failures are retryable; capture failures are not).

## Implementation anchors

| Piece | Existing code |
|---|---|
| Permission check/request/deep-link | `src-tauri/src/commands/permissions.rs` (`permissions_status`, `permissions_request`, `permissions_open_settings`); frontend `src/components/Permissions.tsx` |
| Whisper model registry + download | `src-tauri/src/local_whisper.rs` (registry, lines ~58–104); `whisper_model_download` / `whisper_download_progress`; UI pattern in `src/pages/settings/components/LocalModelManager.tsx` |
| Diarize download | `src-tauri/src/commands/models.rs` (`diarize_download`, `diarize_download_progress` with listing/downloading/compiling phases); UI pattern in `DiarizeModelManager.tsx` |
| API keys | `src-tauri/src/commands/api_keys.rs` (`provider_key_get/set/test`); `src/pages/settings/components/ApiKeyField.tsx` |
| Transcribe config | `src-tauri/src/commands/transcription_config.rs` (`get/set_transcribe_config`); broken fallback default lives in `src-tauri/src/stt/config.rs` |
| Cloud | `src/pages/settings/tabs/Account.tsx`, `Organization.tsx`; `src/lib/cloud.ts` (`HUMLA_CLOUD_URL`); `cloud_status/signup/login/configure`, `cloud_create_workspace`, `billingCheckout`, `billingPortal` |
| Routing guard | `src/App.tsx` (routes; wizard renders instead of `<Layout />`) |
| Heartbeats for the meter | audio-capture sidecar `heartbeat` stdout events (frame counts + peaks), parsed in the Rust reader thread |
| Settings keys to add | `onboarding_completed`, `onboarding_step` |

## Release plan

Everything ships together in **v0.31.0**: wizard, grandfathering migration, nag chip, audio meter +
no-audio warning, "Run setup again" link. Rationale against splitting: the meter is the insurance
that justified cutting the test recording — shipping the wizard without it ships the hope, not the
insurance; the rest is small and inseparable.
