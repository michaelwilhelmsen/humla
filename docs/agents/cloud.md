# Claude Code cloud sessions

A cloud session is an Ubuntu VM holding a clone of this repo. The SessionStart
hook in `.claude/settings.json` runs `scripts/cloud-session-start.sh`, which does
nothing outside a cloud session and inside one leaves the clone ready to build
and test:

- Tauri's Linux build libraries (webkit2gtk, gtk, …), installed when missing
- the pnpm version `packageManager` pins, and `pnpm install --frozen-lockfile`
- a Rust at least as new as `src-tauri/Cargo.toml`'s `rust-version`
- empty stand-ins for the three `externalBin` sidecars, which `tauri-build`
  refuses to compile without (`src-tauri/binaries/` is gitignored)

It needs no secrets, and the default "Limited" network access covers every
host it reaches.

## What can be checked there

| Check | Command |
|---|---|
| Frontend tests, lint, types + bundle | `pnpm test`, `pnpm lint`, `pnpm build` |
| Every Rust test in the workspace | `cd src-tauri && cargo test --workspace` |
| The local-only build | `cd src-tauri && cargo check -p humla --no-default-features` |
| A component against the mock IPC | `pnpm exec vite --port 1420`, then headless Chrome (`$CHROME_PATH`) on `/mock.html?case=…` |

Start the mock server with `pnpm exec vite` rather than `pnpm mock`: stopping
the `pnpm` wrapper leaves its Vite child holding the port, while a bare `vite`
is one process to stop.

Off macOS, whisper.cpp builds for the CPU instead of Metal, notifications only
ever take the plugin's plain-banner path, and the tests that run
`/usr/bin/afconvert` are compiled out.

## What needs a Mac

- The Swift sidecars: `audio-capture` (AVFoundation, ScreenCaptureKit) and
  `speaker-diarize` (FluidAudio on CoreML), their build scripts and `swift test`.
- Metal and the Neural Engine: on-device Whisper on the GPU, `default.metallib`,
  the diarize models.
- Running the app: `pnpm tauri dev`, the Keychain, permission prompts,
  notifications with buttons, AAC encoding through `afconvert`.
- Signing, notarising and releasing.
- The `#[ignore]` live tests, which use real accounts and spend real requests.

A change that touches any of these is unverified until it has been run on a Mac,
and the summary of the work says so.

## Faster session starts

Without an environment setup script the hook installs the Linux libraries in
every session, about half a minute. A setup script is cached in the VM snapshot,
so putting the same install there makes the hook's check a no-op:

```bash
apt-get update
DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
  pkg-config cmake clang libwebkit2gtk-4.1-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev \
  libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev libssl-dev libxdo-dev
```
