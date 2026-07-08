import { useRecordingStore } from "../lib/store";

// Bottom-right toast that appears during the post-stop diarize step.
// The recording status store carries the phase from the backend; we
// just render a discrete card while diarize is active. Mounted globally
// so it stays visible if the user navigates away from the note being
// processed.
export function PolishToast() {
  const phase = useRecordingStore((s) => s.status.phase);
  if (phase !== "diarizing") return null;

  return (
    <div className="no-drag fixed bottom-6 right-6 z-[60] max-w-sm">
      <div className="px-4 py-3 rounded-lg bg-[var(--color-surface)] border border-[var(--color-line)] shadow-md text-sm flex items-center gap-3">
        <Spinner />
        <div>
          <div className="font-medium">Identifying speakers…</div>
          <div className="text-[var(--color-text-muted)] text-xs">
            Running the diarization model.
          </div>
        </div>
      </div>
    </div>
  );
}

function Spinner() {
  return (
    <span
      className="inline-block w-3.5 h-3.5 rounded-full border-2 border-[var(--color-line-visible)] border-t-[var(--color-text)] animate-spin"
      aria-hidden
    />
  );
}
