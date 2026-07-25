# Steno AI — competitor analysis

Date: 2026-07-25 · Subject: [stenoai.co](https://stenoai.co/) / [ruzin/stenoai](https://github.com/ruzin/stenoai)
Compared against Humla at `v0.35.0`.

Sources: Steno's `README.md`, `DESIGN.md`, and renderer source read directly from
`main`. The marketing site 403s to automated fetches, so every claim below is
sourced from the repository rather than the landing page.

---

## 1. What Steno is

> "Steno is the privacy-first AI notepad for all your confidential conversations.
> On Windows & MacOS. Perfect for government, healthcare, defence, legal and CXOs."

Same product category as Humla, same origin story (open-source Granola
alternative, local-first, BYO model). Where it differs is in framing: Steno
positions on **regulated-industry confidentiality** and names logos (AWS,
Deliveroo, Tesco, Hashicorp, Rutgers, the EU). Humla positions on
*personal ownership* — "Your audio. Your keys. Your data."

| | Humla | Steno |
|---|---|---|
| Stack | Tauri 2 + Rust + React 19 | Electron + Python 3.9 backend |
| Platforms | macOS 13+ | macOS 14.4+, Windows x64 (alpha, unsigned) |
| Live STT | none (post-chunk) | Parakeet TDT v3 (MLX), 25 languages, live on screen |
| Post STT | Local Whisper, OpenAI, Deepgram, Groq | Whisper Large V3 Turbo, Parakeet |
| Diarization | FluidAudio — Community-1 + Sortformer, two engines | speaker labels via Parakeet, `[You]`/`[Others]` |
| Summaries | OpenAI, Ollama (native `/api/chat`), any OpenAI-compat | Ollama (Gemma 4 / Qwen 3.5), OpenAI, Anthropic, Bedrock |
| Chat | agentic tool loop, citations, note/folder/all scope | streaming RAG over summary + topics + transcript |
| Sync/teams | Humla Cloud (paid) or self-hosted sync server | "Organisation AI" adapter for managed deployments |
| Licence | MIT | MIT (+ CLA) |
| Traction | 80 commits | 535 commits, 1.2k stars, 163 forks, Discord, OCV-sponsored |

## 2. Where Humla is genuinely ahead

These are real, defensible, and worth not throwing away while chasing parity.

- **Two-stream capture, kept separate end to end.** Mic and system audio get
  independent chunk WAVs, independent Whisper invocations, and independent
  `prior_context` trails. Steno captures system audio and attributes
  `[You]` vs `[Others]`, but there's no equivalent of Humla's per-source
  transcript trails — which is what stops a bilingual call drifting across
  streams.
- **Per-language provider routing.** `TranscribeConfig { default, per_language }`
  means Norwegian → NB Whisper, English → Deepgram Nova-3, everything else →
  OpenAI. Steno picks one engine globally. For a Norwegian user this is not a
  nicety, it's the product.
- **Two diarization engines, user-selectable, re-runnable.** Community-1 vs
  Sortformer with tunable thresholds, and `keep_audio` lets a note be
  re-diarized later at different settings. Steno has one path.
- **A much stronger chat *engine*.** See §4 — this is the important nuance.
- **Rust/Tauri over Electron+Python.** Smaller bundle, no bundled interpreter,
  no `requirements.txt` at runtime.

## 3. Where Steno is ahead (beyond chat)

- **Live transcription on screen while recording.** Parakeet TDT v3 via MLX
  renders text as you speak, in a Granola-style bubble view. Humla shows the
  transcript only as chunks land, unlabelled, in arrival order. This is the
  single most *visible* gap to a prospect doing a side-by-side.
- **Auto start/stop meeting detection** (`app/meeting-detect.js`) — Steno
  notices a meeting began and offers to take notes, then offers to summarise
  when it ends. Humla has no calendar integration and no auto-detect; every
  recording is manual. Grep confirms zero calendar code in `src/` or
  `src-tauri/src/`.
- **Windows.** Alpha and unsigned, but it exists. Humla is macOS-only by
  architecture (ScreenCaptureKit, CoreML, Metal).
- **Report templates** — a note can hold multiple generated reports, switchable
  in the detail view. Humla has presets, but one summary per note.
- **Distribution.** 1.2k stars, an active Discord, a CLA, a `granola-to-steno`
  migration skill, and Open Core Ventures backing. Humla is at 80 commits with
  no community surface.

## 4. The chat gap — engine vs. surface

The honest summary: **Humla has the better chat engine and the worse chat
surface.** Steno's advantage is entirely in placement and invitation, not in
retrieval quality.

### Humla's engine is ahead

`src/components/ChatPanel.tsx` runs a genuine agentic retrieval loop —
`search_notes` / `get_note` / `list_notes` — streams the answer, and shows:

- **De-duplicated citation chips** per assistant turn (`messageCitations`,
  ChatPanel.tsx:57).
- **Live tool activity** ("Searching your notes…", ChatPanel.tsx:73) and a
  persistent past-tense receipt ("Searched your notes · Read 2 notes",
  `summarizeToolUse`, ChatPanel.tsx:91).
- **Scope as a live filter** — note / folder / all — persisted on the backend
  conversation row, not just in component state.
- Multi-conversation history per note, gen-guarded against stale writes on
  switch, plus workspace BYOK and a turn-allowance meter.

Steno's `AskBar` sends the question against a prebuilt bundle (summary + key
topics + transcript) and streams markdown back. No tool calls, no citations, no
"here's what I looked at". Its `Chat.tsx` route does the same across the library
with a folder filter.

