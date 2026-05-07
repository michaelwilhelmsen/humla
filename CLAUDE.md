# Humla — project notes

## What this app is

**Humla** is a personal macOS meeting-notes app inspired by Granola. You take freeform notes during a meeting; in parallel the app records mic + system audio, transcribes it, and produces an AI summary that fuses your notes with the transcript. Personal/small-team, not SaaS — your data, your API keys, local SQLite, no backend.

The name is Norwegian for "bumblebee".

## Core capabilities

- **Hybrid capture (parallel streams)** — mic + macOS system audio recorded simultaneously via a Swift sidecar, kept as **two separate streams end-to-end** (no mixdown). Each gets its own VAD-bounded chunk WAVs, its own full.wav, its own Whisper invocations with its own `prior_context` trail. In-person meetings produce only mic chunks (system stays silent → no chunks emitted) and the diarizer runs on the mic stream so multiple humans in the same room get distinct labels. Remote calls produce both: mic chunks tagged "You" by channel attribution and system chunks diarized for remote-side speakers.
- **Four STT providers** — OpenAI (whisper-1 / gpt-4o-transcribe / mini / diarize), on-device Whisper via Metal, Deepgram (nova-3, nova-2, base), and Groq (whisper-large-v3-turbo). All slot into the `stt::BatchSttAdapter` trait so the dispatch path is provider-agnostic.
- **Per-language routing** — `transcribe_config` (typed JSON, single source of truth) is `{ default: ProviderConfig, per_language: BTreeMap<String, ProviderConfig> }`. Resolution at chunk time: per-note language → per-language override → default. E.g. Norwegian → local NB Whisper, English → Deepgram Nova-3, default → OpenAI whisper-1.
- **Whisper quality preset** — Fast (greedy) / Balanced (beam=3) / Quality (beam=5, low no_speech threshold) for the local provider; bundles sampling strategy + confidence thresholds together so the user picks one knob.
- **Per-note transcription language** — global Settings → Language is the default; each note has its own language chip that overrides for that note.
- **Offline diarization on stop** — `speaker-diarize` Swift sidecar runs after `recording_stop`. Two engines selectable via the `diarize_model` setting: **Community-1** (FluidAudio's `OfflineDiarizerManager` — community-1 segmentation + VBx clustering with PLDA) and **Sortformer** (NVIDIA end-to-end, 4-speaker cap). Branches on which streams produced content: mic-only diarizes `mic_full.wav` and emits `Speaker 1:` / `Speaker 2:`; both streams labels every mic chunk `You:` and runs diarize only on `sys_full.wav`.
- **Speaker rename + colour-coded pills** — each unique speaker gets one of four design-token colours (interactive blue, success green, warning gold, accent red, cycling for 5+). A chip strip above the transcript lets the user click any speaker to rename inline; rename is a regex line-anchored rewrite of the transcript text — no separate metadata table.
- **Two-source summaries** — model gets `[Notater]` (typed notes) and `[Transkripsjon]` (transcript) as separate inputs, with a system prompt that tells it to favour notes for intent and transcript for facts.
- **Per-note presets** — Meeting / 1:1 / Lecture / Interview / Brainstorm / Voice memo, each with its own summary prompt. Custom prompts also supported (rows in `summary_prompts` table, referenced as `custom:<id>`).
- **Custom vocabulary** — per-user list of names and tech terms biasing decoding. Threaded through Whisper-shaped providers as `initial_prompt`, Deepgram as `keyterm` (Nova-3) or `keywords` (other models) query params.
- **Trailing transcript context** — every chunk's transcription receives the last ~150 committed words as `prior_context` (Whisper's `initial_prompt` slot for OpenAI/Local/Groq; Deepgram ignores it because its `keywords` is a per-token boost, not a continuation primer). Single biggest mitigation against silence-driven hallucinations and proper-noun drift.
- **VAD-bounded chunks** — sidecar rotates each chunk at natural speech pauses (min 1.0 s / max 15 s / 500 ms silence trigger) instead of a fixed timer.
- **Reasoning-model temperature handling** — gpt-5.x / o-series reject `temperature`; `openai::summarize` detects via `is_reasoning_model()` and omits.
- **Folders** — flat folder list, per-note assignment, search across titles/bodies/transcripts/folder names with auto-expand on hits.
- **Click-to-edit transcript** — styled view by default with coloured pills + plain text; clicking enters a textarea for edits. Locked while a recording is in flight.

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
   - **Both streams have content** (remote/hybrid): label every mic chunk `You:` (no diarize call) and run diarize on `sys-full.wav` to label system chunks `Speaker N:`.
   `build_labelled_transcript` merges all chunks across sources, sorted by `(start_ms, source)`. Resumed recordings prepend the prior transcript snapshot via `combine_with_snapshot` with `Speaker N:` numbers offset past any in the snapshot. Skips silently when the diarize model isn't downloaded.
