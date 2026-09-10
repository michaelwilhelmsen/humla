import { useEffect, useRef, useState } from "react";
import { MicOff, Pause, Play, Square } from "lucide-react";
import { ipc, type RecordingPhase, type RecordingStatus, type Step } from "../lib/ipc";
import { useRecordingStore, type ReplayRun } from "../lib/store";
import { cn } from "../lib/cn";

// ~10s of active capture with the mic never rising above the audible floor
// trips the "no audio detected" warning. Active time only — pauses don't count
// (see the store's activeAccumMs / activeSince bookkeeping).
const NO_AUDIO_WARN_MS = 10_000;

// Device names are user-authored and arbitrary, so a pathological one is
// clamped rather than allowed to grow the warning without limit. 40 is measured
// against the narrowest body column the layout allows (`BODY_MIN`, 420px) via
// the harness's `?case=noaudio-long`: at the clamp the warning wraps to two
// lines and still fits, so a longer name buys nothing but a taller pill.
const MAX_DEVICE_NAME = 40;

/**
 * The no-audio warning's copy, which turns on whether the sidecar could name
 * the input device (#174).
 *
 * The warning that motivated this was correct and useless in the same breath:
 * a pair of headphones in another room held the macOS default input, and
 * "check your microphone" misdirected toward the mic hardware in front of the
 * user rather than the device selection somewhere else entirely. Naming the
 * device is the whole fix — with no name we say strictly less, never a guess.
 */
export function noAudioWarning(device?: string | null): string {
  const name = device?.trim();
  if (!name) return "No audio detected — check your microphone";
  // Clamp by code point, not by UTF-16 code unit: people put emoji in device
  // names, and `slice` can cut a surrogate pair in half, which renders as a
  // U+FFFD replacement glyph — a broken character inside a warning reads as a
  // second bug. `trimEnd` so a name clamped mid-space doesn't strand one
  // before the ellipsis.
  const chars = Array.from(name);
  const shown =
    chars.length > MAX_DEVICE_NAME
      ? `${chars.slice(0, MAX_DEVICE_NAME - 1).join("").trimEnd()}…`
      : name;
  return `No audio from ${shown} — check your input device in System Settings`;
}

/**
 * How the controls row gives way when the body column gets narrow.
 *
 * Every pill in the row is `shrink-0 whitespace-nowrap` — each is a fixed-height
 * pill that would overflow rather than grow if its text wrapped — so the row's
 * width is a constant and the column's is not. It therefore degrades in steps,
 * the way `NoteToolbar` already does in the same view, ordered by how little
 * each costs:
 *
 *   1. `detail` — the diagnostics pill's seconds and chunk count. The meters
 *      stay, and the numbers stay reachable in the pill's `title`.
 *   2. `pausedWord` — the word PAUSED. A pause glyph beside a frozen timer
 *      already says it.
 *   3. `pill` — the diagnostics pill entirely.
 *   4. `busyLabel` — "Summarizing…" down to its spinner. The pill keeps its
 *      name (`role="status"` + `aria-label`).
 *   5. `stopTimer` — the frozen elapsed reading, but only once stop has taken
 *      the controls off that pill.
 *   6. `counter` — the progress pill's unit counter: a replay's " in take 2 of
 *      3", or a step's " 1/2". The pill has no step beyond this one — what it
 *      is doing is the point of it.
 *
 * Two arrangements, because the thresholds depend on what else is in the row:
 * `roomy` is diagnostics or controls or the stop's own pills, `tight` adds the
 * busy pill a summary puts there. Every step fires earlier in `tight`, pinned
 * as an ordering test since that direction is the only one that can be a bug.
 *
 * Cost order is what each step is worth, not a promise about the numbers: a
 * step fires where the row needs the width it frees, so `tight` reaches for the
 * diagnostics pill before the word PAUSED, which at that width closes nothing.
 *
 * The thresholds are the CONTAINER'S CONTENT BOX — the body column minus this
 * bar's `px-4` — measured in graphite (the wider theme) against the widest
 * honest content by `scripts/measure-recording-bar.js` over the harness's
 * `?case=recbar-*`. jsdom pins every box to 0, so no unit test can ask this;
 * re-derive them with that script after any change to the row's contents or to
 * a theme's control metrics.
 */
