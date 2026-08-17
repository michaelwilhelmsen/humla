import { useCallback, useEffect, useState } from "react";
import { RotateCcw, Trash2 } from "lucide-react";
import { ipc, type Note } from "../lib/ipc";
import { useNotesStore } from "../lib/store";
import { useCloudStore } from "../lib/cloud";

function formatDeleted(ts: number | null | undefined): string {
  if (!ts) return "";
  return new Date(ts).toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  });
}

// Trash view: soft-deleted notes for the active workspace. Restore them (which
// re-syncs, so they reappear for teammates too) or delete permanently. Fetched
// directly via IPC rather than from the notes store, which only holds live notes.
export function Trash() {
  const refreshNotes = useNotesStore((s) => s.refresh);
  // Refetch when the active workspace changes (trash is workspace-scoped).
  const wsId = useCloudStore((s) => s.status.current_workspace?.id ?? "");
  const [trashed, setTrashed] = useState<Note[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      setTrashed(await ipc.listTrashedNotes());
    } catch {
      setTrashed([]);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    load();
  }, [load, wsId]);

  async function restore(id: string) {
    setBusy(id);
    try {
      await ipc.restoreNote(id);
      await refreshNotes();
      await load();
    } finally {
      setBusy(null);
    }
  }

  async function purge(id: string) {
    setBusy(id);
    try {
      await ipc.purgeNote(id);
      await load();
    } finally {
      setBusy(null);
    }
  }

  const count = trashed.length;

  return (
    <div className="h-full flex flex-col overflow-hidden">
      <div className="shrink-0">
        <div className="max-w-[880px] mx-auto w-full px-8 pt-14">
          <div className="flex items-center gap-3 px-2">
            <h1 className="nd-heading truncate">Trash</h1>
            <span className="text-[14px] text-[var(--color-text-disabled)] tabular-nums shrink-0">{count}</span>
          </div>
          <p className="px-2 pt-2 text-[13px] text-[var(--color-text-muted)]">
            Deleted notes are kept here. Restore them, or delete permanently.
          </p>
        </div>
      </div>

      <div className="flex-1 overflow-y-auto">
        {loading ? (
          <div className="max-w-[880px] mx-auto w-full px-8 pt-8 text-sm text-[var(--color-text-muted)]">
            Loading…
          </div>
        ) : count === 0 ? (
          <div className="h-full flex items-center justify-center -mt-12 text-[var(--color-text-muted)]">
            Trash is empty
          </div>
        ) : (
          <div className="pt-3 pb-20">
            <ul>
              {trashed.map((n) => (
                <li key={n.id}>
                  <div className="max-w-[880px] mx-auto w-full px-8">
                    <div className="flex items-center gap-3 p-3 rounded-[11px] hover:bg-[var(--color-pill-hover)] transition-colors">
                      <span className="flex-1 truncate text-[14.5px] text-[var(--color-text)]">
                        {n.title.trim() || "Untitled"}
                      </span>
                      <span className="shrink-0 text-[12px] text-[var(--color-text-disabled)] tabular-nums whitespace-nowrap">
                        {formatDeleted(n.deleted_at)}
                      </span>
                      <button
                        onClick={() => restore(n.id)}
                        disabled={busy === n.id}
                        title="Restore"
                        className="shrink-0 inline-flex items-center gap-1.5 px-2.5 py-1 rounded-md text-[12px] text-[var(--color-text-muted)] hover:text-[var(--color-text)] hover:bg-[var(--color-surface-raised)] transition-colors disabled:opacity-50"
                      >
                        <RotateCcw size={14} strokeWidth={1.6} /> Restore
                      </button>
                      <button
                        onClick={() => purge(n.id)}
                        disabled={busy === n.id}
                        title="Delete permanently"
                        aria-label="Delete permanently"
                        className="shrink-0 grid place-items-center w-7 h-7 rounded-md text-[var(--color-text-muted)] hover:text-[var(--color-danger)] hover:bg-[var(--color-pill-hover)] transition-colors disabled:opacity-50"
                      >
                        <Trash2 size={14} strokeWidth={1.6} />
                      </button>
                    </div>
                  </div>
                </li>
              ))}
            </ul>
          </div>
        )}
      </div>
    </div>
  );
}
