<h1 align="center">Humla</h1>

<p align="center">
  <em>Your meetings, transcribed and summarised on your Mac.</em><br>
  <em>Your audio. Your keys. Your data.</em>
</p>

<p align="center">
  <a href="https://github.com/michaelwilhelmsen/humla/releases/latest">
    <img alt="Humla — personal meeting notes for macOS" src="docs/screenshot.png" width="900">
  </a>
</p>

<p align="center">
  <a href="https://github.com/michaelwilhelmsen/humla/releases/latest"><strong>Download for macOS</strong></a>
  ·
  <a href="#what-it-does">What it does</a>
  ·
  <a href="#privacy">Privacy</a>
  ·
  <a href="#how-it-works">How it works</a>
  ·
  <a href="#build-from-source">Build</a>
</p>

<p align="center">
  <a href="https://github.com/michaelwilhelmsen/humla/releases/latest"><img alt="Latest release" src="https://img.shields.io/github/v/release/michaelwilhelmsen/humla?style=flat-square&color=black"></a>
  <a href="#license"><img alt="MIT" src="https://img.shields.io/badge/license-MIT-black?style=flat-square"></a>
  <img alt="macOS" src="https://img.shields.io/badge/macOS-13%2B-black?style=flat-square">
  <img alt="Apple Silicon" src="https://img.shields.io/badge/Apple%20Silicon-recommended-black?style=flat-square">
</p>

<h1></h1>

## About

**Humla** is a meeting-notes app for macOS, inspired by Granola. You take freeform notes during your meeting; Humla records the audio, transcribes it, separates speakers, and produces a structured summary that combines your notes with what was actually said.

Built around one principle: **your audio and your data stay on your machine** unless you explicitly send them to a provider you control. Everything works locally — recording, transcription, speaker identification, even summarisation if you point it at a local LLM.

The name is Norwegian for *bumblebee* — small, hum, personal.

> [!NOTE]
> Humla is a personal project, not a SaaS. There's no signup, no telemetry, no shared backend. The trade-off: you bring your own API keys (or run fully local), and you maintain it yourself.

## What it does

### Records your meetings, two streams at once

Humla captures your microphone and your computer's audio (Zoom, Meet, Slack huddles, anything) at the same time, kept as two separate streams. That means in a remote call your voice doesn't get mixed with the other person's, so the transcript stays clean and "you said vs. they said" is unambiguous. In an in-person meeting it records your room mic and tags the different voices it hears.

### Transcribes accurately — including in your language

Pick the transcription engine that fits, and you can mix-and-match **per language**:

- **Local Whisper** — runs entirely on your Mac via Apple Silicon's GPU. Free after a one-time download. Multiple multilingual models plus a **Norwegian-tuned model** (NB Whisper Large from Nasjonalbiblioteket).
- **OpenAI** — `whisper-1`, `gpt-4o-transcribe`, `gpt-4o-mini-transcribe`, `gpt-4o-transcribe-diarize`.
- **Deepgram** — Nova-3 / Nova-2, native diarization, very strong on conversational English.
- **Groq** — `whisper-large-v3-turbo` at OpenAI-compatible endpoints — same Whisper quality, ~10× cheaper and faster than OpenAI's hosted Whisper.

Set a **default provider** in Settings, then add **per-language overrides** if you want — e.g. *Norwegian → Local NB Whisper, English → Deepgram, fallback → OpenAI*. Humla picks the right one automatically based on the recording's language.

### Identifies speakers, automatically and offline

When you stop the recording, Humla runs a speaker-identification pass on your Mac (no audio uploaded). It labels each turn with `Speaker 1`, `Speaker 2`, etc. — click any label to rename them ("Speaker 2" → "Wilma") and the change applies across the whole transcript.

Two engines, both free and on-device:
- **Community-1** — robust default, auto-detects how many speakers are in the room.
- **Sortformer** — better at rapid back-and-forth, fixed 4-speaker cap.

### Summarises with both your notes and the transcript