export const ROW_STEPS = {
  // Diagnostics or controls, or the stop's own pills.
  roomy: {
    detail: "@max-[600px]:hidden",
    pausedWord: "@max-[420px]:hidden",
    pill: "@max-[370px]:hidden",
    busyLabel: "",
    stopTimer: "",
    counter: "",
  },
  // The same, with the busy pill a running summary adds.
  tight: {
    detail: "@max-[760px]:hidden",
    pausedWord: "@max-[430px]:hidden",
    pill: "@max-[575px]:hidden",
    // `sr-only`, not `hidden`: the pill is a `role="status"` live region and
    // `display: none` would leave it announcing nothing when a summary starts.
    // Absolutely positioned, so it is out of flow and out of the flex gap.
    busyLabel: "@max-[430px]:sr-only",
    stopTimer: "@max-[500px]:hidden",
    // `sr-only`, and out of flow, for the busy pill's reason. It fires before
    // `busyLabel` because the pill carries the counter in a `title` a sighted
    // user can still reach, which a `role="status"` live region cannot.
    counter: "@max-[490px]:sr-only",
  },
} as const;

/**
 * What each named step of a chain is called. One map, because the stop and the
 * replay draw from one `Step` vocabulary — a user who stops a recording and a
 * user who presses Transcribe are watching the same work. Labels stay short:
 * the row they sit in has only a few pixels of slack.
 */
const STEP_LABELS: Record<Step, string> = {
  transcribing: "Transcribing",
  saving_audio: "Saving audio",
  diarizing: "Identifying speakers",
  writing_playback: "Writing playback",
  matching_speakers: "Matching speakers",
};

/**
 * How a step that counts DISCRETE units is drawn: its position as text beside
 * the label, and an indeterminate track.
 *
 * The track is determinate for exactly two things, both genuinely continuous —
 * the stop's drain (chunks done over chunks pending) and the replay's audio
 * position. A discrete counter is not one of them: each step emits its counter
 * *before* that unit's work, so `index / count` reads 100% with the last unit
 * not started and retreats to 50% at the next step. `(index - 1) / count`
 * leaves the track at zero for the whole first unit, which reads as stuck.
 *
 * Returning both fields together is what keeps that from being re-decided per
 * call site: a caller cannot pair a counter with a fraction.
 *
 * Compact — `1/2`, not "stream 1 of 2" — because what the step IS is the point
 * of the pill. The backend omits `index` / `count` below two units, so the
 * guard is only ever hiding a malformed pair.
 */
function discreteCounter(index?: number, count?: number): { counter: string; value: null } {
  if (!index || !count || count < 2) return { counter: "", value: null };
  return { counter: ` ${Math.min(index, count)}/${count}`, value: null };
}

/** What drawing a stop's progress reads off a `recording_status`. Its own type
    rather than a wide `Pick`: the flat optional fields are the wire format's
    shape, not this function's contract. */
type StopProgress = {
  phase: RecordingPhase;
  /** The drain: transcribes in flight when stop was pressed, and how many have
      landed. */
  pending?: number;
  done?: number;
  /** The capture ran with "Transcribe manually" on. */
  deferred?: boolean;
  /** The named step of the post-stop chain, and its discrete position. */
  step?: Step;
  index?: number;
  count?: number;
};

/**
 * What the bar says once the capture itself is over.
 *
 * `stopping` means the tail of the transcript is still arriving — the chunks
 * that were mid-decode when stop was pressed now append live — so the fraction
 * is a real one and reaches 1 exactly as the last of them lands. A stop with
 * nothing in flight is full immediately rather than starting at zero and
 * jumping.
 *
 * `diarizing` is the phase the whole post-stop chain reports inside, and the
 * step named on it is what the label follows. Its counter is discrete, so the
 * track is indeterminate — see `discreteCounter`. A `diarizing` carrying no
 * step at all keeps the old copy: it is the moment before the chain has named
 * its first step.
 *
 * Null for every other phase — those are the busy pill's, not the bar's. Null
 * too for a `deferred` stop: that capture dispatched nothing and lands on idle
 * in a few hundred milliseconds, so a bar drawn for it appears and vanishes
 * without ever having measured anything.
 */