### Steno's surface is ahead — four specific reasons

**1. Chat is a floating dock, not a tab that evicts the note.**

Humla renders chat as the third tab of the right-hand panel
(`src/pages/Note.tsx:829-932`). Choosing Chat *replaces* Summary and Transcript
— the two things you most want in front of you while asking a question about
them.

Steno's `AskBar` is a translucent bar fixed to the bottom of the window; the
answer panel expands *upward over* the note (`AskBar.tsx:401-431`,
`max-height: 360`). The note never leaves the screen. Click-outside collapses
it back to a one-line bar. Chat is an overlay, not a destination.

**2. One canonical bottom slot, shared by every "what's happening now" state.**

This is the deepest idea in their design, and it's worth quoting their own
comment (`BottomDockSlot.tsx`):

> Canonical fixed-bottom anchor used by AskBar, LiveDock, and ProcessingDock.
> Ensures all three states sit in the exact same screen slot so transitions
> between recording → processing → meeting feel like a content swap, not a
> layout reshuffle.

`PrimaryDock.tsx` then decides what occupies the slot from *recording status,
not route*: recording pill, expanded live transcript, or the Ask bar. While
recording, the Ask bar stays rendered but `disabled` with the placeholder
"Chat available after recording" — deliberately kept in the tree so a React
remount can't drop a typed draft or an in-flight stream.

**Humla already has this anchor and hasn't generalized it.** `RecordingBar.tsx:122`
is `absolute bottom-6 left-1/2 -translate-x-1/2` — the same slot, same floating
pill treatment. It's just (a) only ever the recording state and (b) mounted
inside `Note.tsx` rather than at app level.

**3. Chat is unreachable outside a note.**

Humla's only chat entry point is `Note.tsx`. Home, All Notes, Folder, and Trash
have none — verified by grep, `ChatPanel` is imported in exactly one file. The
backend supports `all` scope, so "what did I commit to this week?" is
answerable — but only after opening an arbitrary note first, which then becomes
a meaningless anchor for the conversation.

Steno splits this cleanly in two:
- `AskBar` — ephemeral, per-meeting, "ask about *this*".
- `/chat` route (`routes/Chat.tsx` + `routes/ChatConversation.tsx`) — a
  first-class library-wide surface with its own history list and a
  `FolderScopePicker` chip (All notes / a folder / Shared notes).

**4. The empty state invites; ours doesn't.**

Steno's `AskBar` shows three suggestion chips the moment you focus an empty
conversation (`AskBar.tsx:434`): *Summarize key decisions* · *Action items* ·
*Main topics*.

The `/chat` route goes further — a typewriter placeholder that cycles four
library-scale presets (`lib/chatPresets.tsx`), each with a coloured glyph
reinforcing the `/` shortcut, and each honouring `prefers-reduced-motion`:

- **List recent todos** — "List my action items from the last week."
- **Coach me** — "Coach me on my recent meetings — patterns, blind spots, things to work on."
- **Write weekly recap** — "Write a recap of this week based on my notes."
- **Blind spots** — "What blind spots have come up across my recent meetings?"

Those four prompts *are* the pitch for cross-note chat. They teach the user that
the corpus is queryable at all. Humla's chat opens as an empty text field —
which asks the user to invent the product's value proposition themselves.

## 5. What to take, in priority order

1. **Promote the bottom pill to an app-level dock slot.** Lift `RecordingBar`'s
   anchor out of `Note.tsx` into `Layout.tsx` as a slot that holds recording /
   summarizing / ask, one at a time. Keep the ask bar mounted-but-disabled
   during recording rather than unmounting it.
2. **Turn chat into an overlay on the note, not a tab that replaces it.** Keep
   the existing `ChatPanel` engine wholesale — citations, tool receipts, scope
   chip, history — and change only where it renders: expanding upward from the
   dock over the note body, collapsing on click-outside. The Chat tab can go.
3. **Add a library-level chat route** reachable from the sidebar, defaulting to
   `all` scope, with its own conversation history. The backend already supports
   it; this is a routing and shell change, not a retrieval change.
4. **Ship preset prompts in both empty states.** Note-scoped chips for the
   overlay, library-scale presets for the route. Cheapest change on this list
   and probably the highest leverage per line.
5. **Then reconsider live transcription and meeting auto-detect** — bigger
   builds, and the ones that close the remaining demo-visible gaps.

Items 1–4 are surface changes over an engine that is already ahead of theirs.

## 6. Strategic read

Steno is not beating Humla on capability; it's beating it on *reachability*.
Their chat answers are thinner — no tool loop, no citations, no receipts — but
you can ask a question from anywhere in one click and you're told what to ask.
Humla's answers are better and almost nobody will discover them, because the
feature is three clicks deep behind a tab that hides the thing you're asking
about.

Two things are genuinely defensible against them and should stay in the
positioning: **per-language transcription routing** (nobody else is shipping a
Norwegian-tuned path) and **two-stream separation**. The chat gap is a week or
two of shell work, not a rewrite.
