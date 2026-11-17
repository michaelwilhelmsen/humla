# `audio-capture` sidecar and notifications

## `audio-capture/`

A Swift Package binary bundled via `tauri.conf.json`'s `bundle.macOS.externalBin`, signed with the same Developer ID + hardened runtime as the app plus `src-tauri/sidecar.entitlements` (mic input). Built by `scripts/build-sidecar.sh`, cached via a SHA-256 stamp at `src-tauri/binaries/.audio-capture-<triple>.stamp` (`FORCE_SIDECAR_REBUILD=1` to override).

- AVFoundation for the mic. System audio comes from a Core Audio process tap on macOS 14.2+ (`ProcessTap.swift`, #220), and from ScreenCaptureKit when the tap can't be built, on older macOS, or with `HUMLA_SYS_CAPTURE=sck`. Hidden from the Dock via `NSApplication.shared.setActivationPolicy(.prohibited)`.
- **The process tap** taps every process except the sidecar and its parent into a private aggregate device clocked off the default output, and hands each buffer to `SystemAudioOutput.ingest`, the path an SCK buffer takes. It is rebuilt when the default output or the tap's format changes (AirPods entering call mode), and when it stalls or delivers exact silence while another process is playing (backing off 30 s → 10 min). A tap delivers nothing until some process plays and nothing across a rebuild, so those spans are written as silence from the host clock, which keeps `sys-full.wav` lined up with `mic-full.wav` by index. The tap needs the System Audio Recording permission, asked for in Humla's name: `NSAudioCaptureUsageDescription` is in both `src-tauri/Info.plist` and `audio-capture/Info.plist`.
- **Parent-death watchdog**: polls `getppid()` every 2 s and exits on PID 1. With the `setsid` detach in `recording_start` this prevents zombie sidecars.
- Writes `mic-full.wav` + `sys-full.wav` alongside per-chunk WAVs (prefixed by source). Either may be absent if its source produced no frames.
- Stdout events: `chunk` (`source`, `path`, `start_ms`), `full_recording` (`source`, `path`, `duration_ms`; one per source on shutdown), `stopped`, `paused`, `resumed`, `heartbeat` (frame counts, peaks, `input_device`), `diagnostic` (non-fatal notice, e.g. mic recovered after a device change; carries `input_device`), `capture_timing` (once, before `stopped`), `error`.

### `capture_timing`

Measured stream alignment; nothing reads it yet. Both full WAVs and every `start_ms` count from their own stream's first frame, while SCK starts later than the mic engine. Per stream and per interval of continuous delivery the event carries the first frame's host time (with arrival time as a check), gaps and overlaps beyond 5 ms (first 64), and `frames_written`; plus the HAL latency, transport code and clock domain of the input and output devices — **never a device name** (#174). `StreamTiming.swift` stays Foundation-only. Rust logs one line and writes `diagnostics/<note_id>/capture-<session_id>.json` (timestamps only, synced nowhere). `frames_written` equals what the full WAV holds, which `echo-probe` checks. See `docs/research/stream-alignment-and-echo.md`.

### `input_device` is display-only (#174)

The name of the input device the mic tap is on (engine device, else the system default). It exists so the no-audio warning can name the device. Device names often contain a person's name, so:
- It rides a transient event to a transient warning and is persisted nowhere: not in `write_diagnostics_json`, `chunks.json`, `sessions.json`, or any sync path.
- The `diagnostic` event carries it as its own field, not inside `message` (the reader logs `message` to stderr verbatim); the toast is composed in Rust (`recovery_toast`).
- The three `bad sidecar line` log sites go through `redact_sidecar_line`.
- Clamped for display by `clamp_device_name` (Rust) and `noAudioWarning` (TS), a mirrored pair sharing pinned test vectors.

### Other rules

- **`--keep-awake`** (#180): the sidecar holds an `IOPMAssertionCreateWithName` / `PreventUserIdleSystemSleep` assertion (never display sleep) from when a stream starts capturing until just before `stopped`. Any exit releases it in the OS, so Rust keeps no bookkeeping. Pause keeps it; `--import` ignores it. `sessions::keep_awake` owns the default-on rule.
- **A writer ignores every `write()` after its `close()`.** Shutdown closes writers before awaiting `stopCapture()`, and SCK keeps delivering until that returns; `AVAudioFile(forWriting:)` replaces an existing file, so an ungated late write would leave `sys-full.wav` holding only the late buffers. Drops are counted to stderr.

## Notifications (`notifications/`, #85, #91)

tauri-plugin-notification's permission check always answers granted and its banner is the deprecated `NSUserNotification`, so Humla reads, requests and posts in-process through UserNotifications.

- `notify(app, id, title, body, &[Action])` posts with a button per action; `dismiss(id)` removes it. `install` (in `setup`) turns every click into a `Response { id, action }` emitted as `notification_response` (`RESPONSE_EVENT`); `action` is `None` for a click on the notification itself. A consumer that must hear a launch click listens from `setup`.
- `route`: posted; asked first when macOS has never been asked, then routed again (Don't Allow drops it); dropped when off; outside an `.app` bundle the plugin's plain banner (no buttons, no clicks). `Waiting` lets an earlier `dismiss` cancel a notification still waiting on the prompt.
- Buttons register as a category named by their ids *and* titles (`category`). Registering replaces macOS's whole set, so `Categories` hands it every category used, and a post with buttons waits until macOS holds its category.
- `center.rs` holds every UserNotifications call on a `Center` that exists only inside an `.app` bundle (UserNotifications raises in a bare `tauri dev` binary). **Buttons and clicks can only be checked in a built `.app`.**
- `permission.rs`: macOS's answer as an `Authorization`, whose `label` the Settings rows read. `commands/permissions.rs` exposes it as the `notifications` kind of `permissions_status` / `permissions_request` / `permissions_open_settings`. `menubar::notify` posts through `notify`.
- Offered in onboarding's permissions step (optional, never gating Continue or holding the microphone row) and in Settings → Recording → Permissions.
