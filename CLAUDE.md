# Humla — project notes

## What this app is

**Humla** is a personal macOS meeting-notes app inspired by Granola. You take freeform notes during a meeting; in parallel, the app records mic + system audio, transcribes the call, and produces a structured AI summary that fuses your notes with the transcript. Built for personal/small-team use, not SaaS — your data, your API keys, local SQLite, no backend.

The name is Norwegian for "bumblebee" (think: small, hum, personal).

## Core capabilities

- **Hybrid capture (parallel streams)** — mic input + macOS system audio recorded simultaneously via a Swift sidecar, kept as **two separate streams end-to-end** (no mixdown). Each gets its own VAD-bounded chunk WAVs, its own full.wav, its own Whisper invocations with its own `initial_prompt` trail context, and its own diarization treatment. In-person meetings produce only mic chunks (system stays silent → no chunks emitted) and the diarizer runs on the mic stream so multiple humans in the same room get distinct labels. Remote calls produce both streams: mic chunks get tagged "You" by channel attribution (no diarize needed — every mic chunk is the same person) and system chunks get diarized for remote-side speakers.
- **Two transcription providers** — pick per-note between OpenAI (Whisper / gpt-4o-transcribe / gpt-4o-mini-transcribe / gpt-4o-transcribe-diarize) or **on-device Whisper** via Metal.
- **Whisper quality preset** — `Fast` (greedy, snappy) / `Balanced` (beam=3) / `Quality` (beam=5, low no_speech threshold) for the local provider; bundles sampling strategy + confidence thresholds together so the user picks one knob.
- **Per-note transcription language** — global Settings → Language is the default for new notes; each note has its own language chip that overrides for that note.
- **Offline speaker diarization on stop** — a second Swift sidecar (`speaker-diarize`, FluidAudio CoreML) runs *after* `recording_stop`. Uses FluidAudio's `OfflineDiarizerManager` (community-1 segmentation + VBx clustering with PLDA) — the upgrade from the 3.1-based streaming `DiarizerManager` we used initially. Branches on which streams produced content: in-person mode (mic-only) diarizes `mic_full.wav` and emits `Speaker 1:` / `Speaker 2:` for the room's voices; remote/hybrid mode (both streams have content) labels every mic chunk `You:` and runs the diarizer only on `sys_full.wav` to separate remote-side speakers. Picked over streaming online ID because the streaming path drifts on long recordings (the failure mode that drove the switch was a 13-min 2-speaker call producing 9 speakers) and because community-1 counts/assigns speakers more accurately on dense single-mic captures (e.g. in-person meetings where everyone shares the same acoustic context).
- **Speaker rename + colour-coded pills** — each unique speaker gets one of four semantic colours from the design tokens (interactive blue, success green, warning gold, accent red, cycling for 5+). A chip strip above the transcript lets the user click any speaker to rename inline; rename is a regex line-anchored rewrite of the transcript text — no separate metadata table.
- **Auto-polish on stop** — every recording goes through an LLM cleanup pass after stop (configured `summary_model`, defaults to `gpt-5.4-mini`). Conservative prompt: only fixes typos / chunk-boundary cuts / missing punctuation; preserves line structure, filler words, and `Speaker N:` labels exactly. Bottom-right toast surfaces the active phase.
- **Two-source summaries** — the model gets `[Notater]` (your typed notes) and `[Transkripsjon]` (the meeting transcript) as separate inputs, with a system prompt that tells it to favour your notes for intent and the transcript for facts.
- **Per-note presets** — Meeting / 1:1 / Lecture / Interview / Brainstorm / Voice memo, each with its own summary prompt. Custom prompts also supported.
- **Custom vocabulary** — a per-user list of names, tech terms, and phrases sent as part of Whisper's `initial_prompt` to bias decoding toward those tokens.
- **Trailing transcript context** — every chunk's transcription receives the last ~150 committed words alongside the custom vocabulary as Whisper's `initial_prompt`, so decoding stays anchored to the conversation rather than treating each chunk as a cold start. Single biggest mitigation against silence-driven hallucinations and proper-noun drift across the meeting.
- **VAD-bounded chunks** — the audio-capture sidecar rotates each chunk at natural speech pauses (min 1.0 s / max 15 s / 500 ms silence trigger) instead of a fixed timer, so chunk boundaries land mid-pause rather than mid-word.
- **Reasoning-model temperature handling** — gpt-5.x and o-series models reject custom temperature; `openai::summarize` detects them via `is_reasoning_model()` and omits the parameter, while keeping `temperature=0.2` for traditional chat models.
- **Folders** — flat folder list, per-note assignment, search across titles/bodies/transcripts/folder names with auto-expand on hits.
- **Click-to-edit transcript** — styled view by default with coloured pills + plain text; clicking enters a textarea for edits. Locked while a recording is in flight to avoid clobber.

