import { useEffect, useState } from "react";
import { Pause, Play, Square } from "lucide-react";
import { ipc } from "../lib/ipc";
import { useRecordingStore } from "../lib/store";
import { cn } from "../lib/cn";

// Floating recording controls. Record / Summarize live in the note toolbar
// now; this bar surfaces only the in-flight states (starting / recording /
// paused / stopping / diarizing / summarizing). Two pills: a neutral status
// pill (mic/sys seconds + chunk count) and a red-outlined timer/controls
// pill. No audio visualizer — the status pill's live dots carry "is it
// hearing me?" instead.
export function RecordingBar({ noteId }: { noteId: string }) {
  const status = useRecordingStore((s) => s.status);
  const isThisNote = status.noteId === noteId;
  const phase = isThisNote ? status.phase : "idle";
  const isSummarizing = useRecordingStore((s) => !!s.summarizing[noteId]);
  const diag = useRecordingStore((s) => s.diag);
  const showDiag = (phase === "recording" || phase === "paused") && diag && diag.noteId === noteId;
  const micActive = phase === "recording" && !!showDiag && diag.micPeak > 0.001;
  const sysActive = phase === "recording" && !!showDiag && diag.sysPeak > 0.001;

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
  // The control pill's inner dividers + button hovers tint red while live,
  // neutral while paused — keeps the red reserved for the active state.
  const ctrlEdge = recording
    ? "border-[color-mix(in_srgb,var(--color-record)_32%,transparent)] hover:bg-[color-mix(in_srgb,var(--color-record)_9%,transparent)]"
    : "border-[var(--color-line-visible)] hover:bg-[var(--color-pill-hover)]";

  return (
    <div className="absolute bottom-6 left-1/2 -translate-x-1/2 z-30 flex items-center gap-2.5">
      {showDiag && (
        <div className="nd-recpill shrink-0 whitespace-nowrap flex items-center gap-[15px] h-[38px] px-4 rounded-full border border-[var(--color-line-visible)] text-[13px] text-[var(--color-text-muted)] tabular-nums">
          <span className="inline-flex items-center gap-[7px]">
            <Dot active={micActive} /> mic {(diag.micFrames / 16000).toFixed(0)}s
          </span>
          <span className="inline-flex items-center gap-[7px]">
            <Dot active={sysActive} /> sys {(diag.sysFrames / 16000).toFixed(0)}s
          </span>
          <span className="text-[var(--color-text-disabled)]">· {diag.chunks} chunk{diag.chunks === 1 ? "" : "s"}</span>
        </div>
      )}

      {phase === "starting" && <BusyPill label="Starting…" />}
      {phase === "stopping" && <BusyPill label="Stopping…" />}
      {phase === "diarizing" && <BusyPill label="Identifying speakers…" />}
      {isSummarizing && <BusyPill label="Summarizing…" />}

      {(phase === "recording" || phase === "paused") && (
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
              <span className="uppercase tracking-[0.08em] text-[10px] font-medium">Paused</span>
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
  );
}

function formatTime(s: number) {
  const m = Math.floor(s / 60);
  const r = s % 60;
  return `${m}:${r.toString().padStart(2, "0")}`;
}

function BusyPill({ label }: { label: string }) {
  return (
    <div className="nd-recpill no-drag shrink-0 flex items-center gap-2 h-[38px] px-4 rounded-full border border-[var(--color-line-visible)] text-[13px] font-medium text-[var(--color-text-muted)]">
      <span className="w-3 h-3 rounded-full border-2 border-current border-t-transparent animate-spin" />
      <span>{label}</span>
    </div>
  );
}

function Dot({ active }: { active: boolean }) {
  return (
    <span
      className={cn(
        "inline-block w-[7px] h-[7px] rounded-full",
        active ? "bg-[var(--color-success)]" : "bg-[var(--color-text-muted)]/40"
      )}
    />
  );
}
