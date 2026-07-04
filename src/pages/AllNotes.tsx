import { useMemo, useState } from "react";
import { useNavigate } from "react-router-dom";
import { Plus } from "lucide-react";
import { ipc, type Folder } from "../lib/ipc";
import { useNotesStore } from "../lib/store";
import { cn } from "../lib/cn";
import { groupByDate, isRecorded, isSummarized } from "../lib/noteList";
import { NoteListRow } from "../components/NoteListRow";

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
  const upsert = useNotesStore((s) => s.upsertLocal);
  const [filter, setFilter] = useState<FilterKey>("all");

  const sorted = useMemo(
    () => [...notes].sort((a, b) => b.created_at - a.created_at),
    [notes],
  );
  const folderById = useMemo(() => {
    const map = new Map<string, Folder>();
    for (const f of folders) map.set(f.id, f);
    return map;
  }, [folders]);

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
  const groups = useMemo(() => groupByDate(filtered), [filtered]);
  const total = sorted.length;
  const shown = filtered.length;

  async function newNote() {
    const note = await ipc.createNote();
    upsert(note);
    navigate(`/note/${note.id}`);
  }

  return (
    <div className="h-full flex flex-col overflow-hidden">
      {/* Title + filter chips stay pinned above the scrolling list. New note
          + theme toggle live in the floating TopBar (top-right). */}
      <div className="shrink-0">
        <div className="max-w-[880px] mx-auto w-full px-8 pt-14">
          <div className="flex items-center gap-3 px-2">
            <h1 className="text-[25px] font-semibold tracking-[-0.022em] truncate">All notes</h1>
            <span className="text-[14px] text-[var(--color-text-disabled)] tabular-nums shrink-0">{total}</span>
          </div>
          {total > 0 && (
            <div className="flex flex-wrap gap-1.5 px-2 pt-3 pb-1">
              {FILTERS.map((f) => (
                <button
                  key={f.key}
                  onClick={() => setFilter(f.key)}
                  className={cn(
                    "no-drag text-[12.5px] px-3 py-[5px] rounded-full border transition-colors",
                    filter === f.key
                      ? "bg-[var(--color-accent-soft)] text-[var(--color-accent-text)] border-transparent"
                      : "text-[var(--color-text-muted)] border-[var(--color-line-visible)] hover:text-[var(--color-text)]",
                  )}
                >
                  {f.label}
                </button>
              ))}
            </div>
          )}
        </div>
      </div>

      <div className="flex-1 overflow-y-auto">
        {total === 0 ? (
          <div className="h-full flex flex-col items-center justify-center gap-4 text-center -mt-12 px-12">
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
          <div className="max-w-[880px] mx-auto w-full px-8 pt-16 text-center text-sm text-[var(--color-text-muted)]">
            No notes match this filter.
          </div>
        ) : (
          <div className="pb-20">
            {groups.map((g) => (
              <section key={g.label}>
                <div className="sticky top-0 z-10 bg-[var(--color-canvas)]">
                  <div className="max-w-[880px] mx-auto w-full px-8 pt-5 pb-1">
                    <span className="block px-3 nd-label">{g.label}</span>
                  </div>
                </div>
                <ul>
                  {g.items.map((n) => (
                    <NoteListRow
                      key={n.id}
                      note={n}
                      folder={n.folder_id ? folderById.get(n.folder_id) : undefined}
                    />
                  ))}
                </ul>
              </section>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
