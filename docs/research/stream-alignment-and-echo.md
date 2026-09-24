# Mic vs system audio: alignment and echo

Research, 2026-09-23. Question: when a call plays through the laptop's speakers,
the remote voices reach the mic as an echo — how far apart do the two streams
sit, why, and what should Humla change: align the streams in capture (a), align
only in `build_playback_wav` and the timeline (b), or nothing (c)? And is an
echo-suppressed mic worth it for the diarize input?

**Status: measured, 2026-09-24.** Nothing here changes behaviour yet. What
landed is instrumentation (a `capture_timing` event from the sidecar, persisted
per take) and a standalone tool (`echo-probe`) that measures the lag and
cancels the echo offline. The first real take with `capture_timing` (R1) and a
re-run of the Test audio take settled what both proposals rest on (§6,
*Results*). Both are specified for implementation: #197 aligns the streams in
capture, #196 keeps the echo out of the mic's voices.

Verified against `main` after v0.64.0 and three real takes measured on the
user's M1 Max MacBook Pro. `capture_timing` has now run on a real take (R1).

## TL;DR

- **The streams are misaligned by construction.** Each full WAV and every
  chunk's `start_ms` counts from *that stream's own* first delivered frame, and
  everything that compares the two lines them up by index:
  `build_playback_wav` sums sample by sample from 0, the timeline and the
  transcript sort by `start_ms` across streams, `dedup_mic_against_sys`
  windows by it. The mic engine starts synchronously; ScreenCaptureKit starts
  only after an async `SCShareableContent` query and an async `startCapture`,
  so **sys's index 0 is a later instant than mic's** by a start-up latency that
  varies per take. Pause/resume adds that latency again every cycle, a device
  change subtracts the mic's outage, and any SCK delivery gap shifts everything
  after it.
- **An echo lag measured off the audio is that start offset plus the output and
  acoustic path, and the start offset is most of it.** On R1, `capture_timing`
  put sys's first frame **296 ms** after the mic's; the echo lag was 323 ms, so
  the path is **28 ms**, which is the output latency the HAL reports. The Test
  audio take (153 ms) then carried about 125 ms of start offset, and the
  56-minute call from another Mac 414 ms of lag. **The offset differs per take,
  so a fixed correction is wrong**, and it has to be measured per take.
  Aligning in capture would shrink R1's lag from 323 ms to about 28 ms.
- **The echo breaks "You is earned", and not only in principle.**
  `build_hybrid_labels` numbers the mic's voices over the chunks *before*
  `dedup_mic_against_sys` drops the echoed ones. So every chunk Whisper
  transcribed off the speakers votes for an echo voice. Measured on the test
  take's mic stream: Sortformer finds 4 speakers, Nemotron 5, both
  echo-dominated. Community-1 merges everything into one, so "You" is earned
  by accident, and whatever echo survives dedup is then attributed to the user.
  The rule itself is not the problem: a room with several people is numbered
  as it should be. The problem is that echo becomes voices, which in a hybrid
  meeting reads as people in the room who aren't there.
- **A per-voice check fixes the count; the canceller makes it robust.** Echo
  can only sound while the remote side does, so a mic voice that is almost
  never heard while sys is silent is echo. Dropping such voices left exactly
  the user with Sortformer and Nemotron on both measured takes, on the raw mic
  as well as the cancelled one. The canceller removes 17–21 dB of echo in a
  real room (R1) and leaves the user alone untouched. It is what saves
  Community-1, which merges the user and the echo into one voice no check can
  split, and it keeps echo from taking a diarizer's slots in a crowded hybrid
  meeting (unmeasured).
