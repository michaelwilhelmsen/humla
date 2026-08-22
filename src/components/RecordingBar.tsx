import { useEffect, useRef, useState } from "react";
import { MicOff, Pause, Play, Square } from "lucide-react";
import { ipc } from "../lib/ipc";
import { useRecordingStore } from "../lib/store";
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
 * How the controls row gives way when the body column gets narrow (#177).
 *
 * Every pill in the row is `shrink-0 whitespace-nowrap` — correct individually,
 * since each is a fixed-height pill that would overflow rather than grow if its
 * text wrapped — so the row's width is a constant and the column's is not. The
 * row measured 442px (warm) / 457px (graphite) against a body column allowed
 * down to `BODY_MIN` (420) and sitting at 414 in the shipped default window, so
 * it overhung: under the context panel on the right, over the nav card on the
 * left, taking part of the stop button with it.
 *
 * So it degrades, the way `NoteToolbar` already does in the same view — and for
 * the same reason. Four steps, ordered by how little each costs:
 *
 *   1. `detail` — the diagnostics pill's seconds and chunk count. The meters
 *      stay: "is it hearing me" is why the pill exists, and #174 leaned on
 *      exactly that. The numbers stay reachable in the pill's `title`.
 *   2. `pausedWord` — the word PAUSED. A pause glyph beside a frozen timer
 *      already says it.
 *   3. `pill` — the diagnostics pill entirely. The controls are not optional;
 *      this is.
 *   4. `busyLabel` — "Summarizing…" down to its spinner, which only matters
 *      when a summary runs during a recording and so shares the row with the
 *      controls pill. The pill keeps its name (`role="status"` + `aria-label`).
 *
 * Two arrangements, because the thresholds depend on what else is in the row:
 * `roomy` is diagnostics + controls, `tight` adds the busy pill a summary puts
 * there. Every step fires earlier in `tight` — pinned as an ordering test,
 * since that direction is the one that can only ever be a bug.
 *
 * The cost order above is what each step is worth, not a promise about the
 * numbers: a step fires where the row needs the width it frees, so `tight`
 * reaches for the diagnostics pill (151px) BEFORE the word PAUSED (~50px),
 * because at 575px of content the row is 562px and no amount of PAUSED closes
 * that. Cheapest-first only holds where the cheap step is enough — `roomy`,
 * where it is, keeps PAUSED to 420 and the pill to 370.
 *
 * The numbers are the CONTAINER'S CONTENT BOX, which is the body column minus
 * this bar's `px-4` (32px) — so `BODY_MIN`'s 420 column is 388 here. They are
 * measured in the graphite theme (the wider of the two) against the widest
 * honest content — a 90-minute capture, paused, with an hour-plus timer — by
 * `scripts/measure-recording-bar.js` over the harness's `?case=recbar-*`. jsdom
 * pins every box to 0, so it cannot answer this and no unit test tries to.
 * Re-derive them with that script after any change to the row's contents or to
 * a theme's control metrics.
 */
export const ROW_STEPS = {
  // diagnostics + controls. Full row 583, compact 406, compact over a
  // PAUSED-less controls pill 356.
  roomy: {
    detail: "@max-[600px]:hidden",
    pausedWord: "@max-[420px]:hidden",
    pill: "@max-[370px]:hidden",
    busyLabel: "",
  },
  // diagnostics + busy + controls. Full row 739, compact 562, no diagnostics
  // 411, bare spinner 259 — which clears the narrowest column the layout can
  // produce.
  tight: {
    detail: "@max-[760px]:hidden",
    pausedWord: "@max-[430px]:hidden",
    pill: "@max-[575px]:hidden",
    // `sr-only`, not `hidden`: the pill is a `role="status"` live region and
    // `display: none` would leave it announcing nothing when a summary starts.
    // Absolutely positioned, so it is out of flow and out of the flex gap —
    // width-identical to hiding it, which the sweep confirms.
    busyLabel: "@max-[430px]:sr-only",
  },
} as const;

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

  const [elapsed, setElapsed] = useState(0);
  useEffect(() => {
    if (phase !== "recording" && phase !== "paused") {
      setElapsed(0);
      return;
    }
    if (phase === "paused") return; // hold the timer while paused
    const start = Date.now() - elapsed * 1000;
    const t = window.setInterval(() => setElapsed(Math.floor((Date.now() - start) / 1000)), 250);
    return () => window.clearInterval(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [phase]);

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
  // Which arrangement the row is in (#177). Three pills only ever happen one
  // way — a summary running while this note records — and the diagnostics pill
  // shows only while it does, so this one choice covers every step.
  const steps = isSummarizing && hasControls ? ROW_STEPS.tight : ROW_STEPS.roomy;
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
      {phase === "stopping" && <BusyPill label="Stopping…" />}
      {phase === "diarizing" && <BusyPill label="Identifying speakers…" />}
      {isSummarizing && <BusyPill label="Summarizing…" labelClass={steps.busyLabel} />}

      {hasControls && (
        <div
          className={cn(
            "nd-recpill no-drag shrink-0 whitespace-nowrap flex items-stretch h-[38px] rounded-full overflow-hidden border",
            recording ? "border-[var(--color-record)]" : "border-[var(--color-line-visible)]"
          )}
        >
          <div
            className={cn(
              "flex items-center gap-[9px] px-4 text-[15px] font-semibold tabular-nums",
              recording ? "text-[var(--color-record)]" : "text-[var(--color-text-muted)]"
            )}
          >
            {recording
              ? <span className="rec-dot inline-block w-[9px] h-[9px] rounded-full bg-[var(--color-record)]" />
              : <Pause size={12} strokeWidth={1.8} />}
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
        </div>
      )}
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
