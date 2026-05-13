import { useMemo } from "react";
import { Link, useNavigate } from "react-router-dom";
import { Plus } from "lucide-react";
import { ipc, type Note } from "../lib/ipc";
import { useNotesStore } from "../lib/store";

function formatMeetingTime(ts: number): string {
  const d = new Date(ts);
  const now = new Date();
  const sameDay =
    d.getFullYear() === now.getFullYear() &&
    d.getMonth() === now.getMonth() &&
    d.getDate() === now.getDate();
  if (sameDay) {
    return d.toLocaleTimeString(undefined, {
      hour: "2-digit",
      minute: "2-digit",
      hour12: false,
    });
  }
  const sameYear = d.getFullYear() === now.getFullYear();
  return d.toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
    ...(sameYear ? {} : { year: "numeric" }),
  });
}

function groupByDate(notes: Note[]) {
  const today = new Date();
  today.setHours(0, 0, 0, 0);
  const yest = new Date(today);
  yest.setDate(today.getDate() - 1);
  const weekStart = new Date(today);
  weekStart.setDate(today.getDate() - 7);

  const groups: { label: string; items: Note[] }[] = [
    { label: "Today", items: [] },
    { label: "Yesterday", items: [] },
    { label: "Earlier this week", items: [] },
    { label: "Older", items: [] },
  ];
  for (const n of notes) {
    const d = new Date(n.updated_at);
    if (d >= today) groups[0].items.push(n);
    else if (d >= yest) groups[1].items.push(n);
    else if (d >= weekStart) groups[2].items.push(n);
    else groups[3].items.push(n);
  }
  return groups.filter((g) => g.items.length > 0);
}

export function Home() {
  const navigate = useNavigate();
  const notes = useNotesStore((s) => s.notes);
  const upsert = useNotesStore((s) => s.upsertLocal);

  const sorted = useMemo(
    () => [...notes].sort((a, b) => b.updated_at - a.updated_at),
    [notes],
  );
  const groups = useMemo(() => groupByDate(sorted), [sorted]);
  const count = sorted.length;
  const countLabel = count === 1 ? "1 note" : `${count} notes`;

  async function newNote() {
    const note = await ipc.createNote();
    upsert(note);
    navigate(`/note/${note.id}`);
  }

  return (
    <div className="h-full flex flex-col overflow-hidden">
      <div className="max-w-3xl mx-auto w-full px-12 pt-16 pb-6 flex items-center justify-between gap-6">
        <h1 className="text-5xl font-serif tracking-tight truncate">
          All notes
        </h1>
        <div className="text-sm text-[var(--color-text-muted)] shrink-0">
          {countLabel}
        </div>
      </div>

      <div className="max-w-3xl mx-auto w-full px-12">
        <div className="-mx-4 px-4 pt-4 pb-3 flex items-baseline gap-2 border-b border-[var(--color-line)]">
          <span className="text-sm font-medium text-[var(--color-text)]">
            Notes
          </span>
          <span
            className="text-xs text-[var(--color-text-muted)] tabular-nums"
            style={{ fontFamily: "var(--font-mono)" }}
          >
            {count}
          </span>
        </div>
      </div>

      <div className="flex-1 overflow-y-auto">
        {count === 0 ? (
          <div className="h-full flex flex-col items-center justify-center gap-4 text-center -mt-12 px-12">
            <div className="text-[var(--color-text-muted)] flex items-center gap-2">
              <span>Press</span>
              <kbd
                className="px-2 py-0.5 border border-[var(--color-line-visible)] rounded text-xs"
                style={{ fontFamily: "var(--font-mono)" }}
              >
                ⌘N
              </kbd>
              <span>to start a new note</span>
            </div>
            <button onClick={newNote} className="nd-action no-drag">
              <Plus size={14} strokeWidth={1.5} className="inline-block mr-1 -mt-0.5" />
              New note
            </button>
          </div>
        ) : (
          <div className="pb-6">
            {groups.map((g) => (
              <section key={g.label}>
                <div className="sticky top-0 bg-[var(--color-canvas)] z-10">
                  <div className="max-w-3xl mx-auto w-full px-12 pt-6 pb-2 nd-label">
                    {g.label}
                  </div>
                </div>
                <ul>
                  {g.items.map((n) => (
                    <li key={n.id}>
                      <Link to={`/note/${n.id}`} className="group block">
                        <div className="max-w-3xl mx-auto w-full px-12">
                          <div className="-mx-4 px-4 py-3.5 rounded-md hover:bg-[var(--color-sidebar-active)] transition-colors flex items-center gap-6">
                            <span
                              className="w-16 text-sm text-[var(--color-text-muted)] tabular-nums shrink-0"
                              style={{ fontFamily: "var(--font-mono)" }}
                            >
                              {formatMeetingTime(n.updated_at)}
                            </span>
                            <span className="flex-1 truncate text-sm text-[var(--color-text)]">
                              {n.title.trim() || "Untitled"}
                            </span>
                            {/* Duration column reserved for Slice 4 (duration_ms). */}
                          </div>
                        </div>
                      </Link>
                    </li>
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