8. **Crash recovery** — sidecar stdout EOF detection resets the session and emits an error toast. The audio-capture sidecar polls its PPID every 2 s and self-exits if it sees PID 1 (parent died), so dev-reload zombies clean themselves up.
9. **Summary** is fired manually via `summarize_note`. Reads `note.body` (HTML → plain text) + `note.transcript`, resolves the preset's prompt, appends a language directive, and calls the configured summary provider. Reasoning models (gpt-5.x / o-series) get `temperature` omitted automatically.

## Tech stack

### Frontend (`src/`)

- **React 19** + **TypeScript** + **Vite 6** + **Tauri 2** (`@tauri-apps/api` for `invoke` + event listeners).
- **React Router 7** — note routing (`/note/:id`), settings, home.
- **Zustand** — `useNotesStore` (notes/folders) + `useRecordingStore` (status/errors/diagnostics); backend events bound once via `bindBackendListeners`. Listens for `transcript_replaced`, `summary_ready`, `summary_thinking_delta`, `summary_content_delta`, `summary_status`, `recording_status`, `recording_error`, `recording_diagnostic`, `local_whisper_progress`, `diarize_download_progress`.
- **Tiptap v2** — body editor (StarterKit + Placeholder + Suggestion + BubbleMenu).
- **Transcript view** — styled-by-default with `white-space: pre-wrap` so its rendered height matches the textarea exactly (no per-line margin → no page-jump on click-to-edit). Speaker labels rendered as inline `nd-speaker-pill` chips; rest of line is plain text.
- **`SpeakerLabels` chip strip** — derives unique speaker labels from the transcript on every render; click to inline-rename. Rename rewrites the transcript via line-anchored regex (`/^Speaker N: /gm` → `/^Michael: /gm`).
- **Auto-update** — Tauri updater polls `latest.json` from GitHub releases on launch.
- **react-markdown** + **remark-gfm** — summary + reasoning-trace rendering.
- **Tailwind v4** — `@tailwindcss/vite` plugin; design tokens in `src/styles/globals.css`. Base resets are wrapped in `@layer base` so utility classes can override them via cascade.
- **lucide-react** — icon set.
- **Nothing-design aesthetic** — Space Grotesk + Space Mono, monochrome palette, system-aware dark/light. Custom utilities: `.nd-chip`, `.nd-speaker-pill`, `.nd-action`, `.nd-label`, `.nd-bare`. Speaker pill colours come from `--color-interactive` / `--color-success` / `--color-warning` / `--color-accent`, cycling for 5+ speakers. **`--color-pill` is transparent by design** — use `--color-pill-hover` for surfaces that need a fill (code blocks, hover states).

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
| `commands.rs` | All `#[tauri::command]` fns; recording lifecycle; transcribe fan-out via `stt::BatchSttAdapter`; offline diarize on stop (`diarize_and_apply`); summary; folders; settings; per-provider keychain |
| `db.rs` | SQLite schema, CRUD, settings helpers (`get_setting`, `set_setting`, `delete_setting`); migrations: `migrate_summary_prompts` (legacy single-prompt → table), `migrate_transcribe_config` (v0.23 — collapse legacy flat keys into `transcribe_config` JSON), `migrate_per_language_v4` (v0.24 — wrap bare `ProviderConfig` row into `TranscribeConfig { default, per_language }`) |
| `stt/` | STT adapter abstraction. `adapter.rs` (`BatchSttAdapter` trait + `TranscribeCtx { model, language, bias_terms, prior_context, api_key, base_url }`), `config.rs` (`ProviderConfig` tagged union + `TranscribeConfig` with `resolve(language)`), `openai.rs` / `local.rs` / `deepgram.rs` / `groq.rs` (adapters), `openai_compat.rs` (shared multipart client used by OpenAI + Groq), `keychain.rs` (per-provider slots + cache type) |
| `recording.rs` | `RecordingSession` (child handles, inflight tasks, reader handle, `chunk_log` with per-chunk `source`, separate `mic_full_wav_path` + `sys_full_wav_path`, separate `mic_trail` + `sys_trail`, `transcript_at_start` snapshot for resume); `TranscriptTrail` (rolling 150-word window fed to Whisper as `prior_context`, one per source); `ChunkSource` enum (`Mic` / `Sys`); `Phase` enum (`Idle` / `Starting` / `Recording` / `Paused` / `Stopping` / `Diarizing`) |
| `local_whisper.rs` | On-device Whisper; `SharedContext` (lazy-loaded model, reused across chunks); `prewarm()` fires on `recording_start`; `Preset` enum (Fast/Balanced/Quality) bundling sampling strategy + `no_speech_thold`; `ModelKind` (`Multilingual` / `LanguageSpecific { language }`); registry covers `large-v3-turbo-q5`, `large-v3-q5`, `large-v2-q5`, `medium-q5`, `nb-whisper-large-q5` |
| `openai.rs` | OpenAI HTTP client (`BASE`, `client()`); summary endpoint; `is_reasoning_model()` for temperature handling. Transcription is *not* here — that lives in `stt/openai.rs` (the adapter) and `stt/openai_compat.rs` (the shared multipart client) |
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
  - **Community-1** — `OfflineDiarizerManager` (community-1 segmentation + VBx clustering with PLDA score normalisation). `clusteringThreshold: 0.5` (down from default 0.6) so similar-sounding voices recorded in the same room don't collapse onto one cluster.
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

