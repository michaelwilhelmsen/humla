# Humla — project notes

## What this app is

**Humla** is a personal macOS meeting-notes app inspired by Granola. You take freeform notes during a meeting; in parallel the app records mic + system audio, transcribes it, and produces an AI summary that fuses your notes with the transcript. Personal/small-team, not SaaS — your data, your API keys, local SQLite, no backend.

The name is Norwegian for "bumblebee".

## Core capabilities

- **Hybrid capture (parallel streams)** — mic + macOS system audio recorded simultaneously via a Swift sidecar, kept as **two separate streams end-to-end** (no mixdown). Each gets its own VAD-bounded chunk WAVs, its own full.wav, its own Whisper invocations with its own `prior_context` trail. In-person meetings produce only mic chunks (system stays silent → no chunks emitted) and the diarizer runs on the mic stream so multiple humans in the same room get distinct labels. Remote calls produce both, and **both get diarized** — the mic is never assumed to hold a single person just because the system stream has content. `You` is earned, not assigned: it survives only when the mic diarize resolves to exactly one voice.
- **Four STT providers** — OpenAI (whisper-1 / gpt-4o-transcribe / mini / diarize), on-device Whisper via Metal, Deepgram (nova-3, nova-2, base), and Groq (whisper-large-v3-turbo). All slot into the `stt::BatchSttAdapter` trait so the dispatch path is provider-agnostic.
- **Per-language routing** — `transcribe_config` (typed JSON, single source of truth) is `{ default: ProviderConfig, per_language: BTreeMap<String, ProviderConfig> }`. Resolution at chunk time: per-note language → per-language override → default. E.g. Norwegian → local NB Whisper, English → Deepgram Nova-3, default → OpenAI whisper-1.
- **Whisper quality preset** — Fast (greedy) / Balanced (beam=3) / Quality (beam=5, low no_speech threshold) for the local provider; bundles sampling strategy + confidence thresholds together so the user picks one knob.
- **Per-note transcription language** — global Settings → Language is the default; each note has its own language chip that overrides for that note.
- **Offline diarization on stop** — `speaker-diarize` Swift sidecar runs after `recording_stop`. Two engines selectable via the `diarize_model` setting: **Community-1** (FluidAudio's `OfflineDiarizerManager` — community-1 segmentation + VBx clustering with PLDA) and **Sortformer** (NVIDIA end-to-end, 4-speaker cap). Branches on which streams produced content: mic-only diarizes `mic_full.wav` and emits `Speaker 1:` / `Speaker 2:`; when both streams have content, **both are diarized** and numbered off one shared counter so a mic speaker and a system speaker can never collide on a number (`build_hybrid_labels`). The old shortcut — hard-label every mic chunk `You:` and diarize only `sys_full.wav` — meant a single stray system chunk (a notification chime, a few seconds of video) collapsed an entire in-person meeting onto one `You:` line, since presence of *any* sys chunk routed the recording into remote-call mode. `You:` is now applied only when the mic diarize resolves to exactly one voice.
- **Speaker rename + colour-coded pills** — each unique speaker gets one of four design-token colours (interactive blue, success green, warning gold, accent red, cycling for 5+). A chip strip above the transcript lets the user click any speaker to rename inline; rename is a regex line-anchored rewrite of the transcript text — no separate metadata table.
- **Two-source summaries** — model gets `[Notater]` (typed notes) and `[Transkripsjon]` (transcript) as two labelled blocks in one user message. Preset prompts are deliberately minimal (kind of summary + language only); the only source-trust signal is the parenthetical tags `[Notater] (user-written)` / `[Transkripsjon] (auto)` — the old explicit "favour notes for intent, transcript for facts" rule was dropped. An empty side is sent as `(ingen)` rather than omitted, so thinking models don't second-guess a missing block.
- **Per-note presets** — Meeting / 1:1 / Lecture / Interview / Brainstorm / Voice memo, each with its own summary prompt. Custom prompts also supported (rows in `summary_prompts` table, referenced as `custom:<id>`).
- **Custom vocabulary** — per-user list of names and tech terms biasing decoding. Threaded through Whisper-shaped providers as `initial_prompt`, Deepgram as `keyterm` (Nova-3) or `keywords` (other models) query params.
- **Trailing transcript context** — every chunk's transcription receives the last ~150 committed words as `prior_context` (Whisper's `initial_prompt` slot for OpenAI/Local/Groq; Deepgram ignores it because its `keywords` is a per-token boost, not a continuation primer). Single biggest mitigation against silence-driven hallucinations and proper-noun drift.
- **VAD-bounded chunks** — sidecar rotates each chunk at natural speech pauses (min 1.0 s / max 15 s / 500 ms silence trigger) instead of a fixed timer.
- **Reasoning-model temperature handling** — gpt-5.x / o-series reject `temperature`; `openai::summarize` detects via `is_reasoning_model()` and omits.
- **Local-LLM summary tuning** — Ollama path goes through the native `/api/chat` endpoint (not the OpenAI-compat shim at `/v1`) so `think` is a real toggle instead of being template-stripped. `num_ctx` is sized adaptively from the actual prompt length + `num_predict` budget, rounded up to powers of two, bounded `[8192, 65536]` — a flat 65K KV cache was OOMing the model runner on tighter Macs (the cache is multi-GB on top of model weights). `keep_alive: 0` releases the model immediately after each summary so RAM frees up. Sampling is **model-family-aware** (`sampling_profile(model, think)`): Qwen models get the loop-breaker profile (`presence_penalty=1.5`, `frequency_penalty=0.5`, `repeat_penalty=1.0`) that breaks Qwen 3.5's thinking-phase loop class; every other family (e.g. the recommended `gemma4:12b-mlx`) gets neutral defaults (`presence/frequency=0`, `repeat_penalty=1.1`) rather than penalties tuned for Qwen. Only `ollama_chat_stream` (the summary path) carries this; the agentic tool-calling step (`ollama_chat_step`) uses neutral `temperature=0`. Cloud OpenAI summary **streams** and shares `post_chat` with the chat path, which retries transient `Kind::Request` errors twice (500 ms / 1.5 s backoff) — covering the case where reqwest tries to reuse a pooled HTTP/2 connection that OpenAI's edge has half-closed. Retries can't rescue a connection reaped *while the model reasons*, which is why the path streams at all; see the `openai.rs` row.
- **Your notes over MCP** (#172) — Humla ships its own **Model Context Protocol server**, so Claude Code, Codex or any other MCP client can search and read the library from wherever the user works. Six read-only tools (`search_notes`, `get_note`, `get_transcript`, `list_notes`, `list_folders`, `list_clients`), **off until explicitly enabled** in Settings → General → Integrations, which also offers a ready-to-paste config snippet per client. Read-only in this version, and **no tool returns or references an audio file** — `keep_audio` stays the single absolute gate on audio (#24) and this is not an exception above it. The workspace is **resolved, never accepted as an argument**, so Personal and workspace notes can't reach each other however a client asks. Search is keyword-only on purpose: an agentic client substitutes repeated queries for vector recall, which buys no API key, no per-query cost, no Keychain prompt and no network. The gap that does bite is cross-language — the index is lexical, so a query in the wrong language returns *nothing*, and the fix chosen is to carry each note's language as data (`lang:` on every hit and row, `language` as a filter) rather than to name any language in a tool description. Shipping it first-party is a maintenance decision as much as a feature: Granola never did, so its users wrote nine servers against its local store, and the schema became a public API maintained by strangers.
- **Folders** — flat folder list, per-note assignment; sidebar search matches note titles/bodies/transcripts and renders a flat result list annotated with each note's folder name.
- **Chat over your notes** — an agentic retrieval loop (`chat/`), surfaced as the **Chat tab of the Note right panel**. The assistant searches and reads notes with three tools (`search_notes` over FTS5 keyword + semantic embeddings, `get_note`, `list_notes`), streams its answer, and **cites the notes it drew from** as chips that navigate to the source. Two per-conversation retrieval filters live on the conversation row and bind every turn regardless of what the model asks for: **breadth** (`note` | `folder` | `all` — what is in reach) and the **authorship pin** (`owner_filter`, a user id — whose notes are in reach, workspace-only, surfaced as the "Created by me" toggle beside the breadth picker). Multiple conversations per note, with history; a conversation can be **renamed or deleted** (hard, with a confirm — there is no Trash for chat) from the sidebar row's right-click menu, the `/chat` app bar's ⋯, or the note pane's history popover. Runs on a chat provider configured **separately** from transcription and summary (Settings → Chat; OpenAI or Ollama). In a workspace, turns route to humla-cloud and retrieval happens server-side. Reachable inside a note, at its own library-wide **`/chat`** route, and — scoped to one folder — at **`/folder/:id/chat`** ("Chat about this folder" on the folder row's context menu, #110).
  - **A conversation's scope is `(tenant, scope, scope_id)`** — `note` (the note id), `folder` (the folder id, #110) or `global` (a fixed sentinel). The three populations never leak into each other, locally or server-side. Breadth is a *live filter within* a thread, not its identity: a Note's pane can widen to its folder or the library, but a note-less target is **pinned** to its own reach (`all` for `/chat`, `folder` for a folder chat) because with no anchor the target's identity *is* its reach — `pinned_breadth` / `targetPinsScope` own that rule on each side. Deleting a folder **hard-deletes its conversations** (they have no Trash and their whole reach was that folder); its notes only reparent, so the Sidebar confirms only when threads would actually be lost. `ChatTarget::from_ids` takes the note and folder ids as **alternatives** — both at once is an error, and an empty id is an error under either.
  - **Opening a pane drafts or resumes, by target** (`resumes_on_open` in Rust, `targetResumesOnOpen` in TS — mirrored, change both). **`/chat` drafts**: it opens on an unsaved conversation with the prompt cards showing, persisting *nothing* until the first turn, so an abandoned thread leaves no row and "+" is a local reset that costs no IPC. **A note's Chat tab resumes** that note's most-recent thread, deliberately — a note is an anchor, so continuing the same line of thinking is the plausible default there. A drafting pane's breadth and authorship pin live in the pane and ride in on `chat_send` (`DraftSettings`), which is what preserves the #61/#103 guarantee without the lazy row that used to be hidden from the list yet still resolved to by the next send. Conversation lists hide zero-message rows for drafting targets only (in a note, an empty thread *is* the draft being resumed) — in SQL for Personal so paging stays honest, and off the server's `message_count` for a workspace, whose messages never land in the local table.
- **Every word of the transcript has a timeline behind it** (#169, [ADR-0004](docs/adr/0004-the-timeline-is-canonical-for-a-notes-content.md)) — the styled reader renders from the merged timelines, so text `note.transcript` carries and no timeline accounts for is *invisible* there, and the first rebuild (`rebuild_note_transcript`, reached by cycling a speaker label, deleting a chunk, re-diarizing or unifying) deletes it outright — out of the summary, chat retrieval and embeddings with it. It arose because `combine_with_snapshot` prepends the prior take's transcript while `serialize_timeline` sees the current session's chunks alone, so any recording that landed text but wrote no session assets left an orphaned prefix. Three defences, in order:
  - **Prevention.** `diarize_and_apply` no longer returns early when the diarize model is missing; it serializes the chunks with an empty label — and the label alone, since word timings are orthogonal to who spoke — so the take still writes a session — which also gives a later re-diarize somewhere to attach to.
  - **Repair on open.** `note_timeline_repair` compares `comparable_words` of the transcript against the timelines' projection; `orphaned_prefix` recovers the leading lines the projection misses and `synthesize_orphan_timeline` writes them as a session at **manifest index 0** (named "Earlier transcript", never "Recording 0" — it was never a take). Idempotent, and it runs *after* the workspace session pull so a still-arriving timeline can't be duplicated. A note with **no** timeline at all is left alone — it renders the textarea and hides nothing, and a synthesized session would take its free-text editing away.
  - **Render-time guard.** When `coversTranscript` comes back false — a timeline present but short because malformed lines were skipped, an asset that never downloaded — the note falls back to `TranscriptView` over the whole string. The comparison is the backend's; **the client never re-derives the grouping rule** (Rust groups on label alone, `groupTimeline` on label *and* session, deliberately), and comparisons normalize word sequences, never line counts. One more mirrored pair to change together: `split_label` (Rust) and `parseTranscriptLines` (`Note.tsx`) are the same "`Label: ` prefix" rule, and the repair splits lines by the first while the reader draws them by the second.
  Separately, `commit_rebuilt_transcript` **refuses to commit an empty projection over a non-empty transcript**, which protects pre-sessions notes that have a transcript and no timeline at all from being blanked by one click.
- **Transcript editing routes through the timeline** (#170) — on a note that has a timeline, the styled reader renders *from* it and `note.transcript` is a projection of it, so editing is **per turn**: a hover pencil opens that turn in place, and committing calls `note_timeline_set_chunk_text` with the whole run of chunk indices the turn spans (`TimelineGroup.indices`), which writes the text into the lowest index, empties the rest, and re-derives the transcript from every session's timeline — the same path `note_timeline_set_chunk_label` and `note_timeline_delete_chunk` already take. Word timings on an edited turn are dropped (they describe words that are gone); its **bounds are kept**, so it still highlights during playback and only per-word karaoke is lost, on that turn. The old panel-wide textarea wrote the *derived* copy and touched no timeline, so the edit was invisible in the reader — permanently — while summary, chat and embeddings read the edited string. **A note with no timeline keeps the whole-transcript textarea** (`TranscriptEditor`): with no timeline there is no second copy to orphan. Both are locked while a recording is in flight.

## Architecture overview

```
┌─────────────────────────────────────────────────────────────┐
│ React + Vite frontend (src/)                                │
│  Tiptap editor · Zustand store · React Router · Tailwind v4 │
└──────────────────────┬──────────────────────────────────────┘
                       │ Tauri IPC (invoke / events)
┌──────────────────────▼──────────────────────────────────────┐
│ Rust backend (src-tauri/src/)                               │
│  commands.rs · db.rs · recording.rs · stt/* · diarize.rs    │
│  local_whisper.rs · openai.rs · presets.rs · wav.rs         │
│                                                             │
│  ┌─────────────┐  ┌─────────────────┐  ┌─────────────────┐  │
│  │SQLite(rusql)│  │ audio-capture   │  │ speaker-diarize │  │
│  │ notes /     │  │ sidecar (Swift) │  │ sidecar (Swift) │  │
│  │ folders /   │  │ AVAudioEngine + │  │ FluidAudio      │  │
│  │ settings    │  │ ScreenCaptureKit│  │ (CoreML / ANE)  │  │
│  └─────────────┘  └─────────────────┘  └─────────────────┘  │
│                                                             │
│  ┌─────────────────────────────────┐  ┌─────────────────┐   │
│  │ HTTPS clients                   │  │ Local Whisper   │   │
│  │ OpenAI · Deepgram · Groq · HF   │  │ whisper-rs 0.16 │   │
│  └─────────────────────────────────┘  └─────────────────┘   │
└─────────────────────────────────────────────────────────────┘
```

### Data flow during a recording

1. **`recording_start`** spawns the `audio-capture` sidecar via `setsid` (sandbox-detached so TCC prompts go to *Humla*, not Terminal). Diarize sidecar is *not* spawned here — it runs once, after stop.
2. **Sidecar capture** — `AVAudioEngine` (mic) + `ScreenCaptureKit` (system) feed two **independent** writer pairs. Each source has its own `ChunkWriter` (VAD-bounded WAV chunks, 1.0–15 s; rotates on 500 ms silence) and `FullRecordingWriter` (full stream → `mic-full.wav` / `sys-full.wav`). Sidecar emits `{event:"chunk", source:"mic"|"sys", path, start_ms}` on stdout, plus per-source `{event:"full_recording", source, path, duration_ms}` on shutdown. No mixer — per-chunk audio is single-source so Whisper sees clean signal regardless of overlap.
3. **Rust reader thread** parses each `chunk` event, appends a `ChunkRecord{source, path, start_ms}` to `RecordingSession.chunk_log`, and spawns `transcribe_chunk(source, …)` on a tokio task tracked in `RecordingSession.inflight`. Concurrent chunks serialise on `transcribe_gate` so each one's `prior_context` sees a fresh trail snapshot.
4. **`transcribe_chunk`**:
   1. Resolve language (`note.language || global`) and provider config (`read_transcribe_config(state).resolve(&language)` — picks per-language override if any, else default).
   2. Skip near-silent chunks via `wav::rms` gate (`silence_rms_threshold`, default 0.005).
   3. Acquire `transcribe_gate`. Build `bias_terms` from custom vocab + `prior_context` from the per-source `TranscriptTrail` snapshot (`mic_trail` for mic chunks, `sys_trail` for sys chunks — separate trails so bilingual calls don't drift across streams).
   4. Call provider through `stt::BatchSttAdapter` (one of OpenAI / Local / Deepgram / Groq).
   5. Run `is_likely_hallucination`, `strip_attribution_tail`, repetition-collapse and cross-chunk loop guards.
   6. `db::append_transcript(text, separator)` with raw text — no speaker label yet. Labels are applied after stop.
   7. Push text into the matching per-source `TranscriptTrail` for the next chunk's prompt context.
   8. Emit `transcript_replaced` with the full new transcript so the UI updates live.
5. **Frontend live update** — `useRecordingStore` listens for `transcript_replaced` and updates the note's transcript in `useNotesStore`. The Note view's transcript card re-derives speaker labels from the text on every render and renders coloured pills inline (only after the post-stop diarize pass adds them; during recording the live transcript is plain text in arrival order).
6. **`recording_stop`** — SIGTERM the audio-capture sidecar → 3 s grace → SIGKILL fallback → drain inflight handles + reader handle.
7. **Offline diarize on stop** — `diarize_and_apply` partitions `chunk_log` by source and branches:
   - **Mic only** (in-person): run the diarize sidecar over `mic-full.wav`. Each chunk gets `Speaker N:` from its segment via `assign_speaker(start_ms, segments)` with closest-edge fallback.
   - **Sys only** (mic silent): same, on the system stream.
   - **Both streams have content** (remote/hybrid): diarize **both** `mic-full.wav` and `sys-full.wav`, then number every voice off one shared counter walked in `(start_ms, source)` order (`build_hybrid_labels` → `finalise_stream_labels`). `You:` replaces `Speaker 1` only when the mic resolved to exactly one voice — the solo-remote-call shape — in which case the system side restarts its numbering at 1 so `You` doesn't consume a number. A stream whose diarize failed falls back to a single label taken from `next_free`, so it can't collide with a real speaker from the stream that succeeded. The mic hint is `None` (the note's `expected_speakers` is a total across both streams and can't be split a priori); the sys hint is that total minus the voices the mic accounted for.
   `build_labelled_transcript` merges all chunks across sources, sorted by `(start_ms, source)`. Resumed recordings prepend the prior transcript snapshot via `combine_with_snapshot` with `Speaker N:` numbers offset past any in the snapshot. Skips silently when the diarize model isn't downloaded.
8. **Crash recovery** — sidecar stdout EOF detection resets the session and emits an error toast. The audio-capture sidecar polls its PPID every 2 s and self-exits if it sees PID 1 (parent died), so dev-reload zombies clean themselves up.
9. **Summary** is fired manually via `summarize_note`. Reads `note.body` (HTML → plain text) + `note.transcript`, resolves the preset's prompt, appends a language directive, and calls the configured summary provider. Reasoning models (gpt-5.x / o-series) get `temperature` omitted automatically.

## Tech stack

### Frontend (`src/`)

- **React 19** + **TypeScript** + **Vite 6** + **Tauri 2** (`@tauri-apps/api` for `invoke` + event listeners).
- **React Router 7** — note routing (`/note/:id`), settings, home.
- **Zustand** — `useNotesStore` (notes/folders) + `useRecordingStore` (status/errors/diagnostics); backend events bound once via `bindBackendListeners`. Listens for `transcript_replaced`, `summary_ready`, `summary_thinking_delta`, `summary_content_delta`, `summary_status`, `recording_status`, `recording_error`, `recording_diagnostic`, `local_whisper_progress`, `local_whisper_download_error`, `diarize_download_progress`.
- **Tiptap v2** — body editor (StarterKit + Placeholder + Suggestion + BubbleMenu).
- **Transcript view** — lives in the right context panel's Transcript tab. Styled-by-default with `white-space: pre-wrap` so its rendered height matches the textarea exactly (no per-line margin → no page-jump on click-to-edit). Speaker labels rendered as inline `nd-speaker-pill` chips; rest of line is plain text.
- **`SpeakerLabels` chip strip** — derives unique speaker labels from the transcript on every render; click to inline-rename. Rename rewrites the transcript via line-anchored regex (`/^Speaker N: /gm` → `/^Michael: /gm`).
- **Auto-update** — Tauri updater polls `latest.json` from GitHub releases on launch.
- **react-markdown** + **remark-gfm** — summary + reasoning-trace rendering.
- **Tailwind v4** — `@tailwindcss/vite` plugin; design tokens in `src/styles/globals.css`. Base resets are wrapped in `@layer base` so utility classes can override them via cascade.
- **lucide-react** — icon set.
- **Radix primitives** (`@radix-ui/react-popover` / `react-dropdown-menu` / `react-select`) — every anchored panel in the app goes through one of three thin wrappers in **`src/components/ui/`** (`Popover`, `Menu`, `Select`), which dress Radix in Humla's tokens and share one floating-surface look via `surface.ts`. Radix owns portalling, collision-aware placement, dismissal, focus return, arrow-key roving and typeahead — **don't hand-roll another popover** (#114 deleted six of them). `Menu` for a list of choices, `Popover` for arbitrary content, `Select` for a value picker, and **`Combobox`** (#116 — a fourth wrapper, built *on* `Popover`) when the user must be able to type a value that isn't in the list. Radix has no combobox: `Select` takes no typed input and `Menu` runs its own typeahead over the content, which would eat the keystrokes. `Combobox` keeps focus in the input and expresses its highlight through `aria-activedescendant`. **Not shadcn**: its CVA/token layer (`--background`, `--primary`, …) is deliberately absent, since it would sit beside Humla's own vocabulary. Menus default to `modal={false}`; note that Radix `Select` *is* modal, so the page behind an open listbox is inert and aria-hidden (tests must press outside rather than click through). jsdom needs the `ResizeObserver` / `scrollIntoView` / pointer-capture shims in `src/test/setup.ts` for any of it to render.
- **Design system** (redesigned in v0.30.0 — the old "Nothing" aesthetic with Space Grotesk + Space Mono is **gone**, don't reintroduce it):
  - **One typeface — Hanken Grotesk** everywhere, via `@fontsource/hanken-grotesk`. Hierarchy from size + weight only. `--font-serif` and `--font-mono` are **aliases onto the Hanken stack**, so any lingering `font-serif` / `font-mono` class is a visual no-op — genuine code blocks use `--font-code`. **No serif headlines, no mono, no uppercase labels** — labels are sentence case.
  - **Accent is Humla gold `--color-accent: #ffdc6c`**, the same value in both themes (no dark inversion). `--color-on-accent` for glyphs on filled gold. Raw gold is too light to read as text, so **text/icon/link uses take `--color-accent-text`** (`#7a5a12` light / `#f3d178` dark) — filled uses take accent + on-accent. Filled gold is reserved for `.nd-btn-primary`; "selected" states use `--color-accent-soft` bg + `--color-accent-text`. Red is **not** the accent — `--color-record` is the recording dot, `--color-danger` is destructive.
  - **Layout** — left nav and right context panel are inset rounded cards on the canvas (~6px gutter, no borders — shadow + bg define them), macOS traffic lights inside the nav card. Dark mode is a three-layer elevation ladder: canvas darkest → cards → pills.
  - **Utilities** (`src/styles/globals.css`): `.nd-btn` / `.nd-btn-primary` / `.nd-btn-icon` / `.nd-btn-icon-sm` (all pill/circle radius), `.nd-chip`, `.nd-meta` (+ `.is-interactive`), `.nd-label`, `.nd-bare`, `.nd-recpill`, `.nd-speaker-pill`, `.nd-speaker-dot`, `.nd-word` / `.nd-word-active`. **`.nd-action`, `.nd-prop-*`, `.nd-select`, `.nd-ctl` and `.rec-bar` were deleted** — don't reference them. **`.nd-meta` is the one picker-trigger chip** — borderless, `+ .is-interactive` for the hover fill, `+ .is-filled` for a resting `surface-2` fill where the chip stands alone on a card (the Summary / Transcript panel pickers) rather than sitting in a line of metadata. `.nd-ctl`'s bordered pill is gone: a picker that opens a menu shouldn't look like a different control depending on which panel it's in.
  - Speaker pill colours: `--color-interactive` / `--color-success` / `--color-warning` / **`--color-speaker-4`** (a dedicated red — *not* `--color-accent`, which is now gold), cycling for 5+ speakers.
  - **`--color-pill` is transparent by design** — use `--color-pill-hover` for surfaces that need a fill (code blocks, hover states). Page bg is `--color-canvas`, not `--color-bg`.
- **Note view structure** — a two-column flex: scrollable body + a toggleable, resizable **right context panel** with tabs **Summary | Transcript | Chat** (width persisted to `localStorage` as `humla.panelWidth`, clamped 320–720). Summary and Transcript are *not* cards stacked below the body. A compact inline **meta bar** under the title carries note identity only (author · workspace · date · folder · sync); generation settings live in the panels (preset + provider in Summary, language + speakers in Transcript).

### Backend (`src-tauri/src/`)

- **Rust 1.85** + **Tauri 2** runtime.
- **rusqlite** (`bundled` feature) — single SQLite DB at `~/Library/Application Support/no.humla.app/notes.sqlite`. WAL mode; idempotent ALTER TABLE migrations; index creation runs *after* migrations.
- **reqwest** with `rustls-tls` + `stream` — all HTTPS (OpenAI, Deepgram, Groq, Hugging Face for model download).
- **tokio** — async runtime. `spawn_blocking` wraps local Whisper inference. **Use `tauri::async_runtime::spawn` (NOT `tokio::spawn`) anywhere that runs from Tauri's `setup` closure** — setup runs on the main thread before tokio's runtime is attached; bare `tokio::spawn` panics with "no current Tokio runtime", propagates through the AppKit FFI as `panic_cannot_unwind`, and aborts the app on launch.
- **whisper-rs 0.16** with `metal` feature — bundles whisper.cpp via cmake, runs `large-v3-turbo-q5_0` (~547 MB) on Apple Silicon GPUs. NB Whisper Large available as a Norwegian-specific model, picked via per-language override.
- **parking_lot** — synchronous mutex for session state. **NEVER hold a `parking_lot` guard across an `.await`** — the future becomes non-Send and Tauri command futures must be Send. Use `tokio::sync::Mutex` for state accessed across await points (e.g. `transcribe_gate`).
- **keyring 3** with `apple-native` backend — per-provider Keychain entries (`openai_api_key`, `deepgram_api_key`, `groq_api_key`). Cached on `AppState.api_key_cache: HashMap<&'static str, Option<String>>` so each provider's first read prompts macOS Keychain once per session.
- **serde** / **serde_json** / **chrono** / **uuid** / **anyhow** / **async-trait**.

### Module map

| File | Responsibility |
|---|---|
| `lib.rs` | `AppState`, command registration, plugin setup, startup migrations |
| `main.rs` | Tauri entry |
| `commands.rs` | Module root for `crate::commands`. Holds the tightly-coupled core: recording lifecycle (`recording_start`/`stop`/`pause`/`resume`/`state`), `transcribe_chunk` fan-out via `stt::BatchSttAdapter` + hallucination/repetition/attribution guards, offline diarize on stop (`diarize_and_apply`, `rediarize_note`), transcript labelling + post-processing (`build_labelled_transcript`, bridge/merge passes), `serialize_timeline` + `note_timeline*`, playback/diagnostics writers, plus shared helpers (`err`, `emit_*`, `sidecar_path`, `DEFAULT_*` consts) and the diarize `#[cfg(test)]` modules. Other command groups live in `commands/` (below) |
| `commands/` | Cohesive `#[tauri::command]` groups split out of `commands.rs`, each declared `mod x;` + re-exported `pub use x::*;` so the `commands::<name>` paths in `lib.rs`'s `generate_handler!` stay unchanged: `notes` · `folders` · `settings` · `summary_prompts` · `assets` (path/Finder) · `api_keys` (per-provider keychain + `provider_key_test`) · `transcription_config` · `models` (local-whisper + diarize model lifecycle) · `summary` (`summarize_note`/`run_summary`) · `permissions` · `local_llm` · `chat` (conversations, sends, breadth, note embedding/reindex) · `clients` (Client entity CRUD) · `cloud` + `cloud_worker` (auth, workspaces, sync) · `export` · `mcp` (`mcp_server_path`, the bundled `humla-mcp` binary's path for the Settings config snippet). Helpers still called by the core are `pub(crate)` and imported back into `commands.rs` (`read_provider_api_key`, `read_transcribe_config`, `local_model_path`) |
| `chat/` | Chat over the user's notes with **agentic retrieval**. `mod.rs` (`run_chat` loop, `SYSTEM_PROMPT` + `system_prompt_with_date`, the two step ceilings `MAX_STEPS_NOTE = 6` / `MAX_STEPS_BROAD = 12` picked by `max_steps_for` — a note-scoped turn has the anchor as grounding, a note-less one needs the depth, `build_grounding` + `GROUNDING_CHAR_BUDGET = 24_000`, `assemble_prompt`, `validate_breadth`, typed message `Part`s, `derive_title`), `adapter.rs` (`ChatAdapter::step` — one streamed step offering tools), `providers.rs` (OpenAI + Ollama adapters), `tools.rs` (`search_notes` → `db::hybrid_search_chunks` FTS5 + semantic, reporting how many notes matched distinctly from how many were returned; `get_note`; `list_notes` with per-note summary lines and each note's Client; the bounded relative date window `within_days`/`until_days`; `ToolScope` breadth clamp), `cloud.rs` (workspace turns POST `/api/chat` and stream SSE re-emitted onto the same `chat_*` events — **retrieval runs server-side for workspace chat**, so `tools.rs` is Personal-only and changes must be mirrored). `search_notes`/`get_note` results carry citations; `list_notes` deliberately does not (a listing is an index, not a source). Retrieved content is framed as reference data, never instructions |
| `mcp/` | Humla's own **MCP server** (#172), built as a second binary (`src/bin/humla-mcp.rs`) from this crate and shipped inside the bundle at `Humla.app/Contents/MacOS/humla-mcp`. Opens `notes.sqlite` directly, so it works whether or not the app is running — no port, no token, filesystem permissions are the authorization. `tools.rs` is the seam: `execute(conn, workspace, name, args, now_ms)` behind six read-only tools (`search_notes`, `get_note`, `get_transcript`, `list_notes`, `list_folders`, `list_clients`); `server.rs` adapts it to `rmcp`'s `ServerHandler` and holds no logic. Deliberately a **parallel** module to `chat/tools.rs`, not a reuse of it — chat's specs are pinned pairwise against humla-cloud and deliberately terse for small models, while MCP wants richer descriptions, a `language` field and tools chat has no use for. What IS shared is the vocabulary: the three overlapping tools' names and argument names are pinned by a test, with every deliberate difference listed. Retrieval is **keyword-only by choice** (`query_vec: None`), which is what keeps it keyless, offline and free of a Keychain prompt. Search spans all three indexed sources (typed body, summary, transcript) and tags each excerpt with which one it came from — the transcript being searchable at all is the half of this that has to be *said*, since a client cannot see the index. The date window comes in **two forms**: relative (`within_days` / `until_days`, still the safer default) and absolute (`since` / `until`, `YYYY-MM-DD` and inclusive of the day named). `resolve_filter` owns the whole rule and is fallible for that reason alone — a malformed date **errors** rather than widening, two forms of one edge is a conflict rather than a narrowing, and an empty result echoes the window it resolved to (`window_echo`), which is what makes a hallucinated year visible instead of reading as an absence. The by-id header carries what a listing row carries (client, folder, who spoke) so arriving by id is never to know *less* about a note than skimming would have told you. Off until enabled (`mcp_enabled`), read-only, and no tool returns or references an audio path |
| `db.rs` | SQLite schema, CRUD, settings helpers (`get_setting`, `set_setting`, `delete_setting`); migrations: `migrate_summary_prompts` (legacy single-prompt → table), `migrate_transcribe_config` (v0.23 — collapse legacy flat keys into `transcribe_config` JSON), `migrate_per_language_v4` (v0.24 — wrap bare `ProviderConfig` row into `TranscribeConfig { default, per_language }`) |
| `stt/` | STT adapter abstraction. `adapter.rs` (`BatchSttAdapter` trait + `TranscribeCtx { model, language, bias_terms, prior_context, api_key, base_url }`), `config.rs` (`ProviderConfig` tagged union + `TranscribeConfig` with `resolve(language)`), `openai.rs` / `local.rs` / `deepgram.rs` / `groq.rs` (adapters), `openai_compat.rs` (shared multipart client used by OpenAI + Groq), `keychain.rs` (per-provider slots + cache type) |
| `recording.rs` | `RecordingSession` (child handles, inflight tasks, reader handle, `chunk_log` with per-chunk `source`, separate `mic_full_wav_path` + `sys_full_wav_path`, separate `mic_trail` + `sys_trail`, `transcript_at_start` snapshot for resume); `TranscriptTrail` (rolling 150-word window fed to Whisper as `prior_context`, one per source); `ChunkSource` enum (`Mic` / `Sys`); `Phase` enum (`Idle` / `Starting` / `Recording` / `Paused` / `Stopping` / `Diarizing`) |
| `local_whisper.rs` | On-device Whisper; `SharedContext` (lazy-loaded model, reused across chunks); `prewarm()` fires on `recording_start`; `Preset` enum (Fast/Balanced/Quality) bundling sampling strategy + `no_speech_thold`; `ModelKind` (`Multilingual` / `LanguageSpecific { language }`); registry covers `large-v3-turbo-q5`, `large-v3-q5`, `large-v2-q5`, `medium-q5`, `nb-whisper-large-q5` |
| `openai.rs` | Summary endpoint (cloud OpenAI **and** any OpenAI-compat local server) via `summarize_with_base`. Cloud path **streams** (`summarize_cloud_stream` → SSE via the shared `consume_chat_sse`) — a non-streaming reasoning-model call sits silent on the wire for minutes and a VPN/proxy reaps the idle connection, surfacing as a pre-headers `Kind::Request` unexpected-EOF that retries can't fix; `send_chat_with_retries` still covers genuinely transient send errors (2 retries, 500ms/1.5s). Ollama is detected by port `:11434` and routed to `ollama_native_chat` (streaming `/api/chat` with adaptive `num_ctx`, `keep_alive: 0`, Qwen-tuned sampling); other local servers stay on the OpenAI-compat path. Also: `client()` / `local_client()` (120 s vs 600 s timeouts), `is_reasoning_model()` for temperature handling, `list_models()` for Settings, `trim_runaway_repetition()` as the last-resort guard against Qwen content loops. Transcription is *not* here — that lives in `stt/openai.rs` (the adapter) and `stt/openai_compat.rs` (the shared multipart client) |
| `diarize.rs` | Speaker-diarize sidecar wrapper. Two engines (`community1`, `sortformer`) selectable via the `diarize_model` setting. Surfaces: one-shot `diarize_file(path)` invoked from `diarize_and_apply` post-stop, and model lifecycle (`status` / `download` / `delete`). All offline — no streaming sidecar |
| `presets.rs` | Backend mirror of frontend preset prompts; `{LANGUAGE}` substitution |
| `wav.rs` | Proper RIFF chunk walking; RMS for silence gate; mono-16k decoder |

### Sidecars

Two Swift Package binaries that run alongside the Tauri main process. Both bundled via `tauri.conf.json`'s `bundle.macOS.externalBin` and signed with the same Developer ID.

#### `audio-capture/` — recording

- **AVFoundation** for mic, **ScreenCaptureKit** for system audio.
- **Hidden from Dock** via `NSApplication.shared.setActivationPolicy(.prohibited)`.
- Built via `scripts/build-sidecar.sh`. Binary cached via SHA-256 stamp at `src-tauri/binaries/.audio-capture-<triple>.stamp` (override with `FORCE_SIDECAR_REBUILD=1`).
- **Parent-death watchdog** — polls `getppid()` every 2 s; exits if it sees PID 1 (reparented to launchd). Combined with the `setsid` detach in `recording_start`, this prevents zombie sidecars after dev reloads / crashes.
- Stdout events: `chunk` (with `source`, `path`, `start_ms`), `full_recording` (with `source`, `path`, `duration_ms`; one per source on shutdown), `stopped`, `paused`, `resumed`, `heartbeat` (frame counts + peaks), `error`.
- Writes parallel `mic-full.wav` + `sys-full.wav` for the entire recording in addition to per-chunk WAVs (filenames prefixed by source so they don't collide). Either may be absent if its source produced no frames (mic permission denied, or in-person meeting with no system audio).

#### `speaker-diarize/` — offline speaker diarization

- **FluidAudio Swift package** (Apache 2.0). Runs CoreML / ANE inference.
- Two engines:
  - **Community-1** — `OfflineDiarizerManager` (community-1 segmentation + VBx clustering with PLDA score normalisation). `clusteringThreshold: 0.5`, vs the community default 0.6. **Polarity: HIGHER stops merging earlier → MORE speakers; LOWER keeps merging → FEWER speakers.** This doc previously claimed 0.5 was chosen "so similar-sounding voices in the same room don't collapse onto one cluster" — that had the polarity backwards, and 0.5 in fact merges *more* readily than the default would. `speaker-diarize/main.swift` carries the authoritative note and warns that earlier notes inverted it.
  - **The speaker-count hint is the load-bearing control, not the threshold.** With no hint, VBx picks its own cluster count and collapses to a single speaker on conversations where one person dominates — a 45-minute 3-person meeting came back as one speaker on Auto and as three the moment the note's count was set to 3. `withSpeakers(exactly: n)` is the only reliable override, so `expected_speakers` matters more than any threshold tuning.
  - **Known limitation: `Auto` (no `expected_speakers`) collapses multi-speaker recordings.** Every note starts in this state, so *the default path is the unreliable one* — set the note's speaker chip for any multi-person recording. Deliberately not worked around in code: the two failure modes are indistinguishable without knowing the answer (a 3-person meeting at threshold 0.5 collapses to 1; a genuine voice memo at 0.8 splits into 4), so any automatic retry that fixed meetings would wreck single-speaker recordings.
  - **Don't try to fix that by re-tuning `clusteringThreshold` — measured, it doesn't work.** Sweep over the same 45-minute 3-speaker mic stream with no hint: **0.5 → 2 speakers / 293 segments, 0.6 → 2 / 293, 0.7 → 2 / 293** (byte-identical: the knob is inert across that range, and the 2nd "speaker" is a single stray segment at 45:15), then **0.8 → 4 / 373**, overshooting the real 3. The hint gives 3 / 376. Segment counts at 0.8 and at the hint are close, so segmentation was never the problem — only the cluster count is.
  - **Sortformer** — NVIDIA end-to-end diarizer running in batch over the saved WAV. Fixed 4-speaker cap, no count hint. Designed to handle rapid back-and-forth that the clustering approach struggles with.
  Active engine picked by the `diarize_model` setting. Both can be downloaded independently.
- Built via `scripts/build-diarize.sh` — same Developer ID + hardened runtime as audio-capture, no entitlements file (just reads a WAV and runs CoreML inference).
- Subcommand-style CLI:
  - `speaker-diarize <wav>` — one-shot offline diarization. Loads the active engine's models (downloading + compiling on first run), runs inference, returns a JSON array of `{start_ms, end_ms, speaker_id}` segments and exits.
  - `speaker-diarize status` — checks engine model presence on disk; emits `{downloaded, sizeBytes, path}` JSON.
  - `speaker-diarize download` — fetches + compiles models; streams `{event:"progress", fraction, phase}` updates (phase ∈ `listing` / `downloading` / `compiling`) followed by `{event:"done"}`.
  - `speaker-diarize delete` — wipes the engine's cache directory.
- Lifecycle: short-lived. Spawned by `diarize_and_apply` after `recording_stop`, runs once over `full.wav`, exits. No long-running process, no in-memory speaker state across recordings (clustering is fresh per recording, which is correct since FluidAudio can't unify identities across independent sessions anyway).

## macOS specifics

- **Bundle id** `no.humla.app`. Stable Developer ID signature → TCC permissions (Microphone / Screen Recording) persist across rebuilds.
- **Entitlements** (`src-tauri/entitlements.plist`) — mic input, network client, screen capture usage description, no app-sandbox.
- **Tauri webview limitation** — `window.prompt` / `confirm` / `alert` are blocked by the Tauri webview to avoid main-thread deadlock. Use inline input UIs (folder creation in Sidebar + Note's FolderPicker, etc.).

## Local data layout

- **DB** — `~/Library/Application Support/no.humla.app/notes.sqlite` (SQLite, WAL). Schema: `notes` (with `language`, `summary_preset`, `summary_provider`, `expected_speakers`, `folder_id`, `speakers`, `detected_language` columns), `folders`, `settings`, `summary_prompts`. `note_chunks.chunker_version` records which chunker shaped a row (1 = pre-v0.40 blank-line splitting, 2 = turn-packed transcripts); `db::notes_with_stale_chunks` counts live notes below `CHUNKER_VERSION` so Settings → Chat can offer a rebuild only when one would do something. **Bump `CHUNKER_VERSION` whenever chunk boundaries change.** `notes.speakers` and `note_chunks.speakers` are **derived, local-only and never synced** — written solely by `db::reindex_note` from the transcript text, delimiter-wrapped (`|Michael|Hege|`) so an exact-label match can't merge two people. See `docs/adr/0002-no-person-entity-speakers-are-derived.md`: they are a cache, not a record, and are destroyed with the note that produced them. **`notes.detected_language` follows the same rule** (#167) — the ISO 639-1 code the STT provider reported, decided post-stop by a length-weighted vote across the capture's chunks, written only for notes recorded on `auto`. It resolves `auto` → a real language before the summary prompt is built, so the model gets a hard "write in X" directive instead of having to infer the target from a prompt whose every label is Norwegian. Written by `db::set_detected_language`, **not** through `NotePatch` — like the `speakers` write it deliberately leaves `updated_at` alone, since that is cloud-sync's last-write-wins key and a derived local write must never beat a teammate's real edit.
- **Settings** — `settings` table inside the same DB. Notable keys: `transcribe_config` (typed JSON, the source of truth for STT routing — wraps default + per-language overrides), `language`, `custom_vocabulary`, `summary_model`, `summary_provider`, `summary_prompt`, `default_summary_preset`, `diarize_model`, `community1_threshold`, `sortformer_silence_threshold`, `sortformer_pred_threshold`, `keep_audio`, `silence_rms_threshold`, `local_llm_base_url`, `local_llm_model`, `local_llm_think`, `theme`, `developer_mode`, `mcp_enabled`. Plus migration flags (`summary_prompts_migrated`, `migrated_transcribe_config_v3`).
- **API keys** — macOS Keychain, service `no.humla.app`, accounts `openai_api_key` / `deepgram_api_key` / `groq_api_key`. Read via `read_provider_api_key(state, "openai")` etc.; cached on `AppState.api_key_cache`. The OpenAI key has a one-shot migration from a pre-Keychain SQLite plaintext row.
- **Local Whisper models** — `~/Library/Application Support/no.humla.app/models/` (e.g. `ggml-large-v3-turbo-q5_0.bin` ~547 MB, `nb-whisper-large-q5_0.bin` ~1.1 GB). Downloaded on demand from HuggingFace.
- **FluidAudio diarization models** — `~/Library/Application Support/FluidAudio/Models/` (community-1 set ~30 MB, sortformer separate). FluidAudio writes to its own Application Support root because the path is hardcoded inside the Swift package.
- **Audio temp** — `tempfile::TempDir` per recording session; cleaned at the end of the post-stop chain (after diarize finishes — sequenced behind it because a parallel timer-based cleanup raced FluidAudio's WAV reader on long recordings). Holds per-source per-chunk WAVs (`mic-chunk-NNNN.wav`, `sys-chunk-NNNN.wav`) and per-source full-recording WAVs (`mic-full.wav`, `sys-full.wav`). Either full WAV may be absent if its source produced no frames.
- **Playback assets** — `~/Library/Application Support/no.humla.app/recordings/<note_id>/` holds `timeline.jsonl` (always) plus a mixed `playback.wav` and the raw per-source `mic.wav` / `sys.wav` (only when `keep_audio` is on), written by `write_playback_assets` + `maybe_keep_audio` post-stop. **`keep_audio` is the single, absolute gate on audio (#24)** — `sessions::retain_audio` owns the decision and `commands::keep_audio_enabled` reads it; off means *no* WAV is written, uploaded or downloaded on this device, including a teammate's synced audio (the setting is device-scoped, so `download_note_audio` / `download_note_sessions` honour it too — `session_download_plan` drops `playback` and keeps `timeline`). Before #24 the setting only governed the raw copies while `playback.wav` was written unconditionally, so "off" silently kept a full recording of the meeting; **don't reintroduce an exception above it** — #16's "a note's 2nd take force-retains its sources so #17 can unify them" was one, and it is deliberately gone (a multi-take note recorded with retention off simply can't be unified). `timeline.jsonl` is text (word timings) and is always kept: it drives the merged reader and the session dividers, which is why the note view renders the full styled transcript with no audio and only swaps the player row for one line of explanation. Turning the setting off is going-forward only; **Settings → Recording → "Delete stored audio…"** (`stored_audio_stats` / `delete_stored_audio`) is the explicit sweep for audio already on disk, keeping every transcript, timeline and `chunks.json` and pinging the sync observer per note so removals replicate.

## Build & distribution

| Command | What it does |
|---|---|
| `pnpm dev` | Vite dev server only (frontend) |
| `pnpm mock` | Visual-check harness (`mock.html` + `src/mockBoot.tsx`): renders a real component against a mocked Tauri IPC in a plain browser, so layout/token bugs jsdom can't see are visible. Pick a scenario with `?case=<name>`. Dev-only — Vite builds `index.html` only, so it never ships. **A scenario wrapper must mirror the real container** (e.g. the onboarding canvas in `Onboarding.tsx`); a plain `<div>` wrapper left `StepShell`'s `max-w-lg` box hugging the left edge and looked exactly like a layout bug in the component under review |
| `pnpm lint` | ESLint over `src/` — a **correctness** net, not a style pass (there's no formatter). `react-hooks/rules-of-hooks` is the reason it exists: a hook below an early return type-checks, passes every unit test of the components it renders, and then crashes the whole view with "Rendered more hooks than during the previous render". `exhaustive-deps` is a warning because several omissions here are deliberate and commented. Type-aware rules are off — `pnpm build` runs `tsc -b`. Expect 0 errors / ~5 warnings |
| `pnpm tauri dev` | Tauri dev (assumes sidecars already built) |
| `./scripts/build-sidecar.sh` | Build + Developer ID sign the audio-capture Swift sidecar (skips if unchanged) |
| `./scripts/build-diarize.sh` | Build + Developer ID sign the speaker-diarize Swift sidecar (skips if unchanged) |
| `./scripts/build-mcp.sh` | Build + Developer ID sign the `humla-mcp` server binary into `src-tauri/binaries/`. No source-hash skip (cargo already does that); it writes a zero-byte placeholder first, because `humla-mcp` is a declared `externalBin` and tauri-build fails the very compile that produces it when the file is missing |
| `pnpm icon` | Regenerate the macOS app icon from `src-tauri/icons/source.png` |
| `pnpm tauri build` | Production bundle (`.app` + `.dmg`). It does **not** build the sidecars — `beforeBuildCommand` is just `pnpm build` (the frontend), so a bare `pnpm tauri build` bundles whatever binaries already sit in `src-tauri/binaries/`, however stale. Run the build scripts first, or use `pnpm dmg`, which is the supported path |
| `pnpm dmg` | Wrapper: builds `audio-capture`, the Metal library and `humla-mcp`, then `pnpm tauri build`; prints final DMG path. Note it does **not** run `build-diarize.sh` — the diarize sidecar is expected to be already built in `src-tauri/binaries/`, so run that script by hand after changing its Swift |
| `pnpm release` | Full release pipeline: build + notarise + staple + sign updater payload + tag + push + GitHub release |

DMG output lands in `src-tauri/target/release/bundle/dmg/`.

## Distribution & signing

Builds are signed with the **Developer ID Application: MICHAEL MEHLUM WILHELMSEN (NBUP88JQ35)** identity (configured in `src-tauri/tauri.conf.json` under `bundle.macOS.signingIdentity`). Both sidecars get the same Developer ID + hardened runtime; the audio-capture sidecar additionally uses `src-tauri/sidecar.entitlements` (mic input).

### Notarisation

Notarytool credentials live in `.env.notarise` (gitignored) at the repo root:

```
export APPLE_API_KEY=<10-char Key ID>
export APPLE_API_ISSUER=<Issuer UUID>
export APPLE_API_KEY_PATH=/Users/michaelwilhelmsen/.private_keys/AuthKey_<Key ID>.p8
```

`scripts/build-dmg.sh` sources this before invoking `pnpm tauri build`. Tauri's bundler detects the env vars and runs `xcrun notarytool submit --wait` + stapler automatically.

If `.env.notarise` is absent, the build is still Developer ID signed but not notarised — first launch needs right-click → Open.

### Updater signing key

Tauri's auto-updater uses a separate Ed25519 keypair from the Apple Developer ID — it signs the **update payload** so the app verifies the DMG hasn't been tampered with before installing.

- **Private key**: `~/.private_keys/humla-updater.key` (passwordless, ~700 perms). Treat with the same care as the notarisation `.p8`. Losing it means you can't ship updates that existing installs will accept — you'd have to publish a new app with a new public key.
- **Public key**: `src-tauri/tauri.conf.json` under `plugins.updater.pubkey`. Bundled into every build. Don't change it once shipped or every existing install stops accepting updates.
- The build script reads the private key path from `.env.notarise` (env var `TAURI_SIGNING_PRIVATE_KEY`).

### Verifying a release

```
spctl --assess -vv /Applications/Humla.app
# expect: accepted, source=Notarized Developer ID
```

### Reading notarisation failure logs

```
xcrun notarytool log <submission-id> \
  --key $APPLE_API_KEY_PATH \
  --key-id $APPLE_API_KEY \
  --issuer $APPLE_API_ISSUER \
  | jq
```

Common failure causes: nested binary missing hardened runtime, missing entitlement, wrong identifier on a Framework, executable bit lost during copy.

## Releases

Run `pnpm release` to ship a new version. The script builds a notarised + stapled DMG, signs an updater manifest, creates a GitHub release, and uploads all assets so existing installs see the update.

**Before each release, bump the version number in three places** (they must match exactly, or auto-update will misbehave):

1. `package.json` → `"version": "X.Y.Z"`
2. `src-tauri/tauri.conf.json` → `"version": "X.Y.Z"`
3. `src-tauri/Cargo.toml` → `version = "X.Y.Z"`

Convention: semver. Bug fix → patch (`0.24.0` → `0.24.1`). New feature → minor (`0.23.0` → `0.24.0`). Breaking schema change → major (rare).

The script:
1. Refuses to run if the working tree is dirty or the version isn't bumped beyond the latest GitHub release.
2. Builds the DMG (`pnpm dmg`), signs + notarises + staples + produces a `.sig` file via the Tauri updater key.
3. Generates `latest.json` with version, signature, and the GitHub download URL.
4. Tags the commit `v<version>`, pushes the tag, creates a GitHub release, uploads `.dmg` + `.sig` + `latest.json` as assets.

All existing Humla installs poll the updater endpoint at startup and prompt to install when a new version lands.

## Agent skills

### Issue tracker

Issues are tracked in GitHub Issues (`michaelwilhelmsen/humla`) via the `gh` CLI; external PRs are not a triage surface. See `docs/agents/issue-tracker.md`.

### Triage labels

Default vocabulary — `needs-triage` / `needs-info` / `ready-for-agent` / `ready-for-human` / `wontfix`, mapping 1:1 to GitHub labels. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `CONTEXT.md` + `docs/adr/` at the repo root. See `docs/agents/domain.md`.
