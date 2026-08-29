import { useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";
import { Plus } from "lucide-react";
import { ipc } from "../lib/ipc";
import { useNotesStore, useRecordingStore } from "../lib/store";
import { cn } from "../lib/cn";
import { indexById, isRecorded, isSummarized } from "../lib/noteList";
import { NoteCard, type SelectIntent } from "../components/NoteCard";
import { BulkActionBar } from "../components/BulkActionBar";
import { Modal } from "./settings/components/Modal";

type FilterKey = "all" | "recorded" | "summarized" | "no-folder";

const FILTERS: { key: FilterKey; label: string }[] = [
  { key: "all", label: "All" },
  { key: "recorded", label: "Recorded" },
  { key: "summarized", label: "Summarized" },
  { key: "no-folder", label: "No folder" },
];

export function AllNotes() {
  const navigate = useNavigate();
  const notes = useNotesStore((s) => s.notes);
  const folders = useNotesStore((s) => s.folders);
  const clients = useNotesStore((s) => s.clients);
  const upsert = useNotesStore((s) => s.upsertLocal);
  const removeLocal = useNotesStore((s) => s.removeLocal);
  const pushError = useRecordingStore((s) => s.pushError);
  const [filter, setFilter] = useState<FilterKey>("all");

  // Multi-select (issue #19). `selected` holds note ids; `anchorRef` is the
  // range anchor for Shift-click. `busy` guards the bulk delete/move while
  // in flight; `confirmDelete` gates the single bulk-delete confirmation.
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const anchorRef = useRef<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);

  const sorted = useMemo(
    () => [...notes].sort((a, b) => b.created_at - a.created_at),
    [notes],
  );
  const folderById = useMemo(() => indexById(folders), [folders]);
  const clientById = useMemo(() => indexById(clients), [clients]);

  const filtered = useMemo(
    () =>
      sorted.filter((n) => {
        switch (filter) {
          case "recorded": return isRecorded(n);
          case "summarized": return isSummarized(n);
          case "no-folder": return !n.folder_id;
          default: return true;
        }
      }),
    [sorted, filter],
  );
  // Visual order of the currently-rendered cards — the source of truth for
  // Shift-click range selection.
  const orderedIds = useMemo(() => filtered.map((n) => n.id), [filtered]);
  const total = sorted.length;
  const shown = filtered.length;

  function clearSelection() {
    setSelected(new Set());
    anchorRef.current = null;
  }

  function onSelectRow(id: string, e: SelectIntent) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (e.shiftKey && anchorRef.current) {
        const a = orderedIds.indexOf(anchorRef.current);
        const b = orderedIds.indexOf(id);
        if (a !== -1 && b !== -1) {
          const [lo, hi] = a < b ? [a, b] : [b, a];
          for (let i = lo; i <= hi; i++) next.add(orderedIds[i]);
          return next; // anchor unchanged — repeated shift-clicks pivot on it
        }
      }
      // Cmd/Ctrl toggle (or Shift with no anchor yet).
      if (next.has(id)) next.delete(id);
      else next.add(id);
      anchorRef.current = id;
      return next;
    });
  }

  // Esc clears the selection. Bubble phase (default) so the delete-confirm
  // Modal's capture-phase handler preempts it — pressing Esc with the modal
  // open closes the modal first and leaves the selection intact.
  useEffect(() => {
    if (selected.size === 0) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") clearSelection();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [selected.size]);

  async function newNote() {
    const note = await ipc.createNote();
    upsert(note);
    navigate(`/note/${note.id}`);
  }

  function titleFor(id: string) {
    return notes.find((n) => n.id === id)?.title.trim() || "Untitled";
  }

  // Bulk soft-delete. Each note fires its own `note_deleted` sync hook via the
  // single-note command, satisfying the per-note tombstone invariant. On the
  // first failure we stop and report (never silently skip); notes already
  // deleted are dropped from the selection as we go.
  async function doDelete() {
    setBusy(true);
    const ids = [...selected];
    try {
      for (const id of ids) {
        try {
          await ipc.deleteNote(id);
          removeLocal(id);
          setSelected((prev) => {
            const next = new Set(prev);
            next.delete(id);
            return next;
          });
        } catch (e) {
          pushError({ noteId: id, message: `Couldn't delete "${titleFor(id)}": ${String(e)}` });
          return;
        }
      }
      clearSelection();
    } finally {
      setBusy(false);
      setConfirmDelete(false);
    }
  }

  // Bulk move (folderId null = "no folder"). Each note fires `note_upserted`
  // via the single-note command. Same stop-and-report semantics as delete.
  async function doMove(folderId: string | null) {
    setBusy(true);
    const ids = [...selected];
    try {
      for (const id of ids) {
        try {
          await ipc.moveNote(id, folderId);
          const note = notes.find((n) => n.id === id);
          if (note) upsert({ ...note, folder_id: folderId });
          setSelected((prev) => {
            const next = new Set(prev);
            next.delete(id);
            return next;
          });
        } catch (e) {
          pushError({ noteId: id, message: `Couldn't move "${titleFor(id)}": ${String(e)}` });
          return;
        }
      }
      clearSelection();
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="h-full flex flex-col overflow-hidden">
      {/* One scrolling well: the grid runs under a translucent title bar. New
          note + theme toggle live in the floating TopBar (top-right). */}
      <div className="flex-1 overflow-y-auto nd-well flex flex-col">
        <div className="nd-well-bar shrink-0">
          <div className="max-w-[1180px] mx-auto w-full px-8 pt-14 pb-3">
            <div className="flex items-center gap-3 px-1">
              <h1 className="nd-heading truncate">All notes</h1>
              <span className="text-[14px] text-[var(--color-text-disabled)] tabular-nums shrink-0">{total}</span>
            </div>
            {total > 0 && (
              <div className="flex flex-wrap gap-1.5 px-1 pt-3">
                {FILTERS.map((f) => (
                  <button
                    key={f.key}
                    onClick={() => setFilter(f.key)}
                    className={cn(
                      "no-drag text-[12.5px] px-3 py-[5px] rounded-full transition-colors",
                      filter === f.key
                        ? "bg-[var(--color-accent-soft)] text-[var(--color-accent-text)]"
                        : "text-[var(--color-text-muted)] hover:bg-[var(--color-pill-hover)]",
                    )}
                  >
                    {f.label}
                  </button>
                ))}
              </div>
            )}
          </div>
        </div>

        {total === 0 ? (
          <div className="flex-1 flex flex-col items-center justify-center gap-4 text-center px-12">
            <div className="text-[var(--color-text-muted)] flex items-center gap-2">
              <span>Press</span>
              <kbd
                className="px-2 py-0.5 border border-[var(--color-line-visible)] rounded text-xs"
                style={{ fontFamily: "var(--font-code)" }}
              >
                ⌘N
              </kbd>
              <span>to start a new note</span>
            </div>
            <button onClick={newNote} className="nd-btn nd-btn-primary no-drag">
              <Plus size={15} strokeWidth={1.8} />
              New note
            </button>
          </div>
        ) : shown === 0 ? (
          <div className="max-w-[1180px] mx-auto w-full px-8 pt-16 text-center text-sm text-[var(--color-text-muted)]">
            No notes match this filter.
          </div>
        ) : (
          // The grid clears the title bar's fade before the first row.
          <div className="max-w-[1180px] mx-auto w-full px-8 pt-6 pb-24">
            <ul className="nd-notegrid">
              {filtered.map((n) => (
                <NoteCard
                  key={n.id}
                  note={n}
                  folder={n.folder_id ? folderById.get(n.folder_id) : undefined}
                  client={n.client_id ? clientById.get(n.client_id) : undefined}
                  selected={selected.has(n.id)}
                  selectionActive={selected.size > 0}
                  onSelect={(e) => onSelectRow(n.id, e)}
                />
              ))}
            </ul>
          </div>
        )}
      </div>

      {/* Bar appears at the first selection: with an explicit checkbox affordance,
          a single pick with no visible action would read as a dead end. */}
      {selected.size >= 1 && (
        <BulkActionBar
          count={selected.size}
          folders={folders}
          busy={busy}
          onDelete={() => setConfirmDelete(true)}
          onMove={doMove}
          onCancel={clearSelection}
        />
      )}

      <Modal
        open={confirmDelete}
        onClose={() => {
          if (!busy) setConfirmDelete(false);
        }}
        title="Delete notes"
      >
        <p className="text-[14px] text-[var(--color-text)]">
          Move {selected.size} {selected.size === 1 ? "note" : "notes"} to Trash? You can restore
          {selected.size === 1 ? " it" : " them"} later.
        </p>
        <div className="flex justify-end gap-2 mt-5">
          <button className="nd-btn" onClick={() => setConfirmDelete(false)} disabled={busy}>
            Cancel
          </button>
          <button
            className="nd-btn"
            style={{ color: "var(--color-danger)" }}
            onClick={doDelete}
            disabled={busy}
          >
            Delete
          </button>
        </div>
      </Modal>
    </div>
  );
}
