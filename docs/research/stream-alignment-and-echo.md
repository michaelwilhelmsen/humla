# Mic vs system audio: alignment and echo

Research, 2026-09-23. Question: when a call plays through the laptop's speakers,
the remote voices reach the mic as an echo — how far apart do the two streams
sit, why, and what should Humla change: align the streams in capture (a), align
only in `build_playback_wav` and the timeline (b), or nothing (c)? And is an
echo-suppressed mic worth it for the diarize input?

**Status: measuring.** Nothing here changes behaviour. What landed is
instrumentation (a `capture_timing` event from the sidecar, persisted per take)
and a standalone tool (`echo-probe`) that measures the lag and cancels the echo
offline. The capture-side proposal waits on the measurements in §6.

Verified against `main` at v0.64.0 and two real takes measured on the user's
Mac. The Swift half of the diagnostics compiles but has not yet run on a real
take — see §5.

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
  acoustic path.** The one real take with raw streams shows a 157 ms lag
  (inverted polarity, no drift over 211 s) with the mic 72 ms longer than sys
  — consistent with roughly 70–90 ms of start offset and 65–85 ms of path, but
  lengths alone can't separate the two. The downloaded 56-minute call showed
  420 ms. **The lag differs per take, so a fixed correction is wrong**, and it
  has to be measured per take. The new `capture_timing` diagnostic separates
  start offset from path directly.
- **The echo breaks "You is earned", and not only in principle.**
  `build_hybrid_labels` numbers the mic's voices over the chunks *before*
  `dedup_mic_against_sys` drops the echoed ones. So every chunk Whisper
  transcribed off the speakers votes for an echo voice. Measured on the test
  take's mic stream: Sortformer finds 4 speakers, Nemotron 5, both
  echo-dominated. Community-1 merges everything into one, so "You" is earned
  by accident, and whatever echo survives dedup is then attributed to the user.
- **Recommendation, provisional on §6:** (a) — align in capture with
  timestamp-driven writers — plus **reference-based echo cancellation of the
  mic for the diarize input**. (c) is already out: the benign take's mic WAV
  is 72 ms longer than its sys WAV although the mic stops first, so the two sit
  at least that far apart by index, and the 56-minute call further. (b) would put a per-take offset map (and a gap map) into every
  consumer and the sync schema. The canceller is independent of the alignment
  fix, and it is the prerequisite for moving the mic off Community-1.

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

- The mic ends at `engine.stop()` while sys keeps delivering until
  `sysFullWriter.close()` a few ms later, so **Δ ≈ the length difference plus a
  few ms ≈ 70–90 ms** on the test take, leaving **L ≈ 65–85 ms**. L is
  plausible for an Apple-silicon MacBook, whose speaker and mic paths both carry
  DSP. The 56-minute call, recorded on a different machine, would then carry
  roughly 300+ ms of start offset. `capture_timing` replaces this inference with
  a measurement.
- No drift and no steps across 211 s of continuous playback says nothing about
  silence: a podcast never stops sending audio. Whether SCK goes quiet when the
  far end does is §6's R4.

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
fixture of the event's shape. The Swift side compiles clean on the Mac, but no
real take has emitted the event yet: §6 step 1 is its first run.

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

## 7. Decision rule, and the proposal

| If the measurements show | Then |
|---|---|
| \|Δ\| < 50 ms on every take, no gaps, no steps | (c) for alignment. Still do the canceller |
| Δ ≥ 50 ms but constant within takes, pauses rare | (a) recommended; (b) acceptable as a stopgap |
| gaps, steps at pause/resume, or drift | (a) — (b) can't express them without a per-take discontinuity map |

Every datum so far points at row 2 or 3.

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
component to test), and it rests on SCK's timestamps being host time — which is
exactly what `capture_timing` verifies first. Keep the drift out of it unless
R5 shows some.

**(b), for contrast,** stores Δ (and, for correctness, every gap and resume
edge) per take, then applies it in `build_playback_wav`, `serialize_timeline`,
the transcript sort, the hybrid numbering, dedup, the unify pass, re-diarize
and replay — and the sessions sync contract has to carry it. It is the same fix
in nine places instead of one.

**The canceller, for the diarize input.** Reference-based echo cancellation
belongs in the post-stop chain: `sys-full.wav` in, a cleaned mic stream out,
fed to the mic diarize (and later to `build_playback_wav`, which removes the
doubling). The live transcript keeps its text-based dedup, since chunks
transcribe before the full streams exist. It is independent of (a): the
canceller estimates its own bulk delay per take, steps included. But (a) shrinks
that delay to L and removes the steps, so the two compose. Whether it is worth
it is decided by §6 step 6. The prior is strong: on the test take, 154 s of the
mic's 180 s of content is echo, and every engine that can count voices counts
it. It is also the precondition for leaving Community-1: an end-to-end engine
on an echoed mic stream will never earn `You`.

## 8. Open questions

- Is SCK's presentation timestamp on the host clock? The design of (a) depends
  on it; `capture_timing` answers it on the first take.
- Does SCK stop delivering when the system output goes idle (R3, R4)? If it
  does, sys loses wall-clock time at every silence today, and zero-filling in
  (a) is essential rather than tidy.
- How much of L does the HAL report? A large unexplained remainder means DSP in
  the speaker or mic path. Harmless for (a) — L is real — but it sets the
  canceller's bulk delay after alignment.
- Real-room cancellation depth. Synthetic takes bound the code; a MacBook
  speaker at volume is nonlinear, and the soft-clipped case (15 / 25 dB) is the
  better guide.