- **Recommendation (measured, §6):** (a) — align in capture with
  timestamp-driven writers (#197) — plus the per-voice check and
  **reference-based echo cancellation of the mic for the diarize input**
  (#196). (c) is out: R1's streams sit 296 ms apart by index. (b) would put a
  per-take offset map (and a gap map) into every consumer and the sync schema.
  #196 is independent of the alignment fix, and it is the prerequisite for
  moving the mic off Community-1.

## 1. How the two streams are timed today

Code facts, `audio-capture/Sources/audio-capture/main.swift` and
`src-tauri/src/commands.rs`:

| Where | What it does with time |
|---|---|
| `ChunkWriter.write` | `start_ms` = frames this stream has written ÷ 16 — its own t=0 |
| `FullRecordingWriter` | `mic-full.wav` / `sys-full.wav` each start at their own first frame |
| `build_playback_wav` | sums the two from index 0 |
| `serialize_timeline`, `build_pieces_unbridged`, `build_hybrid_labels` | sort mic and sys chunks together by `(start_ms, source)` |
| `dedup_mic_against_sys` | pairs a mic chunk with the sys chunks whose spans overlap its own, give or take 2 s |

Start-up order: `engine.start()` runs synchronously near the top of
`main.swift`, then `Task { await startSystemAudio() }` queries
`SCShareableContent.excludingDesktopWindows` (async), builds the stream, and
awaits `startCapture()`. The first SCK buffer can only arrive after both.

**What moves the offset during a take:**

| Event | Effect on "sys content sits early" | Established? |
|---|---|---|
| Start | + SCK start-up latency | code |
| Pause → resume | + (SCK restart − mic restart) per cycle — `resumeCapture` restarts the engine synchronously and SCK through the same slow path | code |
| Device change | − the mic's outage while it re-taps | code |
| SCK delivers nothing (silence?) | + each gap, from then on | **unknown** — counted now |
| Mic overload / dropped buffers | − each gap | unknown — counted now |
| Different clock domains (e.g. USB mic, Bluetooth output) | drifts linearly | unknown — clock domains reported now |

Echo lag in index space: **lag = start offset Δ + path L**, where L is output
latency + the speaker-to-mic acoustic path + input latency. Only Δ is
Humla's; L is physics and is still there after any alignment fix.

Consequences beyond the echo:

- **Turn order.** With Δ of a few hundred ms, a remote reply that starts
  within Δ of the user finishing sorts *before* the user's last words.
- **Playback.** `playback.wav` holds each remote voice twice, Δ + L apart: a
  slap-back at 157–420 ms. Aligning shrinks the gap to L, which at 60–90 ms is
  still an audible doubling. Only a cancelled mic removes it.
- **Dedup.** Its 2 s tolerance absorbs the offsets measured so far (157–420 ms
  of lag). An offset that accumulates past it (many pauses, recurring SCK
  gaps) starts to push an echo's sys text out of the window, and the echoed
  mic chunk survives into the transcript.

## 2. What the first measurements say

Measured in the "Nemotron diarization som Sortformer-erstatning" session on the
user's Mac:

| | Test take ("Test audio") | 56-min call `d0c612d0` |
|---|---|---|
| Source | raw `mic.wav` + `sys.wav`, keep_audio on | mixed `playback.wav` only (synced from another device) |
| Setup | multi-voice podcast loud through the built-in speakers, user talking now and then | 2-person remote call |
| Echo lag | **157 ms** sample-level (onset: 160 ms, 0.53 vs 0.02 at any other lag) | **420 ms** (onset autocorrelation 0.124 vs a 0.00–0.01 baseline) |
| Polarity | **inverted** (waveform correlation −0.28 at the lag) | — |
| Drift | none — 160 ms in every 30 s window | not yet measured over time |
| Echo level | mic 16–18 dB below sys on loud frames; about as loud as the user (median mic −35 dBFS) | "similar loudness" |
| Lengths | mic 211.292 s, sys 211.220 s: **mic 72 ms longer** | — |
| Mic content | ~154 s echo-only, ~26 s the user | — |
| Diarizing the mic | Community-1: 1 cluster; Sortformer: 4; Nemotron fast128: 5 | 7 labels in the timeline; overlap-aware engines report the echo as a third speaker co-active ~350 of ~450 s |

Reading it:

- ~~Δ ≈ the length difference plus a few ms ≈ 70–90 ms on the test take,
  leaving L ≈ 65–85 ms.~~ **Wrong, as R1 showed** (§6, *Results*): L is about
  28 ms on this Mac, so the test take's Δ was about 125 ms. The length
  difference understates Δ by about 65 ms, because sys delivers ~56 ms past the
  mic's stop and the mic's converter never writes its last ~10 ms.
- No drift and no steps across 211 s of continuous playback says nothing about
  silence: a podcast never stops sending audio. Whether SCK goes quiet when the
  far end does is §6's R4. The 56-minute call has since answered it for a real
  call: its lag is 414.3 ms from minute 1 to minute 53 with no step (§6).

## 3. The echo and "You is earned"

`diarize_and_apply`'s hybrid branch diarizes both streams, then calls
`build_hybrid_labels(&chunks, …)` with the chunk log as captured.
`dedup_mic_against_sys` only runs later, inside `build_pieces_unbridged` and
`serialize_timeline`. So:

1. The echo is in `mic-full.wav`, so the mic diarize sees the remote voices,
   now through a speaker and a room.
2. The mic chunks transcribed *from* the echo are still in the log when the
   numbering runs, and their words reach the echo clusters.
3. `finalise_stream_labels` grants `You` only when `mic_nums.len() == 1`.

With **Sortformer** (which ignores the count hint) or **Nemotron**, the echo
voices get their own clusters. The mic resolves to 4–5 voices, and `You` is
never earned. With **Community-1** plus a note count of 2, the mic hint is
`2 − (sys voices) = 1`. That forces a single cluster, so `You` is earned, and
any echo chunk that survived dedup is then labelled as the user. Unhinted
Community-1 merged everything on the test take too. Either way the rule "only
works" when clustering is too coarse to be right.

Two ways out, not exclusive:

- **Label layer (cheap, partial).** Number the mic's voices over chunks that
  survive `dedup_mic_against_sys`. Echo-only chunks then can't vote. It leaves
  the diarizer's clustering itself disturbed, and partial-echo chunks still
  vote.
- **Signal layer (the real fix).** Take the echo out of the audio the mic
  diarize reads. The system stream *is* the far-end signal, so one
  reference-based canceller removes every remote voice at once, however many
  there are, and needs nothing from the transcript. See §7.

Plain subtraction of the time-aligned sys signal does not get there. The echo
is sys filtered by a speaker, a room and (here) an inverting path, and a
delay-and-gain match removes a few dB at best. It needs an adaptive filter.

## 4. Found on the way: a full-recording WAV could be rewritten after close

`FullRecordingWriter.close()` set `file = nil`, and `write()` reopened the file
with `AVAudioFile(forWriting:)`, which **replaces** it, whenever `file == nil`.
In `shutdown`, the writers close before `scStream.stopCapture()` is awaited. So
an SCK buffer delivered in that window recreated `sys-full.wav` with only the
late buffer in it. The `full_recording` event had already gone out with the
correct duration, and the reader had stopped at `stopped`. The post-stop chain
then copied and diarized the replacement.

**Fixed in #194:** both writers ignore any `write()` after `close()`, and the
sidecar logs the drops to stderr (`capture timing: a sys buffer reached the
full-recording writer after it closed; dropped …`). SCK does deliver in that
window, even with nothing playing, so this was reachable on any stop.
`echo-probe` still compares each WAV's length with the `frames_written` the
sidecar reported, now as the regression check: a buffer the full writer turns
away is never counted, so the two agree exactly on a healthy take.

## 5. What landed (instrumentation only)

**`capture_timing` sidecar event**, emitted once, after the writers close and
just before `stopped` (`StreamTiming.swift`, `main.swift`). Per stream, one
entry per *interval* of continuous delivery (opened by start, resume, or a mic
device change):

- `first_stamp_ms`: host time of the interval's first frame — the tap's
  `AVAudioTime.hostTime`, or SCK's presentation timestamp. `first_arrival_ms`,
  beside it, is when the callback saw that buffer. On the host clock, arrival
  minus stamp is small and positive (about a buffer's worth); a stamp on any
  other clock shows up as a large or negative difference. That checks the
  assumption per stream.
- `start_frame` (where the interval landed in the WAV), `last_end_stamp_ms`,
  `media_ms` (audio delivered), `in_rate`, `out_frames`, `buffers`.
- Continuity: `gaps` / `gap_ms` / `max_gap_ms` and `overlaps` / `overlap_ms` —
  buffers that started more than 5 ms after or before where the previous one
  ended — and `max_jitter_ms`, the largest discontinuity under that threshold.
  `jumps` lists where the first 64 fell, as `[frame, ms]`.
- `frames_written`: what the full WAV holds.
- `devices.input` / `devices.output`: the HAL's latency, safety offset, IO
  buffer and stream latency (frames), nominal rate, transport as a four-char
  code (`bltn`, `blue`, `usb `…) and clock domain. **No names** (#174): a
  device name never enters this event.

All host times are relative to one instant the sidecar takes before starting
either stream. Rust parses the event (`recording::CaptureTiming`), prints one
stderr line — `capture timing: sys starts +73.4ms after mic, +73.6ms by the end
| …` — and writes `diagnostics/<note_id>/capture-<session_id>.json`: the raw
event plus derived `alignment` (start offset, end offset, gaps, drift per
interval, HAL latencies). It is timestamps only, so it is written whatever
`keep_audio` says, and it syncs nowhere. The Rust side is tested against a
fixture of the event's shape. The first real take to emit it was R1 on
2026-09-24, and it matches the fixture (§6, *Results*).

**`echo-probe`** (`src-tauri/crates/echo-probe`): a workspace member nothing
depends on, with zero DSP dependencies. From `src-tauri/`:

```sh
cargo run --release -p echo-probe -- selftest     # known-answer takes; run once per build
cargo run --release -p echo-probe -- session ~/Library/Application\ Support/no.humla.app/recordings/<note>/<session> --out ~/echo-probe/<name>
cargo run --release -p echo-probe -- autocorr <…>/playback.wav      # mix-only takes
cargo run --release -p echo-probe -- delay mic.wav sys.wav --timing capture-<session>.json --csv track.csv
cargo run --release -p echo-probe -- cancel mic.wav sys.wav --out mic-aec.wav --linear-out mic-aec-linear.wav
cargo run --release -p echo-probe -- synth /tmp/fake --lag 157 --polarity -1 --gap 50:12
```

- **Delay:** GCC-PHAT (β 0.85, 150–4000 Hz) over 4.1 s windows, with the peak
  picked on |r| because the path can invert. Parabolic sub-sample refinement.
  The track is reduced to segments separated by steps. Each step is resolved
  to the window that best splits the two levels, pinned down to a fraction of
  a second with 1 s windows, then snapped onto a discontinuity the capture
  recorded when the timing file has one. Drift is a Theil–Sen slope per segment. With `--timing`
  it prints the decomposition — `echo lag = start offset + output/acoustic
  path` — and whether each WAV holds what the sidecar wrote.
- **Autocorr:** for a mix alone. Onset-envelope autocorrelation (the probe the
  Mac session already used), refined to the sample by an averaged power
  cepstrum, which also recovers the echo's sign.
- **Cancel:** reference-based and content-agnostic, in three stages. First,
  bulk delay from the lag track, steps included. Second, a partitioned-block
  frequency-domain adaptive filter (after SpeexDSP's MDF: 256 ms tail,
  leak-based step size, two-path foreground/background so double talk can't
  pull the output filter off). Third, a Wiener residual suppressor. Two passes,
  so the opening seconds are cancelled by a converged filter. It writes the
  suppressed output and the linear one, because the suppressor trades some of
  the user's voice in double talk.
- **Guard:** it refuses to write audio anywhere under `no.humla.app/`, because
  a WAV there would sit outside `keep_audio` and "Delete stored audio".

Synthetic known-answer results (tests and `selftest`; linear, time-invariant
echo, so these bound the code, not a real room). The lag comes out within
0.01 ms of the truth (the tests allow 0.1) with its polarity. A 40 ppm drift
comes out within 3 ppm. A 12 ms gap is located within 0.3 s, and to the sample
once snapped to the recorded jump.

| Take | Echo removed (linear / suppressed) | User alone | User over echo (SDR, linear / suppressed) |
|---|---|---|---|
| 157 ms, inverted, two remote voices | 45 / 46 dB | ±0.00 dB | 44 / 45 dB |
| 420 ms + 12 ms sys gap at 50 s | 44 / 46 dB | ±0.00 dB | 45 / 22 dB |
| soft-clipped speaker (nonlinear) | 15 / 25 dB | ±0.00 dB | 13 / 23 dB |

Speed: 2.3 s for a 120 s take in the cloud container (delay + two-pass
cancel), so about a minute for an hour.

## 6. Measurement plan

On the Mac:

1. `FORCE_SIDECAR_REBUILD=1 ./scripts/build-sidecar.sh`; macOS may re-ask for
   Screen Recording if the build falls back to ad-hoc signing. Then
   `pnpm tauri dev`, which also shows the `capture timing:` and
   `sidecar: full_recording …` stderr lines.
2. Settings → Recording → **keep_audio on**. Leave "Transcribe manually" off.
3. `cargo run --release -p echo-probe -- selftest` (from `src-tauri/`).
4. Record, each 2–3 minutes, with the built-in speakers loud:

   | | Setup | What it settles |
   |---|---|---|
   | R1 | podcast, user talking now and then (the planned take) | Δ vs L; the canceller on real audio |
   | R2 | R1 with a pause of ~10 s in the middle | offset growth per pause/resume |
   | R3 | a real call (Meet/Teams), with stretches where nobody remote talks | SCK gaps during far-end silence |
   | R4 | a video paused for ~20 s mid-take | SCK delivery when the system output is idle |
   | R5 | optional: Bluetooth or USB output, built-in mic | clock domains, a large L |

5. For each, run
   `cargo run --release -p echo-probe -- session <recordings/<note>/<session>> --out ~/echo-probe/R1`.
   It prints streams, the lag track and the capture-timing decomposition, and
   writes `mic-aec.wav`, `mic-aec-linear.wav` and `report.json`.
6. Score the mic diarize on `mic.wav`, `mic-aec-linear.wav` and `mic-aec.wav`
   with Community-1 (auto, and hinted), Sortformer and Nemotron: the voice
   count, whether `You` would be earned, and the attribution metric.
7. `d0c612d0`: run `echo-probe autocorr <…>/playback.wav --csv track.csv`.
   Is 420 ms constant over the 56 minutes (a start offset), sloped (drift), or
   stepped (pauses or gaps)?

What to read off `session`:

- `sys's first frame +X ms after mic's` is **Δ**. `→ echo lag … = start offset
  … + output/acoustic path …` is **L**. It should be the same order across R1,
  R2 and R3 on one machine, while Δ varies.
- `misaligned … by the end` against the start: anything accumulated. R2 should
  show the pause's share, and any step should read `sit on a discontinuity the
  capture recorded`.
- `gap(s)` per stream, and each first buffer's `arrived … after its stamp:
  stamp is host time ✓`. If it says otherwise, the SCK timestamps are on
  another clock, and (a) must be built on arrival times instead.
- `WAV length matches ✓`. A mismatch means §4's fix has regressed.

### Results (2026-09-24)

Run on `main` at `8771a18`, with the sidecar rebuilt from it. R1 was recorded
for this, and the Test audio take (T0 below) was re-run with the same tools.

**Capture timing** (R1: 233 s, a podcast through the built-in speakers, the
user talking now and then):

- The event matches, field for field, the fixture `recording.rs` tests its
  parser against, and it names no device.
- sys's first frame came **+295.6 ms** after the mic's. Each stream's first
  buffer arrived shortly after its stamp: +108.6 ms for the mic (100 ms tap
  buffers) and +113.1 ms for sys (20 ms buffers). Both stamp spans match the
  frames delivered (−0.8 and 0.0 ppm), and the two devices share one clock
  domain. **SCK's presentation timestamp is host time**, so (a) stands.
- There were no gaps, overlaps or steps in either stream. `WAV length matches ✓`
  held for both, so #194's fix holds: one late sys buffer was dropped at stop,
  as designed.
- Devices: the input is `grup` (the private aggregate AVAudioEngine makes when
  input and output are different devices) at +52.7 ms; the output is the
  built-in speakers at +28.0 ms.
- `misaligned +286.2 ms by the end` reads 9.4 ms under the start, although
  the audio shows no change. The cause is the mic's converter: `installMicTap`
  converts in the default prime mode and never sends end-of-stream, so the
  resampler's last ~10 ms (160 frames) is never written, and `end_offset_ms`
  counts written frames. The sys converter holds back only 6 frames.

**The lag and its parts:**

| | R1 | T0 (Test audio) | `d0c612d0` (56 min, other Mac) |
|---|---|---|---|
| Echo lag | 323.26 ms ±0.05, inverted | 153.45 ms ±0.06, inverted | 414.31 ms ±0.18, normal |
| Drift, steps | +0.8 ppm, none | +0.7 ppm, none | +0.4 ppm, none (32 clear windows, minutes 1–53) |
| Start offset Δ | 295.61 ms (`capture_timing`) | ≈ 125 ms (lag − L) | unknown |
| Path L | 27.65 ms | — | — |

- L equals the output latency the HAL reports (28.04 ms). It doesn't equal the
  80.7 ms the HAL reports for output and input together, so the tap's
  `hostTime` evidently already sits at the mic. After (a), the echo lag on
  this Mac should read about 28 ms.
- `d0c612d0` answers step 7. On a real call the lag stayed flat for 53
  minutes, so SCK kept delivering through that call's silences.

**Cancellation** (`echo-probe cancel`, both passes):

| | R1 | T0 |
|---|---|---|
| Echo in the mic, against sys | −21.3 dB | −15.2 dB |
| Echo removed where it dominates (median, linear / suppressed) | 17.2 / 21.1 dB | 20.7 / 28.6 dB |
| The mic alone, level change | −0.0 dB | −0.1 dB |

The real room landed where the soft-clipped synthetic take did (15 / 25 dB).

**Diarizing the mic.** The reference comes from the chunk log: a mic word is
echo when the sys transcript has it within ±2 s of (mic time − lag). R1 has
126 user words and 586 echo words; T0 has 98 and 779. The sys stream has 5
voices (Nemotron on sys, both takes).

| Mic stream, engine | Voices (R1 / T0) | `You` earned (R1 / T0) | Echo inside the voices (R1 / T0) |
|---|---|---|---|
| raw, Community-1 | 1 / 1 | yes / yes | 97% / 98% |
| raw, Sortformer | 3 / 4 | no / no | 91% / 91% |
| raw, Nemotron | 5 / 5 | no / no | 95% / 94% |
| linear, Community-1 | 2 / 1 | no / yes | 69% / 61% |
| linear, Sortformer | 2 / 3 | no / no | 18% / 42% |
| linear, Nemotron | 4 / 3 | no / no | 83% / 56% |
| suppressed, Community-1 | 1 / 1 | yes / yes | 34% / 14% |
| suppressed, Sortformer | 2 / 1 | no / yes | 8% / 4% |
| suppressed, Nemotron | 3 / 2 | no / no | 11% / 12% |

Sortformer and Nemotron find the user as a voice of their own in every
variant: 39–41 s on R1 and 14–19 s on T0. Community-1 merges the user with
the echo. What breaks the count is the echo that is left. On R1's suppressed
mic that is 4 s for Sortformer, and 10 s plus a 1 s fragment for Nemotron.

**The per-voice check.** For each mic voice, take the share of its time when
the lag-shifted sys stream is silent. Divide it by the share of the whole take
when sys is silent. Echo can't sound while sys is silent, so an echo voice
reads near 0. A person talks independently of sys (about 1) or in its pauses
(above 1). Sys activity here came from sys's own energy (frames within 40 dB
of its loud frames, with a 150 ms hang), which doesn't depend on how an engine
pads its segments.

- Echo voices read 0.00–0.36. The user read 0.67–3.55. The low end is T0,
  where sys is silent only 4% of the take and the user mostly talked over it.
- Dropping every voice under 0.5 leaves exactly the user, who then earns
  `You`. That held for Sortformer and Nemotron on the raw, linear and
  suppressed mic of both takes. The one exception is R1's suppressed Nemotron,
  where a 1 s fragment reads 1.96.
- With sys activity taken from the sys diarize segments instead, the result is
  the same with a thinner margin: Nemotron's segments put two linear-mic echo
  voices at 0.51 and 0.52.
- The lag must be the take's own. An error of ±100 ms still works, ±300 ms
  doesn't.
- Community-1's single merged voice can't be split. The check must keep the
  last voice rather than drop it.
- Words the user speaks over the remote side mostly land in an echo voice.
  When that voice is dropped, they move to the nearest remaining voice. That
  is right when the user is alone at the mic, and a guess in a room of people.

A second signal needs no pauses in sys: how much of a voice's energy the linear
canceller removed.

- The user lost 0.9–1.0 dB on R1 and 3.0–3.8 dB on T0, where most of the
  user's speech was double talk. Echo voices lost 5.1–26 dB. This catches R1's
  1 s fragment (5.2 dB).
- It is biased for voices found on a cancelled mic. What the canceller leaves
  behind is where it removed little, so one T0 echo voice read 3.7 dB. It
  therefore only ever adds to the timing check.
- With both rules (echo if the ratio is under 0.5, or at least 4.5 dB was
  removed), every Sortformer and Nemotron run on both takes resolves to the
  user alone.

These thresholds are fitted on two podcast takes from one machine. #196 records
the numbers per voice so they can be revisited on real meetings.

**The plan from here.** R1 answered what (a) and the canceller rest on, so the
rest of the plan changes:

- R2 (pause) and R4 (idle output) test what (a) handles by design. They become
  its acceptance takes.
- R3's question is answered by `d0c612d0`.
- R5 only matters if Bluetooth shows drift.
- The take that would still add information is a **real hybrid meeting**, with
  several people in the room and the remote voices on the speakers.

The scoring scripts (reference, votes, cluster map, the per-voice check) lived
in a session scratchpad and are not in the repo. The tables above are their
output.

## 7. Decision rule, and the proposal

| If the measurements show | Then |
|---|---|
| \|Δ\| < 50 ms on every take, no gaps, no steps | (c) for alignment. Still do the canceller |
| Δ ≥ 50 ms but constant within takes, pauses rare | (a) recommended; (b) acceptable as a stopgap |
| gaps, steps at pause/resume, or drift | (a) — (b) can't express them without a per-take discontinuity map |

R1 is row 2: Δ = 296 ms, constant within the take. R2 would show whether pauses
make it row 3, and (a) is the answer either way.

**(a) Align in capture — timestamp-driven writers.** Each writer places a
buffer at the frame its own timestamp says it belongs at:
`round((stamp − epoch − paused_so_far) × 16 000)`, with `epoch` shared by both
streams and taken before either starts. It zero-fills forward when that lies
ahead of what's written (the leading pad, and any SCK gap), and trims when it
lies more than ~2 ms behind. Pause boundaries are taken once, in host time, and
applied to both streams, so a slow SCK restart becomes silence in sys rather
than a shift. The result: the two WAVs share one clock, and every consumer in
§1 is correct without knowing any of this. No data-model or sync change, and
old takes keep their meaning. Risks: it is the sidecar (macOS-only, the hardest
component to test), and it rests on SCK's timestamps being host time — which
R1 confirmed. Keep the drift out of it unless R5 shows some.

R1 adds two constraints:

- **Place converted audio by frame count, not by each input buffer's stamp.**
  The mic converter holds back ~10 ms in steady state, so a converted buffer
  starts ~10 ms before its input buffer's stamp. Placing each output at that
  stamp would shift the mic 10 ms late, behind a 10 ms hole. Place from the
  interval's first stamp plus the frames written, and re-anchor only at a
  discontinuity `capture_timing` already detects (> 5 ms). Flush the
  converters at close so the tail is written.
- **Make the offsets read WAV positions.** `CaptureTiming::start_offset_ms` and
  echo-probe's decomposition compare stamps only, so after (a) they would still
  read ~296 ms. They must subtract each first interval's `start_frame`, which
  is 0 today and the leading pad afterwards.

**(b), for contrast,** stores Δ (and, for correctness, every gap and resume
edge) per take, then applies it in `build_playback_wav`, `serialize_timeline`,
the transcript sort, the hybrid numbering, dedup, the unify pass, re-diarize
and replay — and the sessions sync contract has to carry it. It is the same fix
in nine places instead of one.

**Keeping the echo out of the mic's voices (#196).** Two parts, in this order:

1. **The per-voice check** drops mic voices that aren't heard while sys is
   silent (§6, *Results*). It is what makes the count right, and it works on
   the raw mic.
2. **Reference-based echo cancellation** in the post-stop chain: `sys-full.wav`
   in, a cleaned mic stream out. The mic diarize reads the suppressed output.
   For Sortformer and Nemotron it left 4–12% echo inside the voices, against
   18–83% for the linear output, with the user's voice the same size in both.
   Later, `build_playback_wav` can use it to remove the doubling. With the
   canceller in, the check also counts the energy it removed per voice.

The live transcript keeps its text-based dedup, since chunks transcribe before
the full streams exist. #196 is independent of (a): it estimates its own bulk
delay per take, steps included. But (a) shrinks that delay to L and removes the
steps, so the two compose. #196 is also the precondition for leaving
Community-1, because an end-to-end engine on an echoed mic counts the echo as
people. That switch is decided in
[ADR-0005](../adr/0005-nemotron-3-is-the-default-diarization-engine.md) and
waits on this (#193).

## 8. Open questions

- ~~Is SCK's presentation timestamp on the host clock?~~ **Yes** (R1). Each
  stream's first buffer arrived about one buffer plus a little after its stamp.
  Both stamp spans match the frames delivered to within a ppm, and the
  decomposition gives a positive path that matches the HAL's output latency.
- Does SCK stop delivering when the system output goes idle? **Not during a
  call**: `d0c612d0` is flat for 53 minutes. With nothing playing at all (R4),
  it is still open. (a) zero-fills either way, and R4 is one of its acceptance
  takes.
- ~~How much of L does the HAL report?~~ **More than L.** The HAL reports
  80.7 ms and the measured path is 27.65 ms, which matches its output latency
  alone. After (a), the canceller's bulk delay on this Mac is about 28 ms.
- ~~Real-room cancellation depth.~~ **17 / 21 dB (R1) and 21 / 29 dB (T0)**,
  median, linear / suppressed. The soft-clipped synthetic case was the right
  guide.
- Hybrid meetings: does the per-voice check hold with several people in the
  room and the remote voices on the speakers, and does the echo crowd the
  diarizer's slots? One real meeting answers both.