## Architecture overview

```
┌─────────────────────────────────────────────────────────────┐
│ React + Vite frontend (src/)                                │
│  Tiptap editor · Zustand store · React Router · Tailwind v4 │
└──────────────────────┬──────────────────────────────────────┘
                       │ Tauri IPC (invoke / events)
┌──────────────────────▼──────────────────────────────────────┐
│ Rust backend (src-tauri/src/)                               │
│  commands.rs · db.rs · recording.rs · openai.rs ·           │
│  diarize.rs · local_whisper.rs · presets.rs · wav.rs        │
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
│  │ OpenAI · HuggingFace (model dl) │  │ whisper-rs/Metal│   │
│  └─────────────────────────────────┘  └─────────────────┘   │
└─────────────────────────────────────────────────────────────┘
```

### Data flow during a recording

1. **`recording_start`** spawns the `audio-capture` sidecar via `setsid` (sandbox-detached so TCC prompts go to *Humla* itself, not Terminal). The diarize sidecar is *not* spawned here — it only runs once, after stop.
2. **Sidecar capture** — `AVAudioEngine` (mic) and `ScreenCaptureKit` (system audio) feed two **independent** writer pairs. Each source has its own `ChunkWriter` that emits VAD-bounded WAV chunks (1.0–15 s; rotates on 500 ms silence once min length reached, hard cap on max) and its own `FullRecordingWriter` that captures the full stream to `mic-full.wav` / `sys-full.wav`. Sidecar emits `{event:"chunk", source:"mic"|"sys", path, start_ms}` events on stdout, plus per-source `{event:"full_recording", source, path, duration_ms}` on shutdown. There is no mixer — the two streams never merge in the sidecar, so per-chunk audio is single-source and Whisper sees clean signal regardless of overlap between the user and the remote side.
3. **Rust reader thread** parses each `chunk` event, appends a `ChunkRecord{source, path, start_ms}` to `RecordingSession.chunk_log`, and spawns `transcribe_chunk(source, …)` on a tokio task tracked in `RecordingSession.inflight`. Concurrent chunks serialise on `transcribe_gate` so each one's `initial_prompt` sees a fresh trail snapshot.
4. **`transcribe_chunk`**:
   1. Read provider config + per-note language (`note.language || global`).
   2. Skip near-silent chunks via `wav::rms` gate.
   3. Acquire `transcribe_gate`. Build initial prompt from custom vocab + the **per-source** `TranscriptTrail` snapshot (`mic_trail` for mic chunks, `sys_trail` for sys chunks — sharing one trail would pull each Whisper invocation toward the wrong stream's vocabulary and cause language drift on bilingual calls).
   4. Call provider (local whisper-rs or OpenAI multipart).
   5. Run `is_likely_hallucination` + `strip_attribution_tail`.
   6. `db::append_transcript(text, separator)` with the raw text — no speaker label yet. Labels are applied after stop, not per chunk.
   7. Push the text into the matching per-source `TranscriptTrail` for the next chunk's prompt context.
   8. Emit `transcript_replaced` with the full new transcript so the UI updates live.
5. **Frontend live update** — `useRecordingStore` listens for `transcript_replaced` and updates the note's transcript in `useNotesStore`. The Note view's transcript card re-derives speaker labels from the text on every render and renders coloured pills inline (only shows up after the post-stop diarize pass adds them; during recording the live transcript is plain text in arrival order).
6. **`recording_stop`** — SIGTERM the audio-capture sidecar → 3 s grace → SIGKILL fallback → drain inflight handles + reader handle.
7. **Offline diarize on stop** — `diarize_and_apply` partitions `chunk_log` by source and branches:
   - **Mic only** (in-person): run the `speaker-diarize` sidecar over `mic-full.wav` (community-1 + VBx, `clusteringThreshold: 0.5`). Each chunk gets `Speaker N:` from its segment via `assign_speaker(start_ms, segments)` with closest-edge fallback.
   - **Sys only** (mic somehow silent): same, but on the system stream.
   - **Both streams have content** (remote/hybrid): label every mic chunk `You:` (no diarize call — every mic chunk is the same person by channel attribution) and run diarize on `sys-full.wav` to label system chunks `Speaker N:`.
   `build_labelled_transcript` then merges all chunks across sources, sorted by `(start_ms, source)`, into a single transcript with newline-separated turns and space-joined same-label continuations. On a resumed recording the prior transcript snapshot gets prepended via `combine_with_snapshot`, with the new session's `Speaker N:` numbers offset past any in the snapshot (the `You:` label is fixed and isn't offset). Bottom-right toast shows `Diarizing…` while this runs. Skips silently when the model isn't downloaded.