function captureProgress(
  status: StopProgress,
): { label: string; counter: string; value: number | null } | null {
  if (status.deferred) return null;
  if (status.phase === "stopping") {
    const pending = status.pending ?? 0;
    const done = status.done ?? 0;
    return {
      label: "Finishing transcript",
      counter: "",
      value: pending > 0 ? Math.min(1, done / pending) : 1,
    };
  }
  if (status.phase === "diarizing") {
    return {
      label: status.step ? STEP_LABELS[status.step] : "Identifying speakers",
      ...discreteCounter(status.index, status.count),
    };
  }
  return null;
}

/**
 * What a deferred transcription's replay says while it runs. The fraction is
 * audio position over the whole run, which the backend has already made
 * monotonic — so this side does one division and nothing else.
 *
 * A run whose total is absent or zero is drawn FULL, never empty: the fraction
 * is unknown rather than zero, and an empty track beside a live label reads as
 * stuck. The take counter appears only when there is more than one take, since
 * "take 1 of 1" is a number about nothing.
 *
 * Only the replay itself has a continuous measure. Every step past it counts
 * discrete units and so leaves the track indeterminate — see
 * `discreteCounter`. The run's audio position keeps arriving through those
 * steps, because it is where the next take resumes, which is why `step` and
 * not the fraction decides which the track is.
 *
 * The pill names ONE counter, the coarsest it has: which take of the run beats
 * which stream of the take, since a run's takes are what the user pressed
 * Transcribe on. A single-take run has no take clause, so its steps show their
 * stream counter instead.
 */
function replayProgress(run: ReplayRun): { label: string; counter: string; value: number | null } {
  const takes = run.takes ?? 1;
  const take = takes > 1 ? ` take ${run.take ?? 1} of ${takes}` : "";
  const step = run.step ?? "transcribing";
  if (step !== "transcribing") {
    const stream = discreteCounter(run.index, run.count);
    return {
      label: STEP_LABELS[step],
      counter: take ? ` in${take}` : stream.counter,
      value: stream.value,
    };
  }
  const total = run.totalMs ?? 0;
  return {
    label: STEP_LABELS.transcribing,
    counter: take,
    value: total > 0 ? Math.min(1, (run.doneMs ?? 0) / total) : 1,
  };
}

/** The replay to show when several are in flight: `state.transcribing` is a
    set server-side, so two notes can legitimately replay at once and the
    indicator has one slot. The most recent is the one the user just asked
    for. */
function latestReplay(
  transcribing: Record<string, ReplayRun>,
): { noteId: string; run: ReplayRun } | null {
  let latest: { noteId: string; run: ReplayRun } | null = null;
  for (const [noteId, run] of Object.entries(transcribing)) {
    if (!latest || run.startedAt > latest.run.startedAt) latest = { noteId, run };
  }
  return latest;
}

/** What the indicator is drawing, once the rule below has picked it. */
export type IndicatorState = {
  /** The note the state belongs to — where the compact variant navigates. */
  noteId: string | null;
  /** `progress` draws a labelled track, `live` the elapsed timer, `spinner` a
      phase with no measure of its own. */
  kind: "progress" | "live" | "spinner";
  /** What the pill says, without the trailing ellipsis every one of these
      labels carries — the pill owns that, so the counter can sit between the
      words and it. */
  label: string;
  /** Which unit of how many the step is on — a replay's take clause, or a
      step's stream counter. Split off because the row's narrowest step drops it
      from view. Empty where there is nothing to count. */
  counter: string;
  value: number | null;
  /** `live` only: a paused capture takes the pause glyph over the record dot. */
  paused: boolean;
};

/**
 * What the indicator is about right now, or null when it has nothing to say.
 *
 * One rule for both densities and for `Layout`'s "am I already on that note"
 * check, which has to agree with it — a second copy there would draw a compact
 * pill beside the note view's own bar the moment the two disagreed about a
 * deferred stop.
 *
 * A live capture wins the slot over a replay: the replay is background work on
 * a note the user may not even be looking at. A capture that draws nothing —
 * idle, or the deferred stop above — lets the replay through rather than
 * blanking the row.
 */
