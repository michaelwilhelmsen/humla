import { useNavigate } from "react-router-dom";
import { useRecordingStore } from "../lib/store";

export function Toaster() {
  const errors = useRecordingStore((s) => s.errors);
  const dismiss = useRecordingStore((s) => s.dismissError);
  const navigate = useNavigate();

  if (errors.length === 0) return null;

  return (
    <div className="no-drag fixed bottom-24 right-6 z-50 flex flex-col gap-2 max-w-sm">
      {errors.map((e) => (
        <div
          key={e.id}
          className="flex items-start gap-3 px-4 py-3 rounded-lg bg-[var(--color-surface)] border border-[var(--color-line)] shadow-md text-sm"
        >
          <div className="flex-1">
            <div className="font-medium text-red-600 dark:text-red-400 mb-1">Recording issue</div>
            <div className="text-[var(--color-text)]">{e.message}</div>
            {e.message.toLowerCase().includes("permission") && (
              <button
                onClick={() => { navigate("/settings"); dismiss(e.id); }}
                className="mt-2 text-xs underline text-[var(--color-text-muted)] hover:text-[var(--color-text)]"
              >
                Open Settings
              </button>
            )}
          </div>
          <button
            onClick={() => dismiss(e.id)}
            className="text-[var(--color-text-muted)] hover:text-[var(--color-text)]"
            aria-label="Dismiss"
          >×</button>
        </div>
      ))}
    </div>
  );
}