8. **Auto-polish (background task)** — after diarization, spawn `polish_transcript` which fetches the configured `summary_model`, sends transcript + notes + custom vocab to chat completions with a strict "preserve line structure and speaker labels" prompt, and replaces the transcript with the polished version. Bottom-right toast shows `Polishing…` while it runs. Settles to `Phase::Idle` when done.
9. **Crash recovery** — sidecar stdout EOF detection resets the session and emits an error toast. The audio-capture sidecar polls its PPID every 2 s and self-exits if it sees PID 1 (parent died), so dev-reload zombies clean themselves up.
10. **Summary** is fired manually via `summarize_note`, which reads `note.body` (HTML → plain text) + `note.transcript`, resolves the preset's prompt, appends a language directive, and calls the configured provider. Same provider/key/model as polish; reasoning models (gpt-5.x / o-series) get `temperature` omitted automatically.

## Tech stack

### Frontend (`src/`)

- **React 19** + **TypeScript** + **Vite 6**
- **Tauri 2** — `@tauri-apps/api` for `invoke` + event listeners; webview-based UI
- **React Router 7** — note routing (`/note/:id`), settings, home
- **Zustand** — `useNotesStore` (notes/folders) + `useRecordingStore` (status/errors/diagnostics); backend events bound once via `bindBackendListeners`. Listens for `transcript_replaced`, `summary_ready`, `recording_status`, `recording_error`, `recording_diagnostic`, `local_whisper_progress`, `diarize_download_progress`.
- **Tiptap v2** — body editor (StarterKit + Placeholder + Suggestion + BubbleMenu).
- **Transcript view** — styled-by-default with `white-space: pre-wrap` so its rendered height matches the textarea exactly (no per-line margin → no page-jump on click-to-edit). Speaker labels rendered as inline `nd-speaker-pill` chips; rest of line is plain text.
- **`SpeakerLabels` chip strip** — derives unique speaker labels from the transcript on every render; one chip per speaker; click to inline-rename. Rename rewrites the transcript via line-anchored regex (`/^Speaker N: /gm` → `/^Michael: /gm`).
- **`PolishToast`** — bottom-right global toast that surfaces `Phase::Diarizing` and `Phase::Polishing` with phase-specific copy.
- **`Updater`** — Tauri auto-update flow; polls `latest.json` from GitHub releases on launch.
- **react-markdown** + **remark-gfm** — summary rendering
- **Tailwind v4** — `@tailwindcss/vite` plugin; design tokens in `src/styles/globals.css`. Base resets are wrapped in `@layer base` so utility classes can override them via cascade — see the v0.3.1 commit for context (the "Install & Restart button is invisible" bug).
- **lucide-react** — icon set
- **Nothing-design aesthetic** — Space Grotesk + Space Mono, monochrome palette, system-aware dark/light. Custom utilities: `.nd-chip`, `.nd-speaker-pill`, `.nd-action`, `.nd-label`, `.nd-bare`. Speaker pill colours come from `--color-interactive` / `--color-success` / `--color-warning` / `--color-accent`, assigned in first-encounter order, cycling for 5+ speakers.