When you click *Summarize*, the model gets your typed notes **and** the transcript as separate inputs, with instructions to favour your notes for intent and the transcript for facts. Pick a preset — Meeting / 1:1 / Lecture / Interview / Brainstorm / Voice memo — or write your own.

The summary can run on:
- **OpenAI** — gpt-5.x reasoning models, gpt-4o, and others.
- **Any OpenAI-compatible local server** — Ollama, LM Studio, llama.cpp, vLLM. Sensitive meetings can stay 100% on-device.

### Stays out of your way

- **Custom vocabulary** — names, jargon, acronyms biased into the transcription so they spell consistently.
- **Per-note language** — each note can override the global language for one-off bilingual calls.
- **Folders + search** — flat folder list with full-text search across titles, bodies, transcripts, and folder names.
- **Click-to-edit transcript** — coloured speaker pills inline; click anywhere to fix a transcription error.
- **Auto-update** — signed and notarised; existing installs detect new releases on launch.
- **System-aware light/dark theme** — Nothing-design aesthetic, Space Grotesk + Space Mono.

## Privacy

The defaults are designed so nothing leaves your machine unless you tell it to.

- **No backend, no telemetry.** Humla doesn't phone home. The only outbound traffic is to the API endpoints you've explicitly configured.
- **Your notes and transcripts** live in a single SQLite database at `~/Library/Application Support/no.humla.app/`.
- **Audio chunks** are written to a per-recording temp directory and deleted ~30 seconds after you stop. Optionally keep them via Settings → Audio retention.
- **API keys** are stored in the macOS **Keychain** (one entry per provider — OpenAI, Deepgram, Groq), not in plaintext on disk.
- **Model downloads** are one-time fetches from HuggingFace; the files live in `~/Library/Application Support/no.humla.app/models/` and `~/Library/Application Support/FluidAudio/Models/`.

If you use only Local Whisper + Community-1 (or Sortformer) + a local LLM for summaries, **no audio or text ever leaves your Mac**.

## Quick start

