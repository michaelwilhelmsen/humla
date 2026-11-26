# Humla — project notes

## What this app is

**Humla** (Norwegian for "bumblebee") is a personal macOS meeting-notes app inspired by Granola. You type notes during a meeting while the app records mic + system audio, transcribes it, and produces an AI summary that fuses your notes with the transcript. Personal/small-team, not SaaS — your data, your API keys, local SQLite, no backend.

Feature detail lives in `docs/features/` and `docs/architecture/`; read the linked file before changing that area. The rules below are the ones an agent must not break.

## Core capabilities

- **Hybrid capture** — mic and system audio are **two separate streams end-to-end** (no mixdown): own VAD chunks (min 1.0 s / max 15 s / 500 ms silence), own `full.wav`, own Whisper calls with their own `prior_context` trail. → [recording pipeline](docs/architecture/recording-pipeline.md)
- **STT providers** — OpenAI (whisper-1 / gpt-4o-transcribe / mini / diarize), on-device Whisper via Metal, Deepgram (nova-3, nova-2, base), Groq (whisper-large-v3-turbo), and a custom OpenAI-compatible endpoint; all behind `stt::BatchSttAdapter`.
- **Per-language routing** — `transcribe_config` (typed JSON, single source of truth) is `{ default: ProviderConfig, per_language: BTreeMap<String, ProviderConfig> }`. Resolution: note language → per-language override → default. Each note has a language chip overriding the global Settings → Language.
- **Whisper quality preset** (local only) — Fast (greedy) / Balanced (beam=3) / Quality (beam=5, low `no_speech_thold`).
- **Custom vocabulary** — `initial_prompt` for Whisper-shaped providers; Deepgram `keyterm` (Nova-3) or `keywords` (others).
- **Trailing context** — each chunk gets the last ~150 committed words as `prior_context` (Deepgram ignores it). The main guard against silence hallucinations and proper-noun drift.
- **Offline diarization on stop** — Nemotron 3 (default) or Community-1, chosen per note, with fallback between them. → [diarization and echo](docs/features/diarization-and-echo.md)
- **Speaker echo removal** (#196) before any mic diarize beside a system stream. → [diarization and echo](docs/features/diarization-and-echo.md)
- **Speakers** — four colours cycling for 5+; rename is a line-anchored regex rewrite of the transcript, no metadata table.
- **Two-source summaries** — one user message with `[Notater] (user-written)` and `[Transkripsjon] (auto)` blocks; an empty side is sent as `(ingen)`, never omitted. Presets (Meeting / 1:1 / Lecture / Interview / Brainstorm / Voice memo) are deliberately minimal (kind + language). Custom prompts are `summary_prompts` rows referenced as `custom:<id>`.
- **Providers for summary/chat** — OpenAI, local (Ollama or any OpenAI-compatible server), Anthropic, GitHub Copilot (#227), custom endpoint (#173). → [providers](docs/features/providers.md)
- **Transcribe manually** (#146), replay and re-transcribe. → [recording pipeline](docs/architecture/recording-pipeline.md)
- **MCP server** (#172), **Send to webhook** (#213), **`humla://` record links** (#91), **Save audio** (#224). → [integrations](docs/features/integrations.md)
- **Cloud session + 2FA** (#221), **private notes** (#191, #232). → [cloud and private notes](docs/features/cloud-and-private-notes.md)
- **Chat over your notes** — agentic retrieval in a note's Chat tab, `/chat` and `/folder/:id/chat`. → [chat](docs/features/chat.md)
- **Timeline-backed transcript**, per-turn editing, automatic titles (#90), note card grid and folders. → [notes and transcript](docs/features/notes-and-transcript.md)
- **Clean transcripts** (#217) — a Clean | Verbatim view in the Transcript tab: a model's clean-verbatim rewrite per segment, aligned back to the timed words so highlighting works. → [notes and transcript](docs/features/notes-and-transcript.md#clean-view-217)

## Rules

### Audio and privacy

- **`keep_audio` is the single, absolute gate on audio (#24).** Off means no WAV/M4A is written, uploaded or downloaded on this device, a teammate's synced audio included. `sessions::retain_audio(setting, forbidden)` owns it; `audio_forbidden` for that note (a managed profile's anywhere, an organisation's only in its own departments, an unreadable policy or note) is its `forbidden` input and closes it whatever the setting says. `commands::keep_audio_enabled(app, note_id)` reads it, `download_note_audio` / `download_note_sessions` honour it (`session_download_plan` drops `playback`, keeps `timeline`), and `write_playback_assets` + `maybe_keep_audio` write audio only through it. Never add an exception above it (#16's force-retain for #17's unify was one and is gone: a multi-take note recorded with retention off simply can't be unified).
- No MCP tool or webhook payload returns or references audio or an audio path. → [integrations](docs/features/integrations.md)
- The echo pass's cancelled mic goes to `humla-echo-<uuid>.wav` in the **system temp dir**, deleted after the diarize reads it; never under `recordings/` or `diagnostics/`.
- **`input_device` is display-only (#174)**: persisted nowhere (not diagnostics, `chunks.json`, `sessions.json` or sync), carried as its own field on `diagnostic`, never interpolated into `message`; stderr goes through `redact_sidecar_line`. → [audio-capture](docs/architecture/audio-capture-and-notifications.md)
- `capture_timing` carries timestamps and device latencies, never a device name.
- The webhook confirmation shows the receiver's host only; no error carries the URL or the receiver's response; header values are write-only.

### Capture and transcription

- Keep the two streams separate end-to-end; don't reintroduce mixing. Don't infer capture topology from a stream merely being present: diarize **both** streams when both have content, and apply `You:` only when the mic diarize resolves to exactly one voice.
- `recording_stop` must drain inflight chunks **before** snapshotting `chunk_log`.
- **The system-audio source is chosen in `permissions::system_audio_source`, never in the sidecar** (#220), passed as `--sys-source tap|sck`: the process tap once System Audio Recording is granted (or neither it nor Screen Recording was ever granted), `none` when both are refused, else ScreenCaptureKit, so a Screen Recording user is never prompted mid-meeting. A denied tap records silence instead of failing; never start one then, and the sidecar stops one turned down at the prompt mid-take. The UI doesn't mirror that rule: `systemAudioKind` / `systemAudioReady` (`src/lib/systemAudio.ts`) only decide which permission it shows and asks for.
- **`recording_status` describes the live capture and nothing else** (#187); only the finishing capture's post-stop chain writes it. Re-diarize reports on `diarize_status`, replay on `transcribe_status`. `ChainProgress` (`PostStop` / `Rediarize` / `Replay` / `Silent`) is passed in; `diarize_and_apply`, `rediarize_apply_to_chunks` and `unify_note_speakers` never choose the channel.
- The backend owns the single capture slot through the whole stop chain: `recording_start` and `import_audio` refuse on `StopState`.
- **A discrete step counter is text, never the progress value** (#189). `discreteCounter` in `RecordingBar.tsx` is the one place this lives. Step index/count derive from `recording::stream_reports`; never write `1, 2` by hand. Keep `stop_steps` / `replay_steps` in step with the emitting code.
- **Transcribe manually** applies only when `keep_audio` is on and only to on-device Whisper (resolved through the note's language); read once at `recording_start`. `sessions::defer_transcription` owns it.
- A take's `transcribed` flag defaults to **true**; a replay that transcribed nothing is not marked transcribed. Replay never takes the live-capture slot and never streams into `note.transcript` (text arrives via `rebuild_note_transcript`).
- `sessions::TranscribeScope` ↔ the `"pending" | "all"` union on `ipc.transcribeNote` — change both. `takes_to_transcribe` is the single answer both surfaces mirror. `All` snapshots a note revision first.
- **Session scalar metadata is immutable, `duration_ms` included**: take a deferred take's length from the sidecar's `full_recording` event at capture time. `record_transcribed_take` writes the flag and nothing else.
- Every chain that writes a note's files (post-stop incl. import, replay, re-diarize, download) ends with `purge_note_assets_if_gone` after its last write; a new one must too.

### Diarization

- Engine choice is per note in `diarize::choose_engine` (pure rule `engine_preference`); every pass goes through `diarize_and_maybe_clean`, which retries with the other engine. → [diarization and echo](docs/features/diarization-and-echo.md)
- Speaker corrections after the split live in `refine_pieces`, which the transcript string, `serialize_timeline` and the diagnostics dump all run; never correct speakers in only one of them. A model's moves (`boundaries::review_note`) go into `timeline.jsonl`, and the transcript is rebuilt from it. → [diarization and echo](docs/features/diarization-and-echo.md#from-segments-to-turns)
- Mirrored, pinned by tests reading the Rust files — change both: `DEFAULT_DIARIZE_MODEL` / `Engine::from_setting` / `NEMOTRON_MAX_SPEAKERS` ↔ `DEFAULT_DIARIZE_ENGINE` / `selectedDiarizeEngine` / `NEMOTRON_MAX_SPEAKERS`, and `Engine::label` ↔ `DIARIZE_ENGINE_LABEL` (all in `src/lib/diarizeEngine.ts`). `enginesToDownload` must fetch what `engine_preference` falls back to. Speaker counts (Auto, 1–12) come from one list, `src/lib/speakerCount.ts`.
- FluidAudio is **pinned exactly at 0.17.4**; any bump is checked for output changes. Never use a split W8A8 Nemotron preset (wrong on M1-class Neural Engines); check preset changes on M1–M3.
- Community-1 threshold polarity: stored `community1_threshold` higher → **more** speakers. The sidecar converts it (`fluidAudioClusteringThreshold`); never pass it raw. The speaker-count hint is the control; don't try to fix `Auto` by re-tuning `clusteringThreshold` or adding automatic retries.
- Echo-pass changes on takes over ten minutes must be judged against several baselines, never one; keep `echo-probe`'s `cancel` as the two-pass reference.

### Transcript and notes

- The timeline is canonical (#169, [ADR-0004](docs/adr/0004-the-timeline-is-canonical-for-a-notes-content.md)). Edits on a timeline note go through `note_timeline_set_chunk_text`; only a note with no timeline uses the whole-transcript textarea. The client never re-derives the backend's grouping rule; compare word sequences, never line counts. `commit_rebuilt_transcript` refuses an empty projection over a non-empty transcript. → [notes and transcript](docs/features/notes-and-transcript.md)
- `split_label` (Rust) ↔ `parseTranscriptLines` (`Note.tsx`) — the same "`Label: ` prefix" rule; change both.
- **Clean (#217) is a derived, local-only reading; the timeline and `notes.transcript` stay verbatim** and are what summary, chat, export, MCP and webhooks read. The model's rewrite is shown only after the guards in `clean::refuse` (negations, numbers, length, share of words actually said); a refused segment stays verbatim. Segments live in the take's `clean.json`, keyed by `clean::fingerprint` of their entries' words, and attach in `note_timeline`. Its provider goes through `resolve_provider`, like summaries. `clean::CleanRequest` ↔ the `CleanRequest` union in `ipc.ts`, and `clean::clean_enabled` ↔ the `!== "false"` checks on `clean_transcripts` (`Note.tsx`, Settings → Transcription) — change both.
- `menubar::is_replaceable_title` ↔ `isReplaceableTitle` (`src/lib/noteSync.ts`) — change both. Automatic titles never run during a recording or on summary completion, and fail silently.
- `notes.speakers`, `note_chunks.speakers` and `notes.detected_language` are derived, local-only, never synced, and their writes leave `updated_at` alone (`db::set_detected_language`, not `NotePatch`). See [ADR-0002](docs/adr/0002-no-person-entity-speakers-are-derived.md).
- **Bump `CHUNKER_VERSION` whenever chunk boundaries change.**
- `notes.private` syncs and bumps `updated_at`; only the author may change it or `shared_with`. Applying a pulled revocation is guarded three ways (same workspace, non-empty `owner`, owner not us). The chat index's `IndexStore.viewerSql` is fail-closed. → [cloud and private notes](docs/features/cloud-and-private-notes.md)
- `db::purge_note_rows` owns what a note takes with it (chunks, FTS rows, revisions, its Chat tab's conversations); `db::purge_note` wraps it in a transaction, and `cloud-sync` calls it through `Config.purge_note` inside its own. A new note-keyed table joins `purge_note_rows`. `org_audit_queue` is the exception: the audit trail outlives the note.
- **Organisation retention deletes only through `org_policy::retention_in_force`**, which reads a cache that parses cleanly and never `effective_for_or_strictest`. It covers notes whose `workspace_id` an organisation with `retention_days > 0` lists, never Personal, by `created_at`, Trash included; a `created_at` below `MIN_EPOCH_MS` (10^12) is never past it. It purges, never trashes, pushes nothing, and skips busy notes (`commands::org::note_busy`); `recording_start` and `transcribe_note` refuse a note past its cutoff. A pulled note or tombstone past the cutoff is purged in `cloud-sync` (`purge_if_expired`), never inserted or trashed. In a retention workspace a note moves in only within its cutoff (`check_move`) and is re-dated only earlier and not past the cutoff (`org_policy::redate_bounds` ↔ `redateBounds`, pinned by `src/lib/redateVectors.json`).

### Chat

- `resumes_on_open` (Rust) ↔ `targetResumesOnOpen` (TS), and `pinned_breadth` ↔ `targetPinsScope` — change both.
- Workspace chat retrieval runs server-side: changes to `chat/tools.rs` must be mirrored in humla-cloud (tool schemas are pinned pairwise). The MCP tools' overlapping names/arguments with chat are pinned by a test.
- `ChatTarget::from_ids`: note and folder ids are alternatives; both, or an empty id, is an error. → [chat](docs/features/chat.md)

### Providers

- `providers.rs` (`REGISTRY`, `ProviderId`) ↔ `src/lib/providers.ts`, pinned by `providers.test.ts` (#192) — change both. The resolvers match exhaustively; an unknown stored id is an error, never a silent OpenAI call. `"ollama"` is the stored alias for `Local` (the `chat_provider` setting never migrated).
- Reasoning models (gpt-5+, o-series) get no `temperature` (`is_reasoning_model()`). Cloud OpenAI summaries stream; keep it that way.
- Titles and the speaker check run on `summary::use_light_model`: OpenAI's `LIGHT_OPENAI_MODEL` (titles with `reasoning_effort: "none"`, the speaker check with `"low"` via `clean::light_provider`), Copilot's cheapest model. Clean uses `CLEAN_OPENAI_MODEL` with `"low"` on OpenAI. `reasoning_effort` is sent only where a caller sets it; `gpt-6.2-sol` refuses `none`.
- Ollama goes through native `/api/chat` (detected by `:11434`), mirrored in TS by `isOllamaUrl`. Qwen-specific sampling applies only to Qwen models.
- Anthropic: never send `thinking`, `temperature`, `top_p` or `top_k`.
- Custom endpoint: base URL used exactly as typed; send only `model`, `messages`, `stream` (and `tools`). Nothing is guessed about the server.
- Copilot: always through the CLI (never the Copilot API directly), started with `--no-auto-update`, `--disable-builtin-mcps`, `--disable-client-telemetry` and the SDK's `disable_client_telemetry`; one SDK session per chat turn; log usage lines, never content. Copilot uses `copilot_model` / `copilot_chat_model`, not `chat_model`.

### Organisation policies

- An organisation's policies govern only notes in its own departments (`org_policy::effective_for(conn, workspace_id)`: the managed profile plus the organisation naming that workspace in `workspaces` or `locked_workspaces`, the departments behind unmet 2FA; Personal and other workspaces get the profile alone). An organisation from an older server naming no department but with a policy on governs every workspace no organisation claims (`orgs_for`); nothing is deleted on that guess. Every enforcing path passes the note's workspace (`effective_for_note`, `check_endpoint`, `check_leaves_mac`); nothing enforces from a device-wide merge. Only `telemetry_off` is app-wide (`effective_device`). A cache that can't be read is strictest for a department, never for Personal. Settings rewrites (`apply_local_only`, `apply_device_policies`) follow the managed profile only, plus `telemetry_off`, since the user's settings are also Personal's.
- A governed note transcribes through `transcription_config::resolve_for_note` (refused slots → on-device Whisper, announced once by `transcription_kept_local`); summaries, titles, chat, webhooks and MCP refuse instead, checked on the provider and URL actually used after a per-note override (`summary::resolve_provider`). An organisation's vocabulary joins prompts only for its departments' notes (`vocabulary_for`), and Settings → Vocabulary shows it only in those departments. Local chat is Personal, held to the profile; a department's note is checked where it enters a turn or the embedder. MCP is switched off only by the profile; a governed active workspace refuses per call (`mcp::workspace_refusal`).
- `check_move` refuses moving a note out of a department whose organisation sets `local_only`, `audio_forbidden` or `retention_days` (`governs_notes`), except to another of its departments. An organisation's audio sweep deletes audio of its departments' notes only (`SweepScope`), recorded per department as `<org>/<workspace>` in `org_audio_swept`; the profile's sweeps everything. A sync conflict copy of a governed note stays in its workspace (`cloud-sync` `Config.governed`). A note moving into an `audio_forbidden` department loses its stored audio in `notes_set_workspace` (clean cache only, `forbids_audio`).

### Organisation admins

- An org admin edits templates and terms on the server only (`commands/org_admin.rs`), never through a local queue; every write ends with `refresh_org_config`. The `organisations` PATCH carries `vocabulary` and nothing else. A template save keeps the departments the admin can't see (`scope_workspaces`). Policies are never edited in the client. An organisation the admin is in no department of is read straight from `org_templates` / `organisations` (`org_admin_content`) into the editing views only: never cached, applied or offered in a picker.

### Cloud session

- The password is only a fallback and is dropped the first time the server answers `two_factor_required`. The sync worker never signs in; it reports `signed_out` / `two_factor_required`. A change made while a workspace is 2FA-locked is never dropped.
- A managed configuration profile (`org_policy::managed`, source "This Mac's configuration") counts only forced values, reads a policy key of the wrong type as on, never sets retention, and pins `server_url` by writing `cloud_base_url` and dropping the whole stored account; forcing `audio_forbidden` to `true` owes the one-time audio sweep (`mac-configuration`). → [cloud and private notes](docs/features/cloud-and-private-notes.md)

### Backend runtime

- **Use `tauri::async_runtime::spawn`, not `tokio::spawn`, in anything run from Tauri's `setup`** — bare `tokio::spawn` panics there and aborts the app on launch.
- **The sync observer is built before `AppState` is managed** (`run_with_sync` → `make_sync`): nothing it runs while being built, or spawns then, may call `app.state::<AppState>()`. It takes the DB and Keychain cache from `SyncContext`.
- **Never hold a `parking_lot` guard across an `.await`** (the future becomes non-Send). Use `tokio::sync::Mutex` for state held across awaits (e.g. `transcribe_gate`).
- Secrets go through `crate::keychain` (`read` / `read_or` / `write`), cached per account on `AppState.keychain_cache`; not `keyring_core::Entry` directly; `keychain::entry` sets the store first.
- HTTP clients come from `crate::http::client` / `client_builder` (cloud-sync: its own `http::client`), never `reqwest::Client::new` / `builder`: they turn proxies off (macOS proxy settings would catch local servers) and install ring, without which reqwest panics. A test enforces it.
- `window.prompt` / `confirm` / `alert` are blocked in the Tauri webview; use inline UI or the Modal component.

### Frontend and design

- Every anchored panel goes through `src/components/ui/` (`Menu`, `Popover`, `Select`, `Combobox`); don't hand-roll a popover. Not shadcn. → [frontend](docs/architecture/frontend.md)
- Page background is `--color-canvas`, not `--color-bg`; `--color-pill` is transparent (use `--color-pill-hover`). Speaker colour 4 is `--color-speaker-4`, not `--color-accent`.
- Gold accent text/icons use `--color-accent-text`; filled accent is only `.nd-btn-primary`. Red is never the accent (`--color-record`, `--color-danger`).
- One sans per theme; no serif headlines, no uppercase labels.
- Don't reference deleted classes: `.nd-action`, `.nd-prop-*`, `.nd-select`, `.nd-ctl`, `.rec-bar`.
- A theme is one CSS file + one `PALETTE_REGISTRY` row + one `@import`; `themes.contract.test.ts` enforces the contract in `src/lib/themeContract.ts`.
- `NoteToolbar` thresholds, `BODY_MIN` and `ROW_STEPS` come from `scripts/measure-toolbar.js` / `scripts/measure-recording-bar.js`; re-derive them after changing those rows or a theme's control metrics.
- In the mock harness (`pnpm mock`) a scenario wrapper must mirror the real container, or it shows layout bugs that aren't real.
- `tsc` is TypeScript 7 (the Go compiler, installed as `@typescript/native`). The `typescript` package is TypeScript 6 (`@typescript/typescript6`, binary `tsc6`), because typescript-eslint and `scripts/check-release-notes.mjs` need the JavaScript compiler API, which 7 doesn't have. Don't point `typescript` at 7.
- `clamp_device_name` (Rust) ↔ `noAudioWarning` (TS) share pinned test vectors — change both.

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
│  │ settings    │  │ process tap/SCK │  │ (CoreML / ANE)  │  │
│  └─────────────┘  └─────────────────┘  └─────────────────┘  │
│                                                             │
│  ┌─────────────────────────────────┐  ┌─────────────────┐   │
│  │ HTTPS clients                   │  │ Local Whisper   │   │
│  │ OpenAI · Deepgram · Groq · HF   │  │ whisper-rs 0.16 │   │
│  └─────────────────────────────────┘  └─────────────────┘   │
└─────────────────────────────────────────────────────────────┘
```

Recording in short: `recording_start` spawns `audio-capture` (via `setsid`); each `chunk` event runs `transcribe_chunk` against the capture's `CaptureSink` (`SinkMode::Live` / `Deferred` / `Replay`), serialised on `transcribe_gate`, appending raw text and emitting `transcript_replaced`; `recording_stop` drains, then diarizes, writes playback assets, unifies speakers across takes and titles the note. Full flow: [recording pipeline](docs/architecture/recording-pipeline.md).

## Tech stack

- **Frontend**: React 19, TypeScript 7, Vite 8, Tauri 2, React Router 7, Zustand (`useNotesStore`, `useRecordingStore`, events bound once in `bindBackendListeners`), Tiptap 3, Tailwind v4, Radix, lucide-react, react-markdown + remark-gfm. Two themes (warm, graphite) × light/dark. → [frontend](docs/architecture/frontend.md)
- **Backend**: Rust 1.94 (`copilot-sdk`'s floor), Tauri 2, rusqlite (`bundled`, WAL, idempotent ALTER TABLE migrations, indexes created after migrations), reqwest 0.13 (rustls on ring, `stream`), tokio (`spawn_blocking` for local Whisper), whisper-rs 0.16 (`metal` on macOS only; `large-v3-turbo-q5_0` ~547 MB; NB Whisper Large for Norwegian), parking_lot, keyring-core + `apple-native-keyring-store`, serde/serde_json/chrono/uuid/anyhow/async-trait.

### Module map (`src-tauri/src/`)

| File | Responsibility |
|---|---|
| `lib.rs` / `main.rs` | `AppState`, command registration, plugin setup, startup migrations / Tauri entry |
| `commands.rs` | Module root of `crate::commands`: module declarations, `ChainProgress` / `StreamSteps`, settings readers (`keep_audio_enabled`, `selected_diarize_engine`, …), shared helpers (`err`, `emit_*`, sync pings, `sidecar_path`, `DEFAULT_*`) |
| `commands/` | `#[tauri::command]` groups, each `mod x;` + `pub use x::*;` so `generate_handler!` paths stay `commands::<name>`: `notes`, `capture` (the recording lifecycle: `recording_start` / `stop` / `pause` / `resume` / `state`, `dispatch_sidecar_event`, the post-stop chain, stop and capture timing writers, `clamp_device_name`, `redact_sidecar_line`), `timeline` (`serialize_timeline`, `note_timeline*`, `note_sessions`, transcript repair and per-chunk edits), `import` (`import_audio`, `finish_import`, `title_from_filename`), `deferred` (deferred transcription, #146: `transcribe_note`, `replay_retained_stream`, `record_transcribed_take`), `diarization` (diarize on stop and re-diarize: `diarize_and_apply`, `rediarize_note`, `diarize_and_maybe_clean`, the echo pass `EchoPass`, speaker numbering across streams and takes, diagnostics dumps), `folders`, `settings`, `summary_prompts`, `assets`, `api_keys`, `transcription_config`, `models`, `summary` (`summarize_note` / `run_summary`), `title`, `permissions`, `local_llm`, `chat`, `clients`, `cloud` + `cloud_worker` + `cloud_auth`, `export` (incl. `note_audio_export_info` / `export_note_audio`), `mcp`, `webhook` (`webhook_destination`, `webhook_send_note`), `copilot`, `clean` (`note_clean`, the clean read, #217). These hold no commands and aren't glob re-exported: `boundaries` (a model's check of speaker changes, run before the clean read), `transcribe` (`transcribe_chunk` + hallucination/repetition/attribution guards, the detected-language vote), `transcript` (`build_labelled_transcript`: per-speaker pieces, mic-echo dedup, and `refine_pieces`: interjection bridging, sentence-boundary moves, continuation chains), `playback` (`write_playback_assets`: the AAC playback mix and `timeline.jsonl`) and `unify` (`unify_note_speakers`: cross-take speaker unification, #17). Helpers used by the core are `pub(crate)` (`read_provider_api_key`, `read_transcribe_config`, `local_model_path`) |
| `chat/` | Agentic retrieval loop, adapters, Personal-only tools, workspace turns via humla-cloud. → [chat](docs/features/chat.md) |
| `mcp/` | The `humla-mcp` server (`src/bin/humla-mcp.rs`); `tools.rs` is the seam, `server.rs` adapts to `rmcp`. → [integrations](docs/features/integrations.md) |
| `db.rs` | Schema, CRUD, settings helpers (`get_setting`, `set_setting`, `delete_setting`), migrations (`migrate_summary_prompts`, `migrate_transcribe_config`, `migrate_per_language_v4`, `migrate_diarize_engine`) |
| `keychain.rs` | One service (`SERVICE`), per-account `Cache`, `read` / `read_or` / `write` (`read_or` migrates the OpenAI key's old SQLite row) |
| `providers.rs` | `REGISTRY` of `ProviderSpec` rows behind `ProviderId`; auth schemes `Bearer`, `Token`, `AnthropicKey` via `headers(key)` (Anthropic needs `x-api-key` + `anthropic-version`). Read by `keychain_account_for`, `canonical_provider_id`, `provider_key_test`, the summary/chat resolvers and `ProviderConfig::provider`. Mirrored by `src/lib/providers.ts` |
| `stt/` | `BatchSttAdapter` + `TranscribeCtx { model, language, bias_terms, prior_context, api_key, base_url }`; `config.rs` (`ProviderConfig`, `TranscribeConfig::resolve`); adapters `openai` / `local` / `deepgram` / `groq` / `custom`; `openai_compat.rs` shared multipart client; `keychain.rs` |
| `recording.rs` | `LiveCapture` (incl. `transcript_at_start` for resume), `CaptureSink` + `SinkMode` (`for_stream` re-points a sink at a take's other stream), `TranscriptTrail`, `ChunkSource` (`Mic` / `Sys`), `Phase` (`Idle` / `Starting` / `Recording` / `Paused` / `Stopping` / `Diarizing`), `Step` |
| `local_whisper.rs` | `SharedContext`, `prewarm()` on start, `Preset`, `ModelKind` (`Multilingual` / `LanguageSpecific { language }`); models `large-v3-turbo-q5`, `large-v3-q5`, `large-v2-q5`, `medium-q5`, `nb-whisper-large-q5` |
| `openai.rs` / `anthropic.rs` / `custom_endpoint.rs` / `copilot/` | Summary and chat transports. → [providers](docs/features/providers.md) |
| `diarize.rs` | Engine choice, fallback, model lifecycle (`status` / `download` / `delete`), `remove_retired_models`. → [diarization and echo](docs/features/diarization-and-echo.md) |
| `echo.rs` | The echo pass (`analyze_take`, `TakeAnalysis::check`, `check_joined`), pure |
| `deep_link.rs` | `humla://` links: `parse`, `decide`, `title_from_name`, `open`. → [integrations](docs/features/integrations.md) |
| `notifications/` | UserNotifications in-process (`notify`, `dismiss`, `install`, `route`, `center.rs`, `permission.rs`). → [notifications](docs/architecture/audio-capture-and-notifications.md#notifications-notifications-85-91) |
| `export_doc/` | Export block model (#18): `parse_markdown` → `render_text` / `render_pdf` (krilla, Hanken Grotesk fonts in `src-tauri/fonts/`, OFL) |
| `presets.rs` / `wav.rs` | Preset prompts with `{LANGUAGE}` / RIFF walking, RMS gate, mono-16k decoder, `concat_pcm16_mono_16k` |

### Sidecars

Two Swift binaries bundled via `bundle.macOS.externalBin`, Developer ID + hardened runtime.

- **`audio-capture/`** — AVFoundation mic + system audio from a Core Audio process tap (ScreenCaptureKit as the fallback, #220), per-source chunk and full WAVs, parent-death watchdog, `--keep-awake` (#180). A writer ignores every `write()` after `close()`. → [audio-capture](docs/architecture/audio-capture-and-notifications.md)
- **`speaker-diarize/`** — FluidAudio (CoreML/ANE), one-shot per pass; subcommands `status`, `download`, `delete`. No entitlements file. → [diarization and echo](docs/features/diarization-and-echo.md)

## macOS specifics

- Bundle id `no.humla.app`; a stable Developer ID signature keeps TCC permissions (Microphone, Screen Recording, System Audio Recording) across rebuilds.
- Entitlements (`src-tauri/entitlements.plist`): mic input, network client, screen capture usage description, no app-sandbox.
- Notifications go through UserNotifications and need the user's permission; buttons and clicks only work in a built `.app`.

## Local data layout

Root: `~/Library/Application Support/no.humla.app/`.

- **DB** — `notes.sqlite` (WAL). Tables include `notes` (`language`, `summary_preset`, `summary_provider`, `expected_speakers`, `folder_id`, `speakers`, `detected_language`, `private`, `shared_with`), `folders`, `settings`, `summary_prompts`, `note_chunks` (`chunker_version`: 1 = blank-line splitting, 2 = turn-packed; `db::notes_with_stale_chunks` drives Settings → Chat's rebuild offer). Speakers are stored delimiter-wrapped (`|Michael|Hege|`), written only by `db::reindex_note`. `detected_language` (#167) is a length-weighted vote of the provider's reported language, written only for notes on `auto`, and resolves `auto` before the summary prompt is built.
- **Settings** — the `settings` table. A value another view may show is written through `writeSetting` (`src/lib/settingsBus.ts`); `useSettings` listens. Notable keys: `transcribe_config`, `language`, `custom_vocabulary`, `summary_model`, `anthropic_model`, `summary_provider`, `summary_prompt`, `default_summary_preset`, `diarize_model` (unset = `nemotron3`), `community1_threshold`, `nemotron_offer` (`pending` / `accepted` / `declined`), `keep_audio`, `transcribe_manually`, `keep_awake` (default on), `silence_rms_threshold`, `local_llm_base_url`, `local_llm_model`, `local_llm_think`, `chat_model`, `embed_base_url`, `embed_model`, `theme`, `palette` (`warm` / `graphite`), `developer_mode`, `mcp_enabled`, `webhook_enabled`, `copilot_model`, `copilot_chat_model`, `custom_base_url`, `custom_model`, `custom_chat_model`. Migration flags: `summary_prompts_migrated`, `migrated_transcribe_config_v3`, `migrated_diarize_engine_v1`, `legacy_streaming_models_purged_v1`, `sortformer_models_purged_v1`. Old `sortformer_*` rows are ignored.
- **Keychain** — service `no.humla.app`; accounts `openai_api_key`, `deepgram_api_key`, `groq_api_key`, …, `webhook_config`, `copilot_credentials`, `custom_api_key`, `cloud_credentials` (email, session token, and a password only for accounts without 2FA). Read via `read_provider_api_key(state, "openai")` or `keychain::read`.
- **Whisper models** — `models/` (e.g. `ggml-large-v3-turbo-q5_0.bin` ~547 MB, `nb-whisper-large-q5_0.bin` ~1.1 GB), downloaded from Hugging Face.
- **Diarization models** — `~/Library/Application Support/FluidAudio/Models/` (path hardcoded in FluidAudio, shared with other FluidAudio apps): `speaker-diarization/` (~22 MB), `nemotron-3-diarization/` (`fast128` under `monolithic/v2/`, ~193 MB).
- **Audio temp** — a `tempfile::TempDir` per capture holding `mic-chunk-NNNN.wav`, `sys-chunk-NNNN.wav`, `mic-full.wav`, `sys-full.wav`; cleaned at the end of the post-stop chain, sequenced after diarize (a parallel cleanup raced FluidAudio's reader).
- **Recordings** — `recordings/<note_id>/<take id>/`: `timeline.jsonl` always (text; drives the reader), `clean.json` once cleaned (#217; clean segments, local only, never synced), `boundaries.json` (speaker changes a model has judged, local only), and only with `keep_audio` on: `playback.m4a` (AAC 32 kbps, levelled by `playback_mix`, #226) plus raw `mic.wav` / `sys.wav` (kept WAV: the echo pass, unify and playback rebuild need exact samples). Older takes keep `playback.wav`; `sessions::playback_file` says which. In sync the M4A rides its own `note_sessions` field, `playback_m4a`, and `notes.audio` stays WAV-only (humla-cloud's `SESSIONS-CONTRACT.md`). A deferred take has audio but no `timeline.jsonl` and `transcribed: false` in `sessions.json`. Settings → Recording → "Delete stored audio…" (`stored_audio_stats` / `delete_stored_audio`) sweeps existing audio, keeps text, and pings the sync observer per note.
- **Diagnostics** — `diagnostics/<note_id>/`: `<engine>-<source>.json`, `<engine>-unify.json`, `timings.json`, `replay-timings.json`, `capture-<session_id>.json`. Synced nowhere; not audio, so "Delete stored audio…" keeps it. `purge_note_assets` owns the list of a note's directories, removes them on a Trash purge and a revocation, and first refuses any id that isn't a plain path segment. `purge_note_assets_if_gone` asks `db::note_exists` (a trashed note passes, so its files stay).

## Build & distribution

| Command | What it does |
|---|---|
| `pnpm dev` | Vite dev server only (frontend) |
| `pnpm mock` | Visual-check harness: real components on mocked IPC in a browser. `?case=<name>`, `?palette=<id>&theme=light\|dark`. Dev-only. → [build and release](docs/architecture/build-and-release.md) |
| `pnpm lint` | ESLint over `src/`, a correctness net (rules-of-hooks). Expect 0 errors / ~6 warnings. `pnpm build` runs `tsc -b` |
| `pnpm tauri dev` | Tauri dev (assumes sidecars built). Each `cargo run` is signed by `scripts/dev-sign.sh` first |
| `./scripts/build-sidecar.sh` | Build + sign `audio-capture` (skips if unchanged) |
| `./scripts/build-diarize.sh` | Build + sign `speaker-diarize` (skips if unchanged) |
| `swift test` | From `speaker-diarize/`: pure logic in `Sources/DiarizeCore` |
| `./scripts/build-mcp.sh` | Build + sign `humla-mcp` into `src-tauri/binaries/` |
| `pnpm icon` | Regenerate the app icon from `src-tauri/icons/source.png` |
| `cargo run --release -p echo-probe -- …` | From `src-tauri/`: echo lag and cancellation measurement CLI (#196) |
| `pnpm tauri build` | Production bundle. Does **not** build sidecars (`beforeBuildCommand` is only `pnpm build`); bundles whatever is in `src-tauri/binaries/` |
| `pnpm dmg` | Builds `audio-capture`, the Metal library and `humla-mcp`, then `pnpm tauri build`. Does **not** run `build-diarize.sh` |
| `pnpm release` | Build + notarise + staple + sign updater payload + tag + push + GitHub release |

DMG output: `src-tauri/target/release/bundle/dmg/`.

## Distribution & signing

- Signed as **Developer ID Application: MICHAEL MEHLUM WILHELMSEN (NBUP88JQ35)** (`bundle.macOS.signingIdentity`); sidecars too, `audio-capture` also with `src-tauri/sidecar.entitlements`.
- Dev builds are signed with the same identity and identifier so Keychain "Always Allow" sticks across rebuilds (`src-tauri/.cargo/config.toml` → `scripts/dev-sign.sh`).
- Notarisation credentials: `.env.notarise` (gitignored), sourced by `scripts/build-dmg.sh`.
- Updater key: private `~/.private_keys/humla-updater.key`; public `plugins.updater.pubkey` in `tauri.conf.json`. **Never change the public key once shipped.**

Detail, verification and failure logs: [build and release](docs/architecture/build-and-release.md).

## Releases

Run `pnpm release` to ship a new version.

**Required for every release:** update `src/content/releases.ts` with an entry matching the new version, a release date, user-facing title, summary, detailed paragraphs, and highlights. Include verified related issues from `michaelwilhelmsen/humla` only, with titles and status. Add or update the optional feature illustration when the change is visible, and check the home list and popup in light and dark mode. Keep previous entries; Home shows the latest three. `pnpm release` blocks publishing when the current version has no complete changelog entry.

**Bump the version in three places** (they must match exactly, or auto-update misbehaves):

1. `package.json` → `"version": "X.Y.Z"`
2. `src-tauri/tauri.conf.json` → `"version": "X.Y.Z"`
3. `src-tauri/Cargo.toml` → `version = "X.Y.Z"`

Semver: bug fix → patch, new feature → minor, breaking schema change → major (rare). The release script resolves artifacts by version and payload, never newest-on-disk; be careful with `SKIP_BUILD=1`. → [build and release](docs/architecture/build-and-release.md#what-pnpm-release-does)

## Agent skills

### Issue tracker

Issues are tracked in GitHub Issues (`michaelwilhelmsen/humla`) via the `gh` CLI; external PRs are not a triage surface. See `docs/agents/issue-tracker.md`.

### Triage labels

Default vocabulary — `needs-triage` / `needs-info` / `ready-for-agent` / `ready-for-human` / `wontfix`, mapping 1:1 to GitHub labels. See `docs/agents/triage-labels.md`.

### User documentation

User-facing guides live at **humla.team/docs**, written in `content/docs/<section>/<page>.md` in the sibling `humla-landing` repo. A change to something a guide describes (a setting's name, a payload, a link) updates that page too. `docs/` here is for developers: ADRs, research and agent notes.

### Cloud sessions

Claude Code cloud sessions run on Linux, readied by `scripts/cloud-session-start.sh`. What builds and tests there, and what still needs a Mac, is in `docs/agents/cloud.md`.

### Domain docs

Single-context: one `CONTEXT.md` + `docs/adr/` at the repo root. See `docs/agents/domain.md`.