### Backend (`src-tauri/src/`)

- **Rust** + **Tauri 2** runtime (rust-version 1.85 because `fluidaudio-rs` was briefly a candidate dep with that MSRV; the bump survived even though we ended up using a Swift sidecar instead).
- **rusqlite** (`bundled` feature) — single SQLite DB at `~/Library/Application Support/no.humla.app/`. Idempotent ALTER TABLE migrations; index creation runs *after* migrations.
- **reqwest** with `rustls-tls` + `stream` — all HTTPS (OpenAI, Hugging Face for model download).
- **tokio** — async runtime. `spawn_blocking` wraps local Whisper inference. Use `tauri::async_runtime::spawn` (NOT `tokio::spawn`) anywhere that runs from Tauri's `setup` closure — the setup callback runs on the main thread before tokio's runtime is attached, and a bare `tokio::spawn` panics with "no current Tokio runtime", which propagates through the AppKit FFI as `panic_cannot_unwind` and `abort()`s the app on launch (seen in v0.6.1).
- **whisper-rs 0.13** with `metal` feature — bundles whisper.cpp via cmake, runs `large-v3-turbo-q5_0` (~547 MB) on Apple Silicon GPUs.
- **parking_lot** — synchronous mutex for session state. NEVER hold a `parking_lot` guard across an `.await` — the future becomes non-Send and Tauri command futures must be Send. Use `tokio::sync::Mutex` for state that's accessed across await points (e.g. `transcribe_gate`).
- **serde** / **serde_json** — IPC + provider payloads + sidecar JSON streams.
- **chrono** — timestamps.
- **uuid** — note + folder IDs.
- **anyhow** — Rust-side error type; converted to `String` at the IPC boundary.

### Module map

| File | Responsibility |
|---|---|
| `lib.rs` | `AppState`, command registration, plugin setup |
| `main.rs` | Tauri entry |
| `commands.rs` | All `#[tauri::command]` fns; recording lifecycle; transcribe fan-out; offline diarize on stop (`diarize_and_apply`); auto-polish; summary; folders; settings; diarize model lifecycle |
| `db.rs` | SQLite schema, migrations, CRUD for notes/folders/settings. `append_transcript(text, separator)` lets the caller control the join character (space for same-speaker, newline for speaker-change) |
| `recording.rs` | `RecordingSession` (child handles, inflight tasks, reader handle, `chunk_log` with per-chunk `source` tag, separate `mic_full_wav_path` + `sys_full_wav_path`, separate `mic_trail` + `sys_trail`, `transcript_at_start` snapshot for resume); `TranscriptTrail` (rolling 150-word window fed to Whisper as `initial_prompt`, one per source); `ChunkSource` enum (`Mic` / `Sys`); `Phase` enum (`Idle` / `Starting` / `Recording` / `Paused` / `Stopping` / `Diarizing` / `Polishing` / `Summarizing`) |
| `openai.rs` | Transcription + summary HTTP clients; default summary system prompt; `is_reasoning_model()` for temperature handling |
| `local_whisper.rs` | On-device Whisper; `SharedContext` (lazy-loaded model, reused across chunks); `prewarm()` fires on `recording_start`; `Preset` enum (Fast/Balanced/Quality) bundling sampling strategy + `no_speech_thold` |
| `diarize.rs` | Speaker-diarize sidecar wrapper. Two surfaces: one-shot `diarize_file(path)` invoked from `diarize_and_apply` post-stop, and model lifecycle (`status` / `download` / `delete`). All offline — no streaming sidecar |
| `presets.rs` | Backend mirror of frontend preset prompts; `{LANGUAGE}` substitution |
| `wav.rs` | Proper RIFF chunk walking; RMS for silence gate; mono-16k decoder |

### Sidecars

Two Swift Package binaries that run alongside the Tauri main process. Both are bundled via `tauri.conf.json`'s `bundle.macOS.externalBin` and signed with the same Developer ID.