export function indicatorState(
  status: RecordingStatus,
  transcribing: Record<string, ReplayRun>,
  scopeNoteId?: string,
): IndicatorState | null {
  const capture = scopeNoteId === undefined || status.noteId === scopeNoteId ? status : null;
  if (capture) {
    const progress = captureProgress(capture);
    if (progress) return { noteId: capture.noteId, kind: "progress", ...progress, paused: false };
    if (capture.phase === "recording" || capture.phase === "paused") {
      return {
        noteId: capture.noteId,
        kind: "live",
        label: "",
        counter: "",
        value: null,
        paused: capture.phase === "paused",
      };
    }
    if (capture.phase === "starting" || capture.phase === "importing") {
      return {
        noteId: capture.noteId,
        kind: "spinner",
        label: capture.phase === "starting" ? "Starting" : "Transcribing audio",
        counter: "",
        value: null,
        paused: false,
      };
    }
  }
  const replay =
    scopeNoteId === undefined
      ? latestReplay(transcribing)
      : transcribing[scopeNoteId]
        ? { noteId: scopeNoteId, run: transcribing[scopeNoteId] }
        : null;
  if (!replay) return null;
  return { noteId: replay.noteId, kind: "progress", ...replayProgress(replay.run), paused: false };
}

/**
 * The capture's elapsed seconds, held by whatever is mounted for the whole
 * recording rather than by the thing that displays it. Frozen rather than
 * cleared through `stopping`: the recording's length is already final when
 * stop is pressed.
 */