- **DB** — `~/Library/Application Support/no.humla.app/notes.sqlite` (SQLite, WAL). Schema: `notes` (with `language`, `summary_preset`, `summary_provider`, `expected_speakers`, `folder_id` columns), `folders`, `settings`, `summary_prompts`.
- **Settings** — `settings` table inside the same DB. Notable keys: `transcribe_config` (typed JSON, the source of truth for STT routing — wraps default + per-language overrides), `language`, `custom_vocabulary`, `summary_model`, `summary_provider`, `summary_prompt`, `default_summary_preset`, `diarize_model`, `community1_threshold`, `sortformer_silence_threshold`, `sortformer_pred_threshold`, `keep_audio`, `silence_rms_threshold`, `local_llm_base_url`, `local_llm_model`, `local_llm_think`, `theme`, `developer_mode`. Plus migration flags (`summary_prompts_migrated`, `migrated_transcribe_config_v3`).
- **API keys** — macOS Keychain, service `no.humla.app`, accounts `openai_api_key` / `deepgram_api_key` / `groq_api_key`. Read via `read_provider_api_key(state, "openai")` etc.; cached on `AppState.api_key_cache`. The OpenAI key has a one-shot migration from a pre-Keychain SQLite plaintext row.
- **Local Whisper models** — `~/Library/Application Support/no.humla.app/models/` (e.g. `ggml-large-v3-turbo-q5_0.bin` ~547 MB, `nb-whisper-large-q5_0.bin` ~1.1 GB). Downloaded on demand from HuggingFace.
- **FluidAudio diarization models** — `~/Library/Application Support/FluidAudio/Models/` (community-1 set ~30 MB, sortformer separate). FluidAudio writes to its own Application Support root because the path is hardcoded inside the Swift package.
- **Audio temp** — `tempfile::TempDir` per recording session; cleaned 30 s after stop. Per-source per-chunk WAVs (`mic-chunk-NNNN.wav`, `sys-chunk-NNNN.wav`) and per-source full-recording WAVs (`mic-full.wav`, `sys-full.wav`). Either full WAV may be absent if its source produced no frames. Setting `keep_audio=true` copies the full WAVs to `~/Library/Application Support/no.humla.app/recordings/<note_id>/` before cleanup.

## Build & distribution

| Command | What it does |
|---|---|
| `pnpm dev` | Vite dev server only (frontend) |
| `pnpm tauri dev` | Tauri dev (assumes sidecars already built) |
| `./scripts/build-sidecar.sh` | Build + Developer ID sign the audio-capture Swift sidecar (skips if unchanged) |
| `./scripts/build-diarize.sh` | Build + Developer ID sign the speaker-diarize Swift sidecar (skips if unchanged) |
| `pnpm icon` | Regenerate the macOS app icon from `src-tauri/icons/source.png` |
| `pnpm tauri build` | Production bundle (`.app` + `.dmg`) — calls both sidecar build scripts via `beforeBuildCommand` chain |
| `pnpm dmg` | Wrapper: builds both sidecars, then `pnpm tauri build`; prints final DMG path |
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