#### `audio-capture/` — recording

- **AVFoundation** for mic, **ScreenCaptureKit** for system audio.
- **Hidden from Dock** via `NSApplication.shared.setActivationPolicy(.prohibited)`.
- Built via `scripts/build-sidecar.sh`. Binary cached via SHA-256 stamp at `src-tauri/binaries/.audio-capture-<triple>.stamp` (override with `FORCE_SIDECAR_REBUILD=1`).
- **Parent-death watchdog** — polls `getppid()` every 2 s; exits if it sees PID 1 (reparented to launchd). Combined with the `setsid` detach in `recording_start`, this prevents zombie sidecars after dev reloads / app crashes.
- Emits these stdout events: `chunk` (with `source: "mic"|"sys"`, `path`, `start_ms`), `full_recording` (with `source`, `path`, `duration_ms`; one per source on shutdown), `stopped`, `paused`, `resumed`, `heartbeat` (frame counts + peaks), `error`.
- Writes parallel `mic-full.wav` + `sys-full.wav` for the entire recording in addition to per-chunk WAVs (filenames prefixed by source so they don't collide in the temp dir). These are the inputs to the offline diarize pass that runs after stop. Either may be absent if its source produced no frames (mic permission denied → no mic-full.wav; in-person meeting with no system audio → no sys-full.wav, since the system writer never opens its file when no system frames arrive).

#### `speaker-diarize/` — offline speaker diarization

- **FluidAudio Swift package** (depends on `FluidInference/FluidAudio`, Apache 2.0). Uses the offline `OfflineDiarizerManager` (community-1 segmentation + VBx clustering with PLDA score normalisation) — the upgrade from the 3.1-based streaming `DiarizerManager` we used initially. `clusteringThreshold: 0.5` (down from the community default 0.6) so similar-sounding voices recorded in the same room don't collapse onto one cluster — the failure mode that drove this tuning was different humans in an in-person meeting being labelled as the same speaker.
- Built via `scripts/build-diarize.sh` — same Developer ID + hardened runtime as audio-capture, but no entitlements file (it just reads a WAV and runs CoreML inference; no mic / screen capture needed).
- Subcommand-style CLI:
  - `speaker-diarize <wav>` — one-shot offline diarization. Loads the offline community-1 models (downloading + compiling on first run), runs `OfflineDiarizerManager.process(url)`, returns a JSON array of `{start_ms, end_ms, speaker_id}` segments and exits.
  - `speaker-diarize status` — checks offline model presence on disk; emits `{downloaded, sizeBytes, path}` JSON.
  - `speaker-diarize download` — fetches + compiles models; streams `{event:"progress", fraction, phase}` updates (phase ∈ `listing` / `downloading` / `compiling`) followed by `{event:"done"}`.
  - `speaker-diarize delete` — wipes the cache directory.
- Lifecycle: short-lived. Spawned by `diarize_and_apply` after `recording_stop`, runs once over `full.wav`, exits. No long-running streaming process, no in-memory speaker state across recordings (clustering is fresh per recording, which is correct since FluidAudio can't unify identities across independent sessions anyway).

## macOS specifics

- **Bundle id** `no.humla.app` — TCC keys on this. Pre-rename `com.notes-app.local` permissions don't transfer.
- **Entitlements** (`src-tauri/entitlements.plist`) — mic input, network client, screen capture usage description, no app-sandbox.
- **TCC pain point** — every rebuild that re-signs the sidecar invalidates the trusted-binary entry. Fix: the stamp-based cache in `build-sidecar.sh`, or graduate to notarised builds (see "Deferred: notarised distribution" below).
- **Tauri webview limitation** — `window.prompt` / `confirm` / `alert` are blocked by the Tauri webview to avoid main-thread deadlock. We use inline input UIs everywhere (folder creation in Sidebar + Note's FolderPicker).

## Local data layout

- **DB** — `~/Library/Application Support/no.humla.app/notes.sqlite` (SQLite, WAL mode). Schema: `notes` (with `language`, `summary_preset`, `folder_id` columns), `folders`, `settings`.
- **Settings** — `settings` table inside the same DB. Keys: `language`, `transcribe_provider`, `transcribe_model`, `whisper_preset`, `custom_vocabulary`, `summary_model`, `summary_prompt`, `theme`, plus opaque secret rows `__openai_api_key__`.
- **Local Whisper model** — `~/Library/Application Support/no.humla.app/models/ggml-large-v3-turbo-q5_0.bin` (~547 MB, downloaded on demand from HuggingFace).
- **FluidAudio diarization models** — `~/Library/Application Support/FluidAudio/Models/speaker-diarization/` (offline community-1 set: `Segmentation.mlmodelc`, `FBank.mlmodelc`, `Embedding.mlmodelc`, `PldaRho.mlmodelc`, `plda-parameters.json`; ~30 MB total, downloaded on demand from HuggingFace + compiled for Apple Neural Engine on first use). FluidAudio writes to its own Application Support root, not the Humla bundle's, because the path is hardcoded inside the Swift package.
- **Audio temp** — `tempfile::TempDir` per recording session; cleaned 30 s after stop. Contains per-source per-chunk WAVs (`mic-chunk-NNNN.wav`, `sys-chunk-NNNN.wav`) and per-source full-recording WAVs (`mic-full.wav`, `sys-full.wav`). Either full WAV may be absent if its source produced no frames.

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

DMG output lands in `src-tauri/target/release/bundle/dmg/`. Currently ad-hoc signed only — see distribution notes below.

## Distribution & signing

Builds are signed with the **Developer ID Application: MICHAEL MEHLUM WILHELMSEN (NBUP88JQ35)** identity (configured in `src-tauri/tauri.conf.json` under `bundle.macOS.signingIdentity`). The Swift sidecar gets the same Developer ID + hardened runtime + `src-tauri/sidecar.entitlements` (mic input).

Stable Developer ID signature means **TCC permissions persist across rebuilds** — Microphone / Screen Recording grants stay valid as long as the cert is the same.

### Notarisation

Notarytool credentials live in `.env.notarise` (gitignored) at the repo root:

```
export APPLE_API_KEY=<10-char Key ID>
export APPLE_API_ISSUER=<Issuer UUID>
export APPLE_API_KEY_PATH=/Users/michaelwilhelmsen/.private_keys/AuthKey_<Key ID>.p8
```

`scripts/build-dmg.sh` sources this file before invoking `pnpm tauri build`. Tauri's bundler detects the env vars and runs `xcrun notarytool submit --wait` + stapler automatically.

If `.env.notarise` is absent, the build is still Developer ID signed but not notarised — first launch needs right-click → Open.

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

1. `package.json` → `"version": "0.1.X"`
2. `src-tauri/tauri.conf.json` → `"version": "0.1.X"`
3. `src-tauri/Cargo.toml` → `version = "0.1.X"`

Convention: semver. Bug fix → patch (`0.1.0` → `0.1.1`). New feature → minor (`0.1.0` → `0.2.0`). Breaking schema change → major (rare for us).

Then:

```
pnpm release
```

The script:
1. Refuses to run if the working tree is dirty or the version isn't bumped beyond the latest GitHub release.
2. Builds the DMG (`pnpm dmg`), which signs + notarises + staples + produces a `.sig` file via the Tauri updater key.
3. Generates `latest.json` with version, signature, and the GitHub download URL.
4. Tags the commit `v<version>`, pushes the tag, creates a GitHub release, uploads `.dmg` + `.sig` + `latest.json` as assets.

All existing Humla installs poll the updater endpoint at startup and prompt to install when a new version lands.

### Updater signing key

Tauri's auto-updater uses a separate Ed25519 keypair from the Apple Developer ID — it signs the **update payload** so the app can verify the DMG hasn't been tampered with before installing.

- **Private key**: `~/.private_keys/humla-updater.key` (passwordless, ~700 perms). Treat with the same care as the notarisation `.p8`. Losing it means you can't ship updates that existing installs will accept — you'd have to publish a new app with a new public key.
- **Public key**: lives in `src-tauri/tauri.conf.json` under `plugins.updater.pubkey`. Bundled into every build. Don't change it once you've shipped or every existing install stops accepting updates.
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
