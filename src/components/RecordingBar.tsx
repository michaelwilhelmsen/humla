import { useEffect, useState } from "react";
import { Circle, Pause, Play, Square, Sparkles, Wand2 } from "lucide-react";
import { ipc } from "../lib/ipc";
import { useNotesStore, useRecordingStore } from "../lib/store";
import { cn } from "../lib/cn";

export function RecordingBar({ noteId }: { noteId: string }) {
  const status = useRecordingStore((s) => s.status);
  const isThisNote = status.noteId === noteId;
  const phase = isThisNote ? status.phase : "idle";
  const transcript = useNotesStore((s) => s.notes.find((n) => n.id === noteId)?.transcript ?? "");
  const hasTranscript = transcript.trim().length > 0;
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

  async function start() {
    try { await ipc.recordingStart(noteId); }
    catch (e) { useRecordingStore.getState().pushError({ noteId, message: String(e) }); }
  }
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
  async function summarize() {
    const t0 = performance.now();
    console.log(`[llm] summarize click note=${noteId}`);
    try {
      await ipc.summarizeNote(noteId);
      console.log(`[llm] summarize ok in ${Math.round(performance.now() - t0)}ms`);
    } catch (e) {
      console.error(`[llm] summarize failed in ${Math.round(performance.now() - t0)}ms:`, e);
      useRecordingStore.getState().pushError({ noteId, message: String(e) });
    }
  }
  async function polish() {
    try {
      await ipc.polishNote(noteId);
    } catch (e) {
      useRecordingStore.getState().pushError({ noteId, message: String(e) });
    }
  }

  const otherNoteRecording = status.noteId !== null && !isThisNote;

  return (
    <div className="absolute bottom-6 left-1/2 -translate-x-1/2 flex items-center gap-2">
      {showDiag && (
        <div className="shrink-0 whitespace-nowrap flex items-center gap-2 px-3 py-1.5 rounded-full bg-[var(--color-surface)] border border-[var(--color-line)] text-xs text-[var(--color-text-muted)] tabular-nums">
          <span className="flex items-center gap-1 whitespace-nowrap">
            <Dot active={micActive} /> mic {(diag.micFrames / 16000).toFixed(0)}s
          </span>
          <span className="flex items-center gap-1 whitespace-nowrap">
            <Dot active={sysActive} /> sys {(diag.sysFrames / 16000).toFixed(0)}s
          </span>
          <span className="whitespace-nowrap">· {diag.chunks} chunk{diag.chunks === 1 ? "" : "s"}</span>
        </div>
      )}

      {phase === "idle" && (
        <>
          <button
            onClick={start}
            disabled={otherNoteRecording}
            className="nd-action no-drag"
            title="⌘R"
          >
            <Circle size={11} fill="currentColor" strokeWidth={0} className="text-[var(--color-accent)]" />
            <span>Record</span>
          </button>
          {hasTranscript && (
            <button
              onClick={polish}
              className="nd-action no-drag"
              title="Re-run polish (typo + punctuation cleanup) on the saved transcript"
            >
              <Wand2 size={13} strokeWidth={1.5} />
              <span>Polish</span>
            </button>
          )}
          {hasTranscript && (
            <button
              onClick={summarize}
              className="nd-action no-drag"
              title="Summarize transcript"
            >
              <Sparkles size={13} strokeWidth={1.5} />
              <span>Summarize</span>
            </button>
          )}
        </>
      )}

      {phase === "starting" && <BusyPill label="Starting" />}
      {phase === "stopping" && <BusyPill label="Stopping" />}
      {phase === "retranscribing" && <BusyPill label="Re-transcribing" />}
      {phase === "diarizing" && <BusyPill label="Diarizing" />}
      {phase === "polishing" && <BusyPill label="Polishing" />}
      {phase === "summarizing" && <BusyPill label="Summarizing" />}

      {(phase === "recording" || phase === "paused") && (
        <div
          className={cn(
            "no-drag shrink-0 whitespace-nowrap flex items-stretch rounded-full overflow-hidden bg-[var(--color-surface)] border",
            phase === "recording" ? "border-[var(--color-accent)]" : "border-[var(--color-line-visible)]"
          )}
        >
          <div
            className={cn(
              "flex items-center gap-2 pl-4 pr-3 py-2 tabular-nums",
              phase === "recording" ? "text-[var(--color-accent)]" : "text-[var(--color-text-muted)]"
            )}
            style={{ fontFamily: "var(--font-mono)", fontSize: "12px", letterSpacing: "0.06em" }}
          >
            {phase === "recording"
              ? <Circle size={9} fill="currentColor" strokeWidth={0} className="rec-dot" />
              : <Pause size={11} strokeWidth={1.5} />}
            <span>{formatTime(elapsed)}</span>
            {phase === "paused" && <span className="uppercase tracking-[0.08em] text-[10px]">Paused</span>}
          </div>
          <span className="w-px self-stretch bg-[var(--color-line-visible)]" />
          <button
            onClick={phase === "recording" ? pause : resume}
            className="no-drag flex items-center px-3 hover:bg-[var(--color-pill-hover)] text-[var(--color-text)] transition-colors"
            title={phase === "recording" ? "Pause (⌘R)" : "Resume (⌘R)"}
            aria-label={phase === "recording" ? "Pause" : "Resume"}
          >
            {phase === "recording"
              ? <Pause size={13} strokeWidth={1.5} />
              : <Play size={13} strokeWidth={1.5} />}
          </button>
          <span className="w-px self-stretch bg-[var(--color-line-visible)]" />
          <button
            onClick={stop}
            className="no-drag flex items-center px-3 text-[var(--color-accent)] hover:bg-[var(--color-accent)]/10 transition-colors"
            title="Stop"
            aria-label="Stop"
          >
            <Square size={13} fill="currentColor" strokeWidth={0} />
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
    <div
      className="no-drag flex items-center gap-2 px-4 py-2 rounded-full bg-[var(--color-surface)] border border-[var(--color-line-visible)] text-[var(--color-text-muted)] uppercase tracking-[0.08em]"
      style={{ fontFamily: "var(--font-mono)", fontSize: "11px" }}
    >
      <span className="w-2.5 h-2.5 rounded-full border-2 border-current border-t-transparent animate-spin" />
      <span>{label}</span>
    </div>
  );
}

function Dot({ active }: { active: boolean }) {
  return (
    <span
      className={cn(
        "inline-block w-1.5 h-1.5 rounded-full",
        active ? "bg-green-500" : "bg-[var(--color-text-muted)]/40"
      )}
    />
  );
}

