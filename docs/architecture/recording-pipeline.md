# Recording pipeline

How a capture becomes a transcript, what the stop chain does, and how a deferred take is replayed later. The rules an agent must not break are also listed in `CLAUDE.md`.

## Data flow during a recording

1. **`recording_start`** spawns the `audio-capture` sidecar via `setsid` (detached, so TCC prompts go to Humla, not Terminal). The diarize sidecar is not spawned here; it runs once, after stop.
2. **Sidecar capture** — `AVAudioEngine` (mic) and `ScreenCaptureKit` (system) feed two independent writer pairs. Each source has its own `ChunkWriter` (VAD-bounded WAV chunks, 1.0–15 s, rotating on 500 ms silence) and `FullRecordingWriter` (`mic-full.wav` / `sys-full.wav`). Stdout: `{event:"chunk", source:"mic"|"sys", path, start_ms}`, and per source `{event:"full_recording", source, path, duration_ms}` on shutdown. No mixer.
3. **Rust reader thread** spawns `transcribe_chunk(source, …)` per chunk on a tokio task tracked in `LiveCapture.inflight`, against the capture's **`CaptureSink`** (rolling context, `chunk_log`, and whether `note.transcript` moves as text arrives: `SinkMode::Live` / `Deferred` / `Replay`). Chunks serialise on `transcribe_gate` so each `prior_context` sees a fresh trail. With **Transcribe manually** on (#146) the sink is `Deferred`: the chunk WAV is deleted on arrival and nothing is dispatched.
4. **`transcribe_chunk`**:
   1. Resolve language (`note.language || global`) and provider (`read_transcribe_config(state).resolve(&language)`).
   2. Skip near-silent chunks via the `wav::rms` gate (`silence_rms_threshold`, default 0.005).
   3. Take `transcribe_gate`; build `bias_terms` from custom vocabulary and `prior_context` from the per-source `TranscriptTrail` (`mic_trail` / `sys_trail`, kept apart so bilingual calls don't drift across streams).
   4. Call the provider through `stt::BatchSttAdapter`.
   5. Run `is_likely_hallucination`, `strip_attribution_tail`, repetition-collapse and cross-chunk loop guards.
   6. `db::append_transcript(text, separator)` with raw text; labels come after stop.
   7. Push the text into the matching trail.
   8. Emit `transcript_replaced` with the full transcript.
5. **Frontend** — `useRecordingStore` applies `transcript_replaced` to `useNotesStore`. The live transcript is plain text in arrival order; pills appear once the post-stop diarize adds labels. The tail keeps arriving after stop (#182): drain chunks emit `transcript_replaced` through `Stopping` too.
6. **`recording_stop`** — SIGTERM the sidecar → 3 s grace → SIGKILL → drain inflight and reader handles, *then* snapshot `chunk_log`. `recording_status` carries `pending` / `done` for the drain (one progress bar, *Finishing transcript…*), then the `Diarizing` phase with named steps. The elapsed timer stays, frozen. The backend owns the single capture slot through the whole chain: `recording_start` and `import_audio` refuse on `StopState`. The same indicator renders compact in `Layout`.
7. **Offline diarize on stop** — a deferred take stops here: it writes the take's playback audio, records `transcribed: false` in the manifest, and ends. Otherwise `diarize_and_apply` partitions `chunk_log` by source:
   - **Mic only**: diarize `mic-full.wav`; each chunk gets `Speaker N:` via `assign_speaker(start_ms, segments)` with closest-edge fallback.
   - **Sys only**: the same on the system stream.
   - **Both**: diarize both, then number every voice off one counter walked in `(start_ms, source)` order (`build_hybrid_labels` → `finalise_stream_labels`). `You:` replaces `Speaker 1` only when the mic resolved to exactly one voice; the system side then restarts at 1. A failed stream falls back to one label from `next_free`. The mic hint is `None`; the sys hint is `expected_speakers` minus the mic's voices.
   `build_labelled_transcript` merges chunks sorted by `(start_ms, source)`. Resumed recordings prepend the prior snapshot via `combine_with_snapshot`, offsetting `Speaker N:` numbers.
8. **Crash recovery** — sidecar stdout EOF resets the session and emits an error toast. The sidecar polls its PPID every 2 s and exits on PID 1.
9. **Summary** — manual, via `summarize_note`: `note.body` (HTML → text) + `note.transcript`, the preset's prompt and a language directive.

## Progress reporting

**`recording_status` describes the live capture and nothing else** (#187). It is one global slot that also drives the tray icon and its Start/Stop items, so only the post-stop chain of the capture that just finished writes to it.

**`ChainProgress`** owns the channel, its bracket and its named steps:
- `PostStop` — the global phase.
- `Rediarize` — the per-note `diarize_status` event.
- `Replay` — the per-note `transcribe_status` event.
- `Silent` — nothing.

It is passed in (like `StopClock`), so `diarize_and_apply`, `rediarize_apply_to_chunks` and `unify_note_speakers` never choose the channel. `LabelFallback` rides beside it and only says what a pass does when labels can't be produced.

### Named steps (#189)

One `Step` vocabulary in `recording.rs` for both chains: `transcribing` · `saving_audio` · `removing_echo` · `diarizing` · `writing_playback` · `matching_speakers`. A step gets a name only if it can run long enough to see; sub-100 ms work (`record_detected_language`, `finalize_session`, manifest writes, temp cleanup) gets none.

- **A discrete counter is text, never the track's value.** Every step emits its counter before that unit's work, so a fraction would read 100% with the last unit not started. `StepCounter` renders as ` 1/2` beside the label, returns `None` below two units, and the track stays indeterminate. `discreteCounter` in `RecordingBar.tsx` is the one place this rule lives: it returns the counter text and `value: null` together.
- The track is determinate for exactly two things: the stop's drain (chunks done / pending) and the replay's audio position.
- Payloads are additive and mirrored field for field: `step` / `index` / `count` ride `Diarizing` on `recording_status` and the run's measure on `transcribe_status`; brackets serialize as before.
- The pill shows one counter, the coarsest (take of the run beats stream of the take).
- `stop_steps` / `replay_steps` (test-only) state each chain's sequence and are kept in step with the emitting code by hand. They and the live emitter (`StreamSteps`) derive index and count from `recording::stream_reports`, so only present streams are counted. Never write `1, 2` by hand.
- Timing data: `StopTimings` (`keep_audio_ms`, `diarize_mic_ms` / `diarize_sys_ms`, `playback_assets_ms`, `unify_ms`) and `ReplayTimings` / `ReplayTakeTimings` (per take transcribe, diarize, `playback_ms`; run `unify_ms`). Replay writes one stderr line and `diagnostics/<note_id>/replay-timings.json`.

## Transcribe manually, and re-transcribing (#146)

**Transcribe manually** (Settings → Recording → Audio retention) captures and retains the streams but sends nothing to a provider until the user presses **Transcribe** on the note.

- `keep_audio` is the outer gate. Deferring with no stored audio would lose the meeting, and force-retaining is the exception #24 removed. `sessions::defer_transcription` owns the rule; Settings shows the toggle only once retention is on.
- It applies only to on-device Whisper, resolved through the note's language: a per-language override that routes a note to a cloud provider keeps its live transcript.
- Read once at `recording_start`; changing it mid-meeting leaves the running capture alone.
- Each take has a persisted `transcribed` flag in `sessions.json`, defaulting to **true** so existing takes are not offered for re-transcription. It is independent of the setting's current value. It is a local fact with no server field: `reconcile_manifest` keeps the local value on a cloud pull and treats a newly arriving session as transcribed.
- **Transcribe** replays each pending take's retained audio, oldest first, through the import pipeline (decode → VAD chunk → transcribe → diarize), since each take's timeline is numbered off the takes before it.
- A replay never takes the live-capture slot, and reports on per-note `transcribe_status` (`ChainProgress::Replay`), never `recording_status`.
- Progress is audio position (`ReplayProgress`: each take's duration × streams replayed, monotonic, clamped in Rust), plus `step`. Steps past `transcribing` keep the position and go indeterminate; the sidecar emits no progress and none may be invented. The note's bar and the compact pill both draw it through `CaptureIndicator`; the Transcript panel keeps its shimmer and draws no second copy.
- Replay chunks never stream into `note.transcript`: text arrives once from `rebuild_note_transcript` over every session's timeline, so a run that dies half-way leaves nothing behind ([ADR-0004](../adr/0004-the-timeline-is-canonical-for-a-notes-content.md)), and a take lands at its own position.
- A diarize that can't run is not a failure here (`LabelFallback::Unlabelled`): text lands without `Speaker N:` prefixes.
- A take that transcribed nothing is not marked transcribed; silence and "every chunk failed" look the same, and the second must stay retryable.
- `note_sessions` reports `canTranscribe`: untranscribed **and** `session_has_replayable_audio` (not `session_has_audio`; a take reduced to its playback file can't be replayed).

### Re-transcribe

A `RefreshCw` icon button in the Transcript panel beside Copy.

- `sessions::TranscribeScope` is the whole difference, mirrored by the `"pending" | "all"` union on `ipc.transcribeNote` (change both). `Pending` finishes deferred takes; `All` re-runs every take whose raw streams survive (`canRetranscribe`), including the legacy flat take.
- `takes_to_transcribe` is the single answer both surfaces mirror; it filters on replayable audio under both scopes.
- `All` snapshots a note revision first (`db::set_transcript` keeps no history). `Pending` needs none.
- A take that fails mid-run stops the run without discarding what earlier takes committed; checkpoints and the asset push still happen, then the failure is reported.

### Session metadata is immutable, `duration_ms` included

`cloud-sync`'s `push_session` derives a session's last-write-wins key from its own `started_at`, assuming index / started_at / duration / streams never change. So a deferred take's length comes from the sidecar's `full_recording` event at capture time (`CaptureSink::captured_duration_ms` → `finalize_session`), not a later backfill. `record_transcribed_take` writes the flag and nothing else.
