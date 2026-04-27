# Humla

Personal meeting transcription for macOS. Records mic + system audio, transcribes through your choice of provider (OpenAI Whisper, Speechmatics, or on-device Whisper via Metal), and produces structured Markdown summaries with per-note prompt presets.

Built native (Tauri 2, Rust, Swift), keyboard-driven, monochromatic Nothing-design aesthetic.

## Features

- **Three transcription providers** — OpenAI (`whisper-1`, `gpt-4o-transcribe`, `gpt-4o-mini-transcribe`, `gpt-4o-transcribe-diarize`), Speechmatics (with region selector for self-serve and enterprise endpoints), and on-device Whisper large-v3-turbo Q5_0 (~547 MB) via `whisper.cpp` with Metal.
- **Mic + system audio** — Swift sidecar uses `AVAudioEngine` for the microphone and `ScreenCaptureKit` for system audio (the other side of meetings). 16 kHz mono WAV chunks, 20-second rotation.
- **Per-note summary presets** — Meeting (default), 1:1, Lecture, Interview, Brainstorm, Voice memo. Each is a tuned system prompt; you can also write a custom prompt globally. Output language follows the Settings language.
- **Editable transcript** — auto-transcribed text is editable when not recording; your edits survive the next chunk.
- **Pause/resume** — recording pauses without rotating the chunk; nothing fires at OpenAI/Speechmatics until you stop.
- **Hallucination scrubbing** — drops chunks below a silence threshold and trims known Whisper subtitle credits ("Undertekster av Ai-Media", "Subtitles by Amara.org", etc.) from the tail of real speech.
- **Local model management** — download/delete the GGML model from Settings; the active model is reused in-process across chunks.
- **System-aware light/dark theme** — Nothing-inspired palette, Space Grotesk + Space Mono, instrument-panel labels.
- **Slash-command Markdown editor** — Tiptap with H1–H4, lists, quotes, dividers, and Markdown shortcuts (`#` → H1, etc.).

## Stack

- **Frontend** — React 19 + Vite + Tailwind v4 + Tiptap + Zustand + Lucide icons + react-markdown
- **App shell** — Tauri 2, Rust
- **Storage** — SQLite (`rusqlite` bundled)
- **Audio capture sidecar** — Swift + `AVAudioEngine` + `ScreenCaptureKit`, sandbox-detached via `setsid` to inherit TCC permissions cleanly
- **Local Whisper** — `whisper-rs` (binds `whisper.cpp`) with the `metal` feature
- **HTTP** — `reqwest` with `rustls-tls`

## Setup

Prerequisites:
- macOS 13+
- Rust toolchain (`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`)
- Node 20+ and `pnpm`
- Xcode command line tools (for Swift)
- `cmake` (for `whisper.cpp` build)

```bash
brew install pnpm cmake
pnpm install
./scripts/build-sidecar.sh   # compiles audio-capture, signs ad-hoc, strips xattr
pnpm tauri dev
```

To build a launchable `.app` bundle:

```bash
pnpm tauri build --debug
open src-tauri/target/debug/bundle/macos/Humla.app
```

## Permissions

On first record, macOS will prompt for:

- **Microphone** — required to record your voice.
- **Screen Recording** — required to capture system audio. After granting, restart the app for it to take effect.

Both are reflected in Settings → Permissions with live status.

## Configuration

Settings → Transcription:

- **Provider** — OpenAI, Speechmatics, or Local (only enabled once the model is downloaded).
- **Language** — ISO 639-1 (Norwegian, English, Swedish, Danish, Auto).
- **OpenAI model** — choose any of the supported transcribe endpoints.
- **Speechmatics region** — EU1, EU2, US1, US2, AU1. Self-serve keys live on EU1.
- **Operating point** (Speechmatics) — Standard or Enhanced.
- **Local model** — Download / Delete buttons; shows ~547 MB Q5_0 large-v3-turbo.

Settings → Summary:

- **Model** — `gpt-5.4-mini` by default (configurable).
- **Custom prompt** — only used when a note's preset is set to "Custom"; insertable from any preset as a starting point.

## Icon pipeline

```bash
pnpm icon             # uses src-tauri/icons/source.png
pnpm icon path/x.png  # use a different source
```

Crops to non-transparent bounding box, masks to the macOS squircle, pads to 1024×1024, then runs `tauri icon` to regenerate the full icon set.

## Architecture notes

- Recording state lives in a single `RecordingSession` guarded by `parking_lot::Mutex`. The session tracks the sidecar `Child`, the stdout reader `JoinHandle`, and an `Arc<Mutex<Vec<JoinHandle>>>` of in-flight transcribe tasks. `recording_stop` waits for the reader to drain and all transcribes to complete before emitting `Phase::Idle`, so no late chunks land after a stop.
- If the sidecar crashes, the stdout reader detects EOF and clears the session, emitting `recording_status: idle` and a toast. Stale sessions are also self-healed if you click Record again.
- Transcribe chunks are silence-gated by RMS on the WAV `data` chunk (proper RIFF parsing — Apple's `AVAudioFile` writer inserts FLLR padding so the header isn't a fixed 44 bytes).
- Per-note `summary_preset` is resolved server-side via a Rust mirror of the frontend preset list, then a hard "respond in `<language>`" directive is appended so user-customized prompts can't drift.

## License

MIT.