export function useCaptureElapsed(phase: RecordingPhase): number {
  const [elapsed, setElapsed] = useState(0);
  useEffect(() => {
    if (phase === "paused" || phase === "stopping") return; // hold the reading
    if (phase !== "recording") {
      setElapsed(0);
      return;
    }
    const start = Date.now() - elapsed * 1000;
    const t = window.setInterval(() => setElapsed(Math.floor((Date.now() - start) / 1000)), 250);
    return () => window.clearInterval(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [phase]);
  return elapsed;
}

/**
 * The one indicator for work that is no longer a live recording, in two
 * densities — the stop's drain, the diarize pass, and a deferred
 * transcription's replay, which runs for minutes on local Whisper and is the
 * longest thing the app does.
 *
 * `bar` is the note view's, standing where the pause/stop controls were: the
 * transcript is arriving into the panel beside it, so the bar's job is to say
 * how much of it is left. `compact` is the app-wide one the sidebar-less
 * placement in `Layout` mounts, so the state follows the user off the note —
 * it adds the elapsed timer while the capture is live and is a link back to
 * the note it belongs to. Both densities carry the same labels and the same
 * track.
 */
export function CaptureIndicator({
  variant,
  noteId,
  elapsed = 0,
  counterStep = "",
  onOpen,
}: {
  variant: "bar" | "compact";
  /** Scopes the `bar` variant to its own note: a capture running elsewhere is
      not this note's business (the compact variant is app-wide and omits it). */
  noteId?: string;
  elapsed?: number;
  /** `ROW_STEPS.counter` for the arrangement the row is in — the `bar`
      variant sits in that row, and which arrangement it is is the row's
      knowledge, not this pill's. The compact variant has a screen edge to
      itself and passes nothing. */
  counterStep?: string;
  onOpen?: () => void;
}) {
  const global = useRecordingStore((s) => s.status);
  const transcribing = useRecordingStore((s) => s.transcribing);
  const state = indicatorState(global, transcribing, noteId);
  const progress = state?.kind === "progress" ? state : null;
  // Named from the whole label, not from the visible text: a step hides the
  // counter at the narrowest widths and it must stay in the name.
  const name = progress ? `${progress.label}${progress.counter}…` : "";
  const live = state?.kind === "live";
  if (variant === "bar") {
    if (!progress) return null;
    return (
      <div
        // Only where a step can hide part of it: the counter survives in the
        // tooltip the way the diagnostics pill's numbers do. A tooltip
        // repeating text that is fully visible is noise.
        title={progress.counter ? name : undefined}
        className="nd-recpill no-drag shrink-0 whitespace-nowrap flex items-center gap-2.5 h-[38px] px-4 rounded-full border border-[var(--color-line-visible)]"
      >
        <span className="shrink-0 whitespace-nowrap text-[13px] font-medium text-[var(--color-text-muted)]">
          {progress.label}
          {progress.counter && <span className={counterStep}>{progress.counter}</span>}
          …
        </span>
        {/* Narrow: the row has to clear a body column allowed down to
            `BODY_MIN`, and every pill in it is `shrink-0`. */}
        <ProgressTrack value={progress.value} label={name} className="w-[56px]" />
      </div>
    );
  }
  // Compact: only ever mounted off the recording's own note, so it says which
  // state the capture is in and nothing about the controls, which are there.
  if (!state) return null;
  return (
    <button
      type="button"
      onClick={onOpen}
      // Named explicitly: the visible content is the state (a timer, or a
      // label the progressbar inside already carries as its own name), and
      // what this control DOES is go to the note.
      aria-label="Open the recording"
      className="no-drag nd-recpill fixed bottom-6 right-6 z-[60] flex items-center gap-2.5 h-[32px] pl-3 pr-3.5 rounded-full border border-[var(--color-line-visible)] text-[12.5px] font-medium text-[var(--color-text-muted)] hover:text-[var(--color-text)] transition-colors"
      title="Open the recording"
    >
      {live ? (
        <>
          {state.paused ? (
            <Pause size={11} strokeWidth={1.8} />
          ) : (
            <span className="rec-dot inline-block w-[8px] h-[8px] rounded-full bg-[var(--color-record)]" />
          )}
          <span className="tabular-nums text-[var(--color-text)]">{formatTime(elapsed)}</span>
        </>
      ) : progress ? (
        <>
          <span className="whitespace-nowrap">{name}</span>
          <ProgressTrack value={progress.value} label={name} className="w-[56px]" />
        </>
      ) : (
        <>
          <span className="w-3 h-3 rounded-full border-2 border-current border-t-transparent animate-spin" />
          <span className="whitespace-nowrap">{state.label}…</span>
        </>
      )}
    </button>
  );
}

/**
 * The track itself. A `null` value is indeterminate in ARIA's own terms — a
 * `progressbar` with no `aria-valuenow` — which is exactly what the diarize
 * step is, so nothing here has to say "no percentage" twice.
 */
function ProgressTrack({
  value,
  label,
  className,
}: {
  value: number | null;
  /** The whole label, as the bar's own name. Not `aria-labelledby` to the
      visible text: a step can hide part of that (#188), and a name that
      shrinks with the column is a name that says less than it knows. */
  label: string;
  className?: string;
}) {
  const pct = value === null ? 100 : Math.round(value * 100);
  return (
    <div
      role="progressbar"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={value === null ? undefined : pct}
      aria-busy={value === null ? true : undefined}
      className={cn("h-1 rounded-full bg-[var(--color-pill-hover)] overflow-hidden", className)}
    >
      <div
        className={cn(
          "h-full rounded-full bg-[var(--color-accent)] transition-[width] duration-200",
          value === null && "nd-progress-indeterminate",
        )}
        style={{ width: `${pct}%` }}
      />
    </div>
  );
}

// Floating recording controls. Record / Summarize live in the note toolbar
// now; this bar surfaces only the in-flight states (starting / recording /
// paused / stopping / diarizing / summarizing). A neutral status pill (mic/sys
// meters + chunk count), a red-outlined timer/controls pill, and — as the
// onboarding safety net — a live mic level meter plus a "no audio detected"
// warning if the mic stays silent for the first ~10s.
export function RecordingBar({ noteId }: { noteId: string }) {
  const status = useRecordingStore((s) => s.status);
  const isThisNote = status.noteId === noteId;
  const phase = isThisNote ? status.phase : "idle";
  const isSummarizing = useRecordingStore((s) => !!s.summarizing[noteId]);
  const transcribing = useRecordingStore((s) => s.transcribing);
  const diag = useRecordingStore((s) => s.diag);
  const showDiag = (phase === "recording" || phase === "paused") && diag && diag.noteId === noteId;

  // --- Live level meter -------------------------------------------------
  // The heartbeat only lands every ~2s and each carries the window's *peak*.
  // Decay the displayed level toward 0 between beats so the meter reads as a
  // continuous VU-style bar rather than a 2s stair-step. Held in refs +
  // rAF-driven local state so it never churns the store.
  const micLevelStore = useRecordingStore((s) => s.micLevel);
  const sysLevelStore = useRecordingStore((s) => s.sysLevel);
  const [micMeter, setMicMeter] = useState(0);
  const [sysMeter, setSysMeter] = useState(0);
  const micRef = useRef(0);
  const sysRef = useRef(0);
  const meterActive = phase === "recording" && !!showDiag;
  useEffect(() => {
    if (!meterActive) {
      micRef.current = 0;
      sysRef.current = 0;
      setMicMeter(0);
      setSysMeter(0);
      return;
    }
    let raf = 0;
    const tick = () => {
      // Attack instantly toward the latest heartbeat peak; decay smoothly.
      const nextMic = Math.max(micLevelStore, micRef.current * 0.9);
      const nextSys = Math.max(sysLevelStore, sysRef.current * 0.9);
      // Only push state when the bar would visibly move — keeps the re-render
      // rate down once the meter has settled toward zero.
      if (Math.abs(nextMic - micRef.current) > 0.0005) setMicMeter(nextMic);
      if (Math.abs(nextSys - sysRef.current) > 0.0005) setSysMeter(nextSys);
      micRef.current = nextMic;
      sysRef.current = nextSys;
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [meterActive, micLevelStore, sysLevelStore]);

  const micActive = meterActive && micMeter > 0.001;
  const sysActive = meterActive && sysMeter > 0.001;

  // --- No-audio warning -------------------------------------------------
  const micHeard = useRecordingStore((s) => s.micHeard);
  const activeSince = useRecordingStore((s) => s.activeSince);
  const activeAccumMs = useRecordingStore((s) => s.activeAccumMs);
  // The device the sidecar says the mic tap is on. Gated on the heartbeat
  // belonging to THIS note, so a recording running on another note can't put
  // its device name in this note's warning.
  const inputDevice = diag && diag.noteId === noteId ? diag.inputDevice : null;
  const [showNoAudio, setShowNoAudio] = useState(false);
  useEffect(() => {
    // Only ever warn while THIS note is actively recording and the mic has
    // never been heard. Paused → activeSince is null → the clock is frozen and
    // we simply don't advance toward the warning.
    if (phase !== "recording" || micHeard) {
      setShowNoAudio(false);
      return;
    }
    const check = () => {
      const active = activeAccumMs + (activeSince !== null ? Date.now() - activeSince : 0);
      setShowNoAudio(active >= NO_AUDIO_WARN_MS);
    };
    check();
    const t = window.setInterval(check, 500);
    return () => window.clearInterval(t);
  }, [phase, micHeard, activeSince, activeAccumMs]);

  const elapsed = useCaptureElapsed(phase);

  async function pause() {
    try { await ipc.recordingPause(); }
    catch (e) { useRecordingStore.getState().pushError({ noteId, message: String(e) }); }
  }
  async function resume() {
    try { await ipc.recordingResume(); }
    catch (e) { useRecordingStore.getState().pushError({ noteId, message: String(e) }); }
  }
  async function stop() {
    try { await ipc.recordingStop(); }
    catch (e) { useRecordingStore.getState().pushError({ noteId, message: String(e) }); }
  }

  const recording = phase === "recording";
  const hasControls = recording || phase === "paused";
  // The timer outlives the controls (#182): stop takes the pause and stop
  // buttons away, and the progress pill stands where they were, but the length
  // of the recording is final and worth keeping on screen. Not for the stop of
  // a capture that deferred its transcription (#146) — that one draws no pill
  // and lands on idle in a few hundred milliseconds, so a timer alone there
  // would flash and vanish, which is the glitch the pill was dropped to avoid.
  const hasTimer = hasControls || (phase === "stopping" && !status.deferred);
  // What the indicator beside the controls is showing, scoped to this note —
  // the stop's own pills, or a replay (#146). Which arrangement the row is in
  // (#177) turns on whether anything shares it with the busy pill a summary
  // puts there.
  const indicator = indicatorState(status, transcribing, noteId);
  const steps =
    isSummarizing && (hasTimer || indicator?.kind === "progress")
      ? ROW_STEPS.tight
      : ROW_STEPS.roomy;
  // What the diagnostics pill says once its numbers are hidden by step 1. The
  // pill is the only place they survive from there, so it carries them whole.
  const readout = diag
    ? `mic ${(diag.micFrames / 16000).toFixed(0)}s · sys ${(diag.sysFrames / 16000).toFixed(0)}s` +
      ` · ${diag.chunks} chunk${diag.chunks === 1 ? "" : "s"}`
    : "";
  // The control pill's inner dividers + button hovers tint red while live,
  // neutral while paused — keeps the red reserved for the active state.
  const ctrlEdge = recording
    ? "border-[color-mix(in_srgb,var(--color-record)_32%,transparent)] hover:bg-[color-mix(in_srgb,var(--color-record)_9%,transparent)]"
    : "border-[var(--color-line-visible)] hover:bg-[var(--color-pill-hover)]";

  return (
    // `inset-x-0` rather than `left-1/2 -translate-x-1/2`: centring by transform
    // left the container shrink-to-fit, and a `shrink-0` child then overflowed
    // it on both sides with nothing able to bound the pill (#174 — naming the
    // device made the warning wider than a 420px `BODY_MIN` body column).
    // Spanning the column and centring with `items-center` gives children a real
    // width to wrap against. `pointer-events-none` keeps the now-full-width
    // container from swallowing clicks meant for the note body beneath it; the
    // pills put it back.
    // `@container` (#177): the row degrades against the BODY COLUMN's width,
    // which is this element's — the window's is the wrong question, since the
    // column narrows as the user drags the context panel wider. Thresholds are
    // this box's CONTENT width, so `px-4` is already outside them.
    <div className="@container absolute bottom-6 inset-x-0 z-30 flex flex-col items-center gap-2.5 px-4 pointer-events-none">
      {showNoAudio && (
        <div
          // Wraps rather than clips: with the device name in it (#174) this copy
          // does not fit one line in a 420px body column, and the actionable
          // half is the tail ("check your input device in System Settings") so
          // truncating it would cost exactly the part worth reading. Hence
          // `min-h` + `py` rather than a fixed `h-[34px]` — one line on a roomy
          // column, two on a narrow one, never overflowing either.
          className="nd-recpill no-drag pointer-events-auto max-w-full flex items-center gap-2 min-h-[34px] py-1.5 px-3.5 rounded-full border text-[12.5px] font-medium text-left"
          style={{
            borderColor: "var(--color-warning)",
            color: "var(--color-warning-text)",
            background: "var(--color-accent-soft)",
          }}
          role="alert"
        >
          <MicOff size={14} strokeWidth={1.8} className="shrink-0" />
          <span>{noAudioWarning(inputDevice)}</span>
        </div>
      )}

      <div className="flex items-center gap-2.5 pointer-events-auto">
      {showDiag && (
        <div
          className={cn(
            "nd-recpill shrink-0 whitespace-nowrap flex items-center gap-[13px] h-[38px] px-4 rounded-full border border-[var(--color-line-visible)] text-[13px] text-[var(--color-text-muted)] tabular-nums",
            steps.pill,
          )}
          title={readout}
        >
          {/* One text run per meter, not three flex items: the seconds hide
              inside the label's own span (leading space included, so the
              compact step reads "mic" and not "mic ") rather than becoming a
              sibling the flex gap would then space differently. */}
          <span className="inline-flex items-center gap-[8px]">
            <Meter level={micMeter} active={micActive} />
            <span>
              mic<span className={steps.detail}> {(diag.micFrames / 16000).toFixed(0)}s</span>
            </span>
          </span>
          <span className="inline-flex items-center gap-[8px]">
            <Meter level={sysMeter} active={sysActive} />
            <span>
              sys<span className={steps.detail}> {(diag.sysFrames / 16000).toFixed(0)}s</span>
            </span>
          </span>
          <span className={cn("text-[var(--color-text-disabled)]", steps.detail)}>
            · {diag.chunks} chunk{diag.chunks === 1 ? "" : "s"}
          </span>
        </div>
      )}

      {/* None of these four phases coexists with the controls pill, so their
          labels never have to give — only `isSummarizing`, which can run over a
          live recording, carries a threshold (`steps.busyLabel`). */}
      {phase === "starting" && <BusyPill label="Starting…" />}
      {phase === "importing" && <BusyPill label="Transcribing audio…" />}
      {isSummarizing && <BusyPill label="Summarizing…" labelClass={steps.busyLabel} />}

      {hasTimer && (
        <div
          className={cn(
            "nd-recpill no-drag shrink-0 whitespace-nowrap flex items-stretch h-[38px] rounded-full overflow-hidden border",
            recording ? "border-[var(--color-record)]" : "border-[var(--color-line-visible)]",
            !hasControls && steps.stopTimer,
          )}
        >
          <div
            className={cn(
              "flex items-center gap-[9px] px-4 text-[15px] font-semibold tabular-nums",
              recording ? "text-[var(--color-record)]" : "text-[var(--color-text-muted)]"
            )}
          >
            {/* No glyph once stop is pressed: a record dot would claim the
                capture is still live and a pause glyph would claim it is
                paused. */}
            {recording ? (
              <span className="rec-dot inline-block w-[9px] h-[9px] rounded-full bg-[var(--color-record)]" />
            ) : phase === "paused" ? (
              <Pause size={12} strokeWidth={1.8} />
            ) : null}
            <span>{formatTime(elapsed)}</span>
            {phase === "paused" && (
              <span
                className={cn(
                  "uppercase tracking-[0.08em] text-[10px] font-medium",
                  steps.pausedWord,
                )}
              >
                Paused
              </span>
            )}
          </div>
          {hasControls && (
          <>
          <button
            onClick={recording ? pause : resume}
            className={cn("no-drag grid place-items-center w-[46px] border-l text-[var(--color-text)] transition-colors", ctrlEdge)}
            title={recording ? "Pause (⌘R)" : "Resume (⌘R)"}
            aria-label={recording ? "Pause" : "Resume"}
          >
            {recording
              ? <Pause size={16} strokeWidth={1.6} />
              : <Play size={16} strokeWidth={1.6} />}
          </button>
          <button
            onClick={stop}
            className={cn("no-drag grid place-items-center w-[46px] border-l text-[var(--color-record)] transition-colors", ctrlEdge)}
            title="Stop"
            aria-label="Stop"
          >
            <Square size={15} fill="currentColor" strokeWidth={0} />
          </button>
          </>
          )}
        </div>
      )}

      {/* Where the pause and stop buttons were. The transcript's tail is
          landing into the panel beside this, so the row's remaining job is to
          say how much of it is still coming. */}
      <CaptureIndicator variant="bar" noteId={noteId} counterStep={steps.counter} />
      </div>
    </div>
  );
}

function formatTime(s: number) {
  const m = Math.floor(s / 60);
  const r = s % 60;
  return `${m}:${r.toString().padStart(2, "0")}`;
}

// `role="status"` + `aria-label` rather than the label text alone: at the
// tightest step (#177) `labelClass` takes the text out of the layout and the
// pill is a bare spinner, which without a name says only that something is
// happening. The text itself stays in the accessibility tree (`sr-only`, not
// `hidden`) so the live region has something to announce when it appears.
function BusyPill({ label, labelClass }: { label: string; labelClass?: string }) {
  return (
    <div
      className="nd-recpill no-drag shrink-0 flex items-center gap-2 h-[38px] px-4 rounded-full border border-[var(--color-line-visible)] text-[13px] font-medium text-[var(--color-text-muted)]"
      role="status"
      aria-label={label}
    >
      <span className="w-3 h-3 rounded-full border-2 border-current border-t-transparent animate-spin" />
      <span className={labelClass}>{label}</span>
    </div>
  );
}

// Compact 4-bar level meter. `level` is the decayed peak (0..~1) from the
// heartbeat; a soft curve (sqrt) lifts quiet-but-present speech so the meter
// isn't pinned near empty at normal talking volume. Bars light left-to-right;
// inactive bars sit dim so the meter's frame is always visible.
const METER_BARS = 4;
const METER_HEIGHTS = [5, 8, 11, 14]; // px, rising left→right

function Meter({ level, active }: { level: number; active: boolean }) {
  const norm = Math.min(1, Math.sqrt(Math.max(0, level)) * 1.6);
  const lit = active ? Math.max(1, Math.round(norm * METER_BARS)) : 0;
  return (
    <span className="inline-flex items-end gap-[2px] h-[14px]" aria-hidden>
      {METER_HEIGHTS.map((h, i) => (
        <span
          key={i}
          className="inline-block w-[3px] rounded-[1px] transition-colors duration-100"
          style={{
            height: `${h}px`,
            background:
              i < lit ? "var(--color-success)" : "color-mix(in srgb, var(--color-text-muted) 28%, transparent)",
          }}
        />
      ))}
    </span>
  );
}