1. **Download** the latest signed + notarised DMG from the [Releases page](https://github.com/michaelwilhelmsen/humla/releases/latest).
2. **Drag Humla** into Applications and open it. macOS Gatekeeper accepts the build directly because it's notarised.
3. **Grant permissions** on first record: Microphone, and (for capturing system audio) Screen Recording. You'll need to relaunch after granting Screen Recording.
4. **Pick your providers** in Settings → Transcription:
   - Local Whisper alone is great if you don't want any cloud calls — click *Download* on a model (~500 MB–1.1 GB depending on which one).
   - OR add an API key for OpenAI / Deepgram / Groq under Settings → API keys.
5. *Optional*: download a speaker-diarization model under Settings → Transcription → Speaker diarization (~30 MB).
6. *Optional*: point Humla at a local LLM server (Ollama / LM Studio / llama.cpp) under Settings → AI Summary if you want fully on-device summaries.

That's it. Click *Record* to start, *Stop* when you're done, *Summarize* when you want notes.

Humla auto-updates: existing installs detect new releases on launch and prompt to install.

## How it works

```
┌─────────────────────────────────────────────────────────────┐
│ React + Vite frontend                                       │
│  Tiptap editor · Zustand store · Tailwind v4                │
└──────────────────────┬──────────────────────────────────────┘
                       │ Tauri IPC
┌──────────────────────▼──────────────────────────────────────┐
│ Rust backend                                                │
│                                                             │
│  ┌─────────────┐  ┌─────────────────┐  ┌─────────────────┐  │
│  │SQLite       │  │ audio-capture   │  │ speaker-diarize │  │
│  │ notes /     │  │ sidecar (Swift) │  │ sidecar (Swift) │  │
│  │ folders /   │  │ AVAudioEngine + │  │ FluidAudio      │  │
│  │ settings    │  │ ScreenCaptureKit│  │ (CoreML / ANE)  │  │
│  └─────────────┘  └─────────────────┘  └─────────────────┘  │
│                                                             │
│  ┌─────────────────────────────────┐  ┌─────────────────┐   │
│  │ HTTPS clients                   │  │ Local Whisper   │   │
│  │ OpenAI / Deepgram / Groq / HF   │  │ whisper-rs/Metal│   │
│  └─────────────────────────────────┘  └─────────────────┘   │
└─────────────────────────────────────────────────────────────┘
```

In plain language: when you hit Record, a small native Swift helper captures your microphone and your system audio as two separate streams, splits each one into short clips at natural speech pauses, and feeds the clips to whichever transcription engine you picked. Your typed notes are saved continuously alongside the transcript. When you stop, Humla runs speaker identification offline (still no audio leaves your Mac) and labels the transcript. *Summarize* sends your notes + the transcript to your chosen LLM and produces a structured Markdown summary.

For a deep dive into the architecture — module map, data flow, gotchas — see [`CLAUDE.md`](CLAUDE.md).

## Build from source

Requires macOS 13+, Apple Silicon recommended.

Prerequisites:
- Rust toolchain (`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`)
- Node 20+ and `pnpm`
- Xcode command line tools (`xcode-select --install`)
- `cmake` (for `whisper.cpp`)

```bash
git clone https://github.com/michaelwilhelmsen/humla.git
cd humla
pnpm install
./scripts/build-sidecar.sh    # builds the audio-capture Swift sidecar
./scripts/build-diarize.sh    # builds the speaker-diarize Swift sidecar
pnpm tauri dev
```

To build a launchable `.app` bundle locally:

```bash
pnpm tauri build --debug
open src-tauri/target/debug/bundle/macos/Humla.app
```

For the full release pipeline (signed + notarised DMG + auto-updater payload + GitHub release), see `scripts/release.sh` and the credentials it reads from `.env.notarise`. Requires an Apple Developer ID, notary key, and a Tauri updater Ed25519 keypair.

## Project layout

```
humla/
├── src/                        # React frontend (Tiptap + Zustand)
├── src-tauri/                  # Rust backend (Tauri 2)
│   ├── src/
│   │   ├── commands.rs         # Tauri commands, recording lifecycle
│   │   ├── recording.rs        # session state, per-source trails
│   │   ├── stt/                # STT adapter abstraction (OpenAI/Local/Deepgram/Groq)
│   │   ├── diarize.rs          # speaker-diarize sidecar wrapper
│   │   ├── local_whisper.rs    # whisper-rs + Metal model registry
│   │   └── openai.rs           # OpenAI HTTP client + summary endpoint
│   └── binaries/               # signed sidecar binaries
├── audio-capture/              # Swift sidecar: mic + screen audio
└── speaker-diarize/            # Swift sidecar: offline diarization
```

## Tech stack

- **Frontend** — React 19 + Vite 6 + Tailwind v4 + Tiptap + Zustand + react-markdown + lucide-react
- **App shell** — Tauri 2, Rust 1.85, reqwest (rustls-tls), rusqlite (bundled), tokio
- **Local Whisper** — `whisper-rs` 0.16 (binds `whisper.cpp`) with the `metal` feature; `large-v3-turbo-q5` default plus alternative multilingual models and NB Whisper Large for Norwegian
- **Speaker diarization** — FluidAudio Swift package; pyannote community-1 + VBx clustering with PLDA, *or* NVIDIA Sortformer; CoreML on Apple Neural Engine
- **Audio capture** — Swift, `AVAudioEngine`, `ScreenCaptureKit`; sandbox-detached via `setsid` so TCC permissions bind to the sidecar binary

## Acknowledgements

Humla stands on the shoulders of:

- [whisper.cpp](https://github.com/ggml-org/whisper.cpp) by Georgi Gerganov — the local transcription engine
- [FluidAudio](https://github.com/FluidInference/FluidAudio) — the offline diarization pipeline (pyannote community-1 + VBx + PLDA, plus Sortformer, ported to CoreML)
- [NB Whisper Large](https://huggingface.co/NbAiLab/nb-whisper-large) by Nasjonalbiblioteket — Norwegian-tuned Whisper model
- [Tauri](https://tauri.app) — the native app shell
- [Tiptap](https://tiptap.dev) — the rich-text editor
- [Granola](https://granola.ai) — the user-experience inspiration

## License

MIT.
