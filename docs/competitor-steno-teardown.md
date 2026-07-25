# Steno — competitive teardown

**Date:** 2026-07-25
**Subject:** [stenoai.co](https://stenoai.co/) · [github.com/ruzin/stenoai](https://github.com/ruzin/stenoai)
**Verdict:** Closest competitor Humla has. Not a hobby project — venture-backed. Beats Humla on chat presentation, breadth of polish, and shipping velocity. Loses to Humla on transcript quality (real diarization, per-language routing), chat substance (actual retrieval + citations), and self-serve team pricing.

---

## 0. Executive summary

Steno is what Humla looks like with a funded team behind it. Same category, same values, near-identical marketing copy, aimed at a different buyer.

Three things matter:

1. **Steno is sponsored by GitLab founder's Open Core Ventures.** This is not a solo evening project. 16 contributors, 535 commits, ~30 commits in the last 3 days, PR-based workflow with CI and nightly e2e tiers, issue numbers in the 400s, a Discord, a Mintlify docs site, an Astro marketing site with programmatic comparison and industry pages, and a paid Enterprise tier being packaged. Treat their roadmap as a well-resourced one.
2. **Michael's read is correct: their chat UX is genuinely better.** But it is better *presentation over a weaker engine*. Steno has no retrieval at all — their own source comment says `We don't have retrieval (RAG) yet`. Library-wide chat concatenates summaries newest-first until a char budget fills, then drops the rest. No embeddings, no FTS, no citations. Humla has FTS5 + semantic hybrid retrieval, a 6-step agentic tool loop, and clickable citation chips. **Humla should steal the chrome, not the architecture.**
3. **The single biggest structural gap is not visual — it is that Humla's chat has no home.** `ChatPanel` is only mounted inside `Note.tsx`. There is no `/chat` route. To ask a question about your whole library you must first open some arbitrary note. Steno has a dedicated `/chat` entry page with a greeting, presets, and a Recents list. That is the gap that makes their chat feel like a product and Humla's feel like a tab.

**Do this week:** add the 18 missing GitHub topics and fix the stale repo description (10 minutes, free discovery). Ship a `/chat` route reusing `ChatPanel`. **Do this month:** composer scaffolding (presets via `/`, model indicator, stop button) and a `docs/compare/` SEO surface. **Ignore:** their gov/defense/sovereign-AI positioning — that is their moat, not a market Humla can take.

---

## 1. Scoreboard

| | Humla | Steno |
|---|---|---|
| Stars | 156 | 1,189 |
| Forks | 13 | 163 |
| Repo age | 3 months (2026-04-27) | 11 months (2025-08-19) |
| Stars/month | ~52 | ~108 |
| Contributors | 1 | 16 |
| Commits (last 30d) | 115 | ~150 |
| Releases | 30 | 40+ (v0.6.4) |
| Open issues | 7 | 51 |
| License | MIT | MIT |
| Backing | none | Open Core Ventures (GitLab founder) |
| GitHub topics | **0** | 18 |
| Stack | Tauri 2 + Rust + Swift sidecars | Electron + Python + Node |
| App footprint | Rust binary, ~547 MB model | Electron + Python + 4.3 GB Ollama model min |
| Platforms | macOS (Apple Silicon) | macOS (Apple Silicon) + Windows alpha |
| Monetization | $5/seat/mo self-serve cloud sync | Free OSS + unpriced Enterprise tier |

Note the rate column. On a stars-per-month basis Steno is ~2× Humla, not ~8×. Humla is three months old. The gap is real but it is not the rout the absolute numbers suggest.

---

## 2. Feature-for-feature

### Where Steno genuinely beats Humla

**Chat UX.** Covered in depth in §3. Two surfaces (inline ask bar per note + dedicated `/chat` route), preset "skills" via `/`, typewriter placeholder, Tab-to-accept, time-bucketed history, virtualized message list, streaming stop button, scope + model + honesty hint in the composer footer. Humla has one surface, no presets, no stop button, no model indicator, no route.

**Windows.** Alpha, but the full pipeline is verified working including system-audio loopback with channel attribution. Humla is macOS-only with no Windows path (Tauri could, but the Swift sidecars could not — that is an architectural commitment Humla has already made).

**Auto start/stop meeting detection.** Steno notices a meeting starting and offers to record, then offers to summarize when it ends. Calendar-connected. Humla requires a manual start. This is the highest-value feature Steno has that Humla lacks, and it is the one users notice daily.

**Live transcription during recording with a real engine.** Parakeet TDT v3 via MLX gives them on-screen text as you speak. Humla transcribes VAD-bounded chunks and streams them in, which is close, but Parakeet is purpose-built for streaming and their live view is a first-class UI (`LiveTranscriptBar.tsx`, 472 lines).

**Docs.** A full Mintlify site: getting-started, features, guides, models, privacy, comparisons, FAQ, changelog, and an `llms.txt` for AI-citation SEO. Humla has `CLAUDE.md` and a handful of design docs. Steno's docs are a marketing asset; Humla's are internal.

**Report templates.** User-defined report styles, multiple reports per note, switchable in the detail view. Humla has presets (Meeting / 1:1 / Lecture / …) plus custom prompts, which is the same idea, but Steno's are surfaced as first-class named artifacts attached to the note rather than a one-shot summary regeneration.

**Settings ⌘K search.** Shipped 2026-07-24. Humla's settings refactor PRD is still open. Minor, but it is the kind of polish that reads as maturity.

**Granola migration skill.** `granola-to-steno` backfills a user's Granola history. A conversion-path asset Humla has no equivalent for.

### Where Humla genuinely beats Steno

**Real speaker diarization. This is the big one.** Steno's "speaker labels" are channel attribution only — `[You]` for mic, `[Others]` for system audio. Verified in `src/transcriber.py` and their own docs: *"[You] / [Others] via stereo-channel diarization"*. That means **for an in-person meeting Steno labels every human in the room `[You]`.** Four people around a table produce one undifferentiated speaker. Humla runs FluidAudio (community-1 + VBx, or Sortformer) offline over `mic_full.wav` after stop and emits distinct `Speaker 1:` / `Speaker 2:` labels, plus rename with colour-coded pills.

> **Correction (added after review).** An earlier draft of this section called the gap "structural, not a missing polish pass". That was overstated — see §4 and §6. Steno already ships Parakeet via MLX, and diarization models are commoditizing (our own watchlist has Parakeet as a Sortformer swap candidate). Adding a diarization engine is a bounded project for a team of 16. Treat this as a **lead measured in months, not a moat** — and specifically, do not build a marketing campaign on it as a durable advantage.

**Chat substance.** Humla: `search_notes` (FTS5 keyword + semantic embeddings), `get_note`, `list_notes`, a 6-step agentic loop (`MAX_STEPS: 6`), per-conversation persisted breadth, clickable citation chips that navigate to the source note, live tool-activity lines, and past-tense tool receipts ("Searched your notes · Read 2 notes"). Steno: newest-first concatenation of title + summary + key points + action items until a char budget fills. **Steno's library-wide chat never sees a transcript at all** — only the per-note ask bar includes one. Ask Steno "what exactly did Anna say about pricing" across your library and it structurally cannot answer.

**Per-language transcription routing.** `transcribe_config` resolves per-note language → per-language override → default across four STT providers. Norwegian → local NB-Whisper, English → Deepgram Nova-3, default → OpenAI. Steno picks one engine globally (Parakeet, or Whisper for the 74 languages Parakeet lacks). For a Nordic user this is a decisive Humla advantage.

**Native footprint.** Tauri + Rust vs Electron + a Python sidecar. Humla's `whisper-rs` with Metal runs a 547 MB model; Steno's minimum viable install is ~670 MB transcription + 4.3 GB Ollama. Their default summarizer is a hard Ollama dependency. Humla's summary path also speaks Ollama but supports cloud OpenAI-compat and does adaptive `num_ctx` sizing with `keep_alive: 0` — meaningfully better behaviour on tight Macs.

**Self-serve team sync.** $5/seat/mo, live Stripe, R2 audio, deployed. Steno's team story is an enterprise "adapter" you sign into on a managed deployment, with pricing "coming soon". For a 3-person consultancy Humla is buyable today and Steno is a sales call.

**Code health.** `simple_recorder.py` is a **224 KB single file** with a 3,500-line CLI surface. Humla's backend is a module map with `commands/` split into cohesive groups, `stt/` behind a `BatchSttAdapter` trait, and typed config. Not user-visible, but it means Humla can change transcription providers in an afternoon and Steno cannot.

**Cross-chunk hallucination guards.** `is_likely_hallucination`, `strip_attribution_tail`, repetition-collapse, cross-chunk loop guards, per-source `prior_context` trails, RMS silence gating. Steno has some of this in the summarizer (`trim_runaway_repetition` equivalent) but Humla's chunk-level guard stack is more developed.

### Where they're effectively identical

Bot-free system-audio capture · on-device transcription · notes-fused-with-transcript summaries · MIT license · macOS Apple Silicon focus · BYOK cloud models (OpenAI/Anthropic/…) · Markdown ownership and export · folders/scoping · local-first data layout · no account required · free forever for the local app.

The marketing copy is close to interchangeable. Steno: *"the privacy-first AI notepad for all your confidential conversations."* Humla: *"The open-source AI notepad for Mac."* Both lead on no-bot + local + open. A visitor who lands on both sites in one session will struggle to articulate the difference — which is the actual strategic problem, not any single feature.

---

## 3. Deep dive: the chat UI/UX

Michael's instinct is right, and it's worth being precise about *why*. Steno's chat wins on five specific patterns. None of them are hard. All of them are presentation.

### 3.1 Two surfaces, one engine

Steno ships chat twice, sharing `useChatSessions`, `useGlobalStreaming`, `renderMarkdown` and `PRESETS`:

- **`AskBar.tsx`** (729 lines) — a floating composer docked at the bottom of every note. Note-scoped. Collapsed to a single input; expands upward into a 360 px message panel on focus.
- **`routes/Chat.tsx`** (553 lines) + **`routes/ChatConversation.tsx`** (599 lines) — a dedicated library-wide chat with its own entry page and per-conversation URLs (`/chat/<id>`).

Humla has only the first, and it isn't even floating — it's a tab inside the note pane. **The `/chat` route is the gap.** It gives chat a home in the sidebar, a landing surface, a URL per conversation, and a reason to exist independent of any note.

### 3.2 The entry page is a real landing surface

`Chat.tsx` renders a centred 640 px column: a 22 vh top spacer, then a 36 px serif greeting — `Hi {FirstName}, ask anything`, name italicised — then a glass-effect composer, then a **Recents** list of the 8 most recent conversations with a "See all" toggle that regroups them into time buckets (Today / Last 2 weeks / April / …). Empty state: a history icon and *"Your past chats will show up here."*

This is the ChatGPT/Claude landing pattern, and it works for a reason: it tells a first-time user *this is a place you can ask things*, and it tells a returning user *here is what you were doing*. Humla's chat empty state is one line of muted text inside a note tab.

(Note for Humla's own design direction: Steno uses a serif — Ovo — for exactly this heading. Michael's stated preference is one sans, no serif headlines, because serif+grotesk reads as "AI fonts". Take the *layout* pattern, not the typeface.)

### 3.3 Preset "skills" — the single best idea to steal

> **Correction (added after review).** Steno has **two** preset sets, which this section originally conflated. `SUGGESTION_CHIPS` in the AskBar are **note-scoped** ("Summarize key decisions", "Action items", "Main topics"); `PRESETS` in `/chat` are **library-scoped** (the four below). The split matters for us: whichever surface ships first needs the matching set, not the other one. Reflected in the action list and in issues #80 / #82.

Steno's library-scoped presets are surfaced three ways from one array (`lib/chatPresets.tsx`, 4 entries):

1. **A `/` popover in both composers.** Type `/` into an empty input → a popover headed **"Skills"** listing each preset with a coloured `/` glyph, a label, and a one-line description underneath. Arrow-key navigable, Enter to pick. Guarded to `input === ''` so a mid-sentence slash doesn't hijack.
2. **A typewriter placeholder** cycling preset labels with a blinking block cursor — and `prefers-reduced-motion` short-circuits it to static text.
3. **Tab-to-accept** — Tab on an empty input fills the currently-shown suggestion. Shell/Copilot ghost-text muscle memory.

The presets themselves are the tell:

- *List recent todos* — "Pulls outstanding to-dos from recent meeting notes"
- *Coach me* — "Coach me on my recent meetings — patterns, blind spots, things to work on"
- *Write weekly recap* — "Summary of the week across every meeting"
- *Blind spots* — "What blind spots have come up across my recent meetings?"

These are not "summarize this meeting". They are **cross-note, multi-meeting, reflective** questions — the ones that only make sense if you have a library. They teach the user what library-wide chat is *for*. Humla's chat, which actually has the retrieval engine to answer these well, ships zero prompts and a blank box.

Calling them **"Skills"** rather than "Presets" or "Prompts" is also better naming, and it's free.

### 3.4 The composer footer: scope + model + honest limitation

Both Steno composers put a control row under the input:

```
[📁 All notes ▾]  gemma4:e2b  · may omit older notes            [↑]
```

- `FolderScopePicker` — all notes / a folder / org-shared
- `formatActiveModel(provider.data)` — which model is about to answer
- **`· may omit older notes`** — shown only for local/remote engines

That third item is the one to notice. It is a voluntary admission that the corpus is truncated, placed exactly where the user is about to type. Their code comment even flags it as approximate and TODOs tying it to the real cap. It buys enormous trust for one line of muted 12 px text.

Humla has the scope picker (`BreadthPicker` — This note / Folder: X / All notes, persisted per conversation, which is *better* than Steno's ephemeral one) and it has a truncation warning — but the warning fires only *after* a turn, and there is no model indicator at all. Users can't see which model is answering without going to Settings.

### 3.5 Streaming, session and interaction details

Worth copying:

- **Stop button.** Send morphs into a `Square` while streaming; Enter also stops. **Humla has no cancel at all** — a long local-model turn cannot be interrupted. On a 12B model that is a 30-second hostage situation.
- **Optimistic navigation with stream handoff.** The entry page starts the stream, records `{sessionId, streamId, folderId}` in a module-level Map, navigates to `/chat/<id>`, and the conversation page claims the in-flight stream on mount. Zero tokens lost across the route change. Keyed by session id so a double-submit can't clobber an earlier handoff.
- **Virtualized message list.** `@tanstack/react-virtual` with `measureElement`, and the streaming bubble is a *synthetic last item* derived at render rather than cloned into the array — their comment notes that cloning per token was O(messages) per delta and defeated the virtualization. Humla renders every message every delta.
- **Asymmetric bubbles.** User turns right-aligned with a tail notch (`rounded-[18px_18px_4px_18px]`) and rendered as plain text so literal backticks survive. Assistant turns get markdown. In the AskBar the assistant has **no bubble at all** — bare markdown at 90 % width. Humla already does exactly this (user bubble, assistant as plain full-width block, Claude-Desktop style), so this one is a tie.
- **Session naming and grouping.** Auto-named from the first 40 chars of the opening question, renameable, deletable, grouped into time buckets. Humla has `relativeTime` and `conversationTitle` with a 40-char `TITLE_MAX_CHARS` — the same primitives — but no bucket grouping and the history lives in a popover on the note header rather than a browsable list.
- **Visible-but-inert while recording.** The AskBar stays on screen during a recording with the placeholder *"Chat available after recording"* instead of vanishing. Explains itself rather than disappearing.
- **Error handling with rollback.** A failed first send deletes the empty session so it never appears in Recents, and restores the user's typed text — but only if nothing was persisted, so the message can't duplicate. Humla's error path is solid too; this is roughly a tie.

### 3.6 Where Humla's chat is already ahead

Do not lose these while copying the chrome:

- **Citations.** Clickable chips under each answer that navigate to the source note. Steno has nothing — you get an answer with no provenance. For a tool whose pitch is trust, this is a significant Humla advantage that is currently under-marketed.
- **Tool-use transparency.** Live "Searching your notes…" while it runs, then a persistent past-tense receipt ("Searched your notes · Read 2 notes"). Steno shows "Thinking" and dots.
- **Real retrieval.** Hybrid FTS5 + semantic, agentic loop, honest empty results (`"No notes matched X. Do not guess"`).
- **Accessibility.** `role="log"`, `aria-live="polite"`, `aria-busy` during bulk loads, `sr-only` author labels so authorship doesn't depend on alignment alone, `motion-reduce` on every animation. Steno respects `prefers-reduced-motion` on the typewriter and has aria-labels on buttons, but Humla's message log is materially more accessible.
- **Per-conversation persisted breadth.** Steno's scope resets on mount; Humla's is stored on the conversation row.

**The summary judgement: Steno built a better front door onto a worse engine. Humla built a better engine behind a worse front door.** Front doors are cheaper to build than engines.

---

## 4. Positioning implications

### First, a correction

`docs/seo-growth-strategy.md` **does not exist in this repo.** I searched the full tree. Whatever named Meetily as the main OSS competitor lives somewhere else (a session transcript, or an un-committed draft). Worth writing down properly, because the mental model does need updating:

> **Meetily is not the main OSS competitor. Steno is.** Steno is closer on features, closer on values, closer on copy, better funded, and shipping faster.

### Does this change the strategy?

**No reposition. One sharpening.** The category framing — open source, local, no bot, own your data, own your keys — is correct and Humla should keep it. Steno validates the market rather than closing it. But the *shared* half of that message is now contested by a funded competitor with 8× the stars and a Mintlify docs site, so Humla's differentiation has to sit in the half Steno cannot copy.

**Steno's buyer is not Humla's buyer, and this is the strategic gift.** Their entire surface points at compliance-driven enterprise procurement: gov-tech, defense-tech, legal-tech, healthcare-ai, sovereign-ai topics; HIPAA/GDPR "by design"; industry landing pages for government / defense / legal / healthcare / finance / executive; a "Book a demo" CTA; an unpriced managed Enterprise tier; trust-strip logos (AWS, HashiCorp, Tesco, Deliveroo, Rutgers, European Union). That is a top-down, sales-led, OCV-shaped motion.

Humla is bottom-up and self-serve: free local app, $5/seat/mo cloud sync you can buy with a card, "a third of Granola's or Otter's per-seat price". **Do not chase them into gov/defense.** With one developer and no compliance certifications that is a fight Humla loses, and the copy would be dishonest.

### The three differentiators to lean on

1. **In-person meetings actually work.** Steno labels every human in the room `[You]`. Humla gives them distinct, renameable, colour-coded speakers. This is a *demonstrable* difference — a screenshot of a four-person in-person transcript side by side would sell it. It is also the hardest for Steno to close.
2. **Ask your notes and see where the answer came from.** Not "we have chat" — everyone has chat. *Citations plus real search.* Humla's landing already says "Don't search your notes. **Ask** them." Extend it: the answer links back to the meeting. Steno cannot make that claim.
3. **Nordic / non-English first-class.** Per-language provider routing, NB-Whisper for Norwegian. Steno picks one engine globally. For a Norwegian-language buyer this is the whole decision, and it is a market Steno is not even looking at.

### Tactical SEO reads from their playbook

They are doing textbook programmatic SEO and Humla has none of it:

- **`docs/compare/`** — 8 pages: granola, otter, fireflies, fathom, tldv, macwhisper, zoom-ai-companion, plus local-vs-cloud. Each has an at-a-glance table, a "Where {competitor} is stronger" section (which reads as credible rather than defensive), "when to choose" for both sides, and FAQ accordions. **Note there is no `steno-vs-meetily` and no `steno-vs-humla`.** Humla is not on their radar. That is an opportunity in both directions.
- **`docs/guides/`** — 6 intent-matched pages: record-zoom-on-mac, record-teams-on-mac, record-google-meet-on-mac, record-system-audio-on-mac, transcribe-audio-locally-on-mac, in-person-meeting-notes. These are exactly the queries a person types when they have the problem. `in-person-meeting-notes` is especially cheeky given their diarization can't do it.
- **`docs/llms.txt`** — a structured, factual summary for AI-citation SEO, with an honest key-facts block (including that analytics are on by default). When someone asks Claude or ChatGPT "what's a local open-source meeting notetaker for Mac", this is the file that gets them cited. Humla has nothing equivalent.
- **18 GitHub topics.** Humla has **zero**. This is free, indexed discovery on the single highest-authority page Humla owns.

---

## 5. Action list

> **Update 2026-07-25 — grilled and filed.** The chat cluster below (items 2, 3, 4, 5, 9, 11) went through a grilling session and is now filed as five issues across two repos. The decisions resolved there supersede parts of this list; each issue carries its own resolved-decisions section.
>
> | Issue | Repo | Blocked by |
> |---|---|---|
> | [#80](https://github.com/michaelwilhelmsen/humla/issues/80) Chat composer — cancel, `/` Prompts, model indicator | humla | — |
> | [#81](https://github.com/michaelwilhelmsen/humla/issues/81) Chat retrieval depth | humla | #66 |
> | [humla-cloud#26](https://github.com/michaelwilhelmsen/humla-cloud/issues/26) Note-less (global) chat scope | humla-cloud | — |
> | [humla-cloud#27](https://github.com/michaelwilhelmsen/humla-cloud/issues/27) Retrieval depth mirror | humla-cloud | cloud#20 |
> | [#82](https://github.com/michaelwilhelmsen/humla/issues/82) Global chat surface — `/chat` route | humla | #80, #81, cloud#26, cloud#27 |
> | [#83](https://github.com/michaelwilhelmsen/humla/issues/83) Auto-detect (unspecced placeholder) | humla | needs its own grilling |
>
> **Key decisions that changed this list:**
> - **Server-first, full parity.** `/chat` will not ship Personal-only — hence the two humla-cloud issues blocking #82.
> - **No thin route first.** Items 2 and 9 are the same surface at two fidelities; built once, in #82.
> - **The retrieval upgrade was split out** (#81) rather than folded in, because it improves today's note-scoped chat on its own merits and shouldn't hold `/chat` hostage.
> - **Item 11 (virtualization) dropped to conditional** — don't pre-build.
> - **Revised sizing: 14–16 slots, not the 8–10 implied below.** Cancel needs real infrastructure (`run_chat` has no cancel token at all — 2 slots, not 1), `CHAT_SCOPE_GLOBAL` means threading `scope` through ~35 call sites plus IPC, and the cloud mirror doubles the retrieval work. Worth knowing, because per §4 this is deliberate **parity spend**, not differentiation.
> - **Auto-detect (item 10) moves up**, queued after #80 and before #82, on the reasoning in §6.

Sized for 1–2 hour evening slots. Ordered by (value ÷ effort).

### This week

**1. GitHub topics + repo description — 15 min.** Humla has zero topics; Steno has 18. Add: `ai`, `meeting-notes`, `meeting-minutes`, `privacy`, `local-llm`, `localllm`, `apple-silicon`, `tauri`, `rust`, `whisper`, `speaker-diarization`, `transcription`, `macos`, `open-source`, `on-device`, `norwegian`. Also fix the description — it still says **"OpenAI / Speechmatics / on-device Whisper"** and Speechmatics is not in the stack (it's OpenAI / Deepgram / Groq / local). Set the `homepage` field to `https://humla.team` — it is currently null. Highest value-per-minute item on this list.

**2. `/chat` route — 1–2 slots.** The structural fix. Add `<Route path="/chat" element={<Chat />} />` and a sidebar entry. v1 can be deliberately thin: mount the existing `ChatPanel` with breadth defaulted to `all`, against a synthetic or most-recent note anchor. `ChatPanel` already takes `noteId` and already supports `all` breadth, so this is mostly plumbing an anchor and a shell. Don't build the greeting/Recents page yet — just give chat a home and a URL.

**3. Stop button — 1 slot.** Humla cannot cancel a streaming turn. On a local 12B model that is unacceptable. Morph the send button to a square while `sending`, wire it to an abort on the Rust side, make Enter stop too. Small, and removes a genuine daily irritation.

### This month

**4. Preset "Skills" via `/` — 1–2 slots.** Copy the pattern wholesale: a `PRESETS` array, a `/`-triggered popover on empty input, label + one-line description per entry, arrow-key nav. Write Humla's own set aimed at what Humla's retrieval can actually do that Steno's cannot — cross-note questions with citations. Start with four: recent action items, weekly recap, "what did we decide about X", "what's outstanding from {folder}". This is the highest-leverage *presentation* item, because it teaches users the feature exists.

**5. Model indicator + honest scope hint in the composer — 1 slot.** Add `formatActiveModel`-equivalent next to `BreadthPicker`. If the grounding budget is likely to truncate for the active model, say so *before* the turn rather than only after. Humla already computes `truncated`; surface the risk pre-emptively. Cheap trust.

**6. `docs/compare/` + guides — 2–4 slots, one page per slot.** Start with the three that match real search intent and where Humla wins on facts:
   - `humla-vs-granola` — the volume query. Local + open + $5/seat vs $14/$35.
   - `humla-vs-steno` — write it honestly, including where Steno is stronger (Windows, auto-detect, docs). Credible comparison pages convert; defensive ones don't. And Humla wins the two facts that matter most to an in-person-meeting buyer.
   - `in-person-meeting-notes-mac` — Humla's strongest genuine advantage against the whole category, and the query is high-intent.
   Then `record-zoom-on-mac`, `record-teams-on-mac`, `transcribe-norwegian-audio-locally`.

**7. `llms.txt` — 1 slot.** A structured factual summary at `humla.team/llms.txt`: what it is, platform, cost, models, providers, diarization, data locations, per-language routing. This is how Humla gets cited when someone asks an assistant for a local meeting notetaker. Keep it honest — Steno's version discloses their default-on analytics, and that honesty is part of why it reads as credible.

**8. Market the citations — 1 slot.** Purely a copy change on the landing page. "Ask your notes" is table stakes now; "and see which meeting the answer came from" is not. One screenshot of an answer with citation chips does the work.

### Later / conditional

**9. Chat entry page (greeting + Recents + buckets) — 2–3 slots.** The full `Chat.tsx` treatment. Only worth it after #2 exists and #4 has given users a reason to visit it. Skip the serif heading and the typewriter; use one sans per the design direction, and a static rotating hint if anything.

**10. Auto start/stop meeting detection — several slots.** The most valuable feature Steno has that Humla lacks, and the most expensive on this list (calendar access, meeting heuristics, a prompt-to-record flow, permissions). Worth scoping as a PRD rather than an evening slot. Not urgent — but it is the feature most likely to make a side-by-side trial go Steno's way.

**11. Message virtualization — 1–2 slots.** Only if long conversations actually feel laggy. Premature otherwise; Humla's conversations are per-note and short today. Note the trick when the time comes: derive the streaming bubble as a synthetic last item, don't clone the array per token.

### Explicitly ignore

- **Gov / defense / sovereign-AI positioning.** Their moat, their funding, their sales motion. Humla cannot credibly claim it and shouldn't try.
- **Windows.** The Swift sidecars make this a rewrite, not a port. macOS-only is a legitimate position.
- **Report templates as separate artifacts.** Humla's presets + custom prompts already cover ~90 % of this. Low marginal value.
- **Matching their star count.** Vanity. 156 stars in 3 months at a similar rate-per-month is fine; a Show HN or an r/opensource post will move it more than any feature will.

---

## 6. One honest closing note

The uncomfortable finding is not any single feature — it's that **Humla and Steno have converged on nearly the same product, the same values, and the same words.** Steno's design system is *paper + ink, no chromatic accent, the accent IS the ink, borders rare, whitespace preferred* — which is almost exactly the direction Humla's own redesign brief is heading. Both are MIT, macOS, local-first, bot-free, BYOK, Markdown-owning, notepad-shaped.

So aesthetic differentiation is thinner than it feels from the inside, and copy differentiation is close to nil. What's left is real and defensible, but it's narrower than "we're the open local one" — because Steno is also the open local one, with money.

The three things that are actually Humla's: **in-person meetings that work, answers that cite their sources, and languages that aren't English.** Lead with those.
