import { useRecordingStore } from "../lib/store";

// Bottom-right toast that appears during post-stop processing —
// diarization first, then polish. The recording status store already
// carries the phase from the backend; we just render a discrete card
// while either step is active. Mounted globally so it stays visible if
// the user navigates away from the note being processed.
export function PolishToast() {
  const phase = useRecordingStore((s) => s.status.phase);
  const message = messageForPhase(phase);
  if (!message) return null;

  return (
    <div className="no-drag fixed bottom-6 right-6 z-50 max-w-sm">
      <div className="px-4 py-3 rounded-lg bg-[var(--color-surface)] border border-[var(--color-line)] shadow-md text-sm flex items-center gap-3">
        <Spinner />
        <div>
          <div className="font-medium">{message.title}</div>
          <div className="text-[var(--color-text-muted)] text-xs">
            {message.subtitle}
          </div>
        </div>
      </div>
    </div>
  );
}

function messageForPhase(phase: string): { title: string; subtitle: string } | null {
  if (phase === "diarizing") {
    return {
      title: "Identifying speakers…",
      subtitle: "Running the diarization model.",
    };
  }
  if (phase === "polishing") {
    return {
      title: "Polishing transcript…",
      subtitle: "Cleaning up typos and chunk-boundary artifacts.",
    };
  }
  return null;
}

function Spinner() {
  return (
    <span
      className="inline-block w-3.5 h-3.5 rounded-full border-2 border-[var(--color-line-visible)] border-t-[var(--color-text)] animate-spin"
      aria-hidden
    />
  );
}
