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
      <div className="max-w-3xl mx-auto w-full px-12 pt-16 pb-6 flex items-center justify-between gap-6">
        <h1 className="text-5xl font-serif tracking-tight truncate">Trash</h1>
        <div className="text-sm text-[var(--color-text-muted)] shrink-0">
          {count === 1 ? "1 note" : `${count} notes`}
        </div>
      </div>

      <div className="max-w-3xl mx-auto w-full px-12">
        <div className="-mx-4 px-4 pt-4 pb-3 border-b border-[var(--color-line)]">
          <span className="text-sm text-[var(--color-text-muted)]">
            Deleted notes are kept here. Restore them, or delete permanently.
          </span>
        </div>
      </div>

      <div className="flex-1 overflow-y-auto">
        {loading ? (
          <div className="max-w-3xl mx-auto w-full px-12 pt-8 text-sm text-[var(--color-text-muted)]">
            Loading…
          </div>
        ) : count === 0 ? (
          <div className="h-full flex items-center justify-center -mt-12 text-[var(--color-text-muted)]">
            Trash is empty
          </div>
        ) : (
          <ul className="pb-6 pt-2">
            {trashed.map((n) => (
              <li key={n.id}>
                <div className="max-w-3xl mx-auto w-full px-12">
                  <div className="-mx-4 px-4 py-3.5 rounded-md hover:bg-[var(--color-sidebar-active)] transition-colors flex items-center gap-4">
                    <span
                      className="min-w-28 text-xs text-[var(--color-text-muted)] tabular-nums shrink-0 whitespace-nowrap"
                      style={{ fontFamily: "var(--font-mono)" }}
                    >
                      {formatDeleted(n.deleted_at)}
                    </span>
                    <span className="flex-1 truncate text-sm text-[var(--color-text)]">
                      {n.title.trim() || "Untitled"}
                    </span>
                    <button
                      onClick={() => restore(n.id)}
                      disabled={busy === n.id}
                      title="Restore"
                      className="shrink-0 inline-flex items-center gap-1.5 px-2 py-1 rounded text-xs text-[var(--color-text-muted)] hover:text-[var(--color-text)] hover:bg-[var(--color-pill-hover)] transition-colors disabled:opacity-50"
                    >
                      <RotateCcw size={14} strokeWidth={1.5} /> Restore
                    </button>
                    <button
                      onClick={() => purge(n.id)}
                      disabled={busy === n.id}
                      title="Delete permanently"
                      aria-label="Delete permanently"
                      className="shrink-0 p-1.5 rounded text-[var(--color-text-muted)] hover:text-[var(--color-accent)] hover:bg-[var(--color-pill-hover)] transition-colors disabled:opacity-50"
                    >
                      <Trash2 size={14} strokeWidth={1.5} />
                    </button>
                  </div>
                </div>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}
