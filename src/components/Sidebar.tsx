import { Link, useNavigate, useLocation } from "react-router-dom";
import { useMemo, useState } from "react";
import { ChevronLeft, Home as HomeIcon, Plus, Search, Settings as SettingsIcon, Trash2 } from "lucide-react";
import { useNotesStore } from "../lib/store";
import { ipc, type Note } from "../lib/ipc";
import { cn } from "../lib/cn";

function group(notes: Note[]) {
  const today = new Date(); today.setHours(0, 0, 0, 0);
  const yest = new Date(today); yest.setDate(today.getDate() - 1);
  const week = new Date(today); week.setDate(today.getDate() - 7);

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
    else if (d >= week) groups[2].items.push(n);
    else groups[3].items.push(n);
  }
  return groups.filter((g) => g.items.length);
}

export function Sidebar({ onCollapse }: { onCollapse: () => void }) {
  const navigate = useNavigate();
  const location = useLocation();
  const notes = useNotesStore((s) => s.notes);
  const upsert = useNotesStore((s) => s.upsertLocal);
  const removeLocal = useNotesStore((s) => s.removeLocal);
  const [q, setQ] = useState("");

  async function deleteNote(e: React.MouseEvent, id: string) {
    e.preventDefault();
    e.stopPropagation();
    await ipc.deleteNote(id);
    removeLocal(id);
    if (location.pathname === `/note/${id}`) navigate("/");
  }

  const filtered = useMemo(() => {
    if (!q.trim()) return notes;
    const needle = q.toLowerCase();
    return notes.filter(
      (n) =>
        n.title.toLowerCase().includes(needle) ||
        n.body.toLowerCase().includes(needle) ||
        n.transcript.toLowerCase().includes(needle)
    );
  }, [notes, q]);

  const grouped = group(filtered);

  async function newNote() {
    const note = await ipc.createNote();
    upsert(note);
    navigate(`/note/${note.id}`);
  }

  return (
    <div className="h-full flex flex-col p-3 gap-2">
      <div data-tauri-drag-region className="h-8 flex items-center justify-end">
        <button
          onClick={onCollapse}
          className="no-drag p-1.5 rounded-md hover:bg-[var(--color-pill-hover)] text-[var(--color-text-muted)] hover:text-[var(--color-text)]"
          aria-label="Collapse sidebar"
          title="⌘\"
        >
          <ChevronLeft size={16} strokeWidth={1.5} />
        </button>
      </div>

      <div className="no-drag flex items-center gap-2 px-3 py-2 rounded-md border border-[var(--color-line)] bg-[var(--color-surface)] focus-within:border-[var(--color-text-muted)] transition-colors">
        <Search size={14} strokeWidth={1.5} className="text-[var(--color-text-muted)] shrink-0" />
        <input
          data-search-input
          value={q}
          onChange={(e) => setQ(e.target.value)}
          placeholder="Search"
          className="flex-1 text-sm"
        />
        <span className="text-[10px] text-[var(--color-text-muted)] tracking-[0.06em]" style={{ fontFamily: "var(--font-mono)" }}>⌘K</span>
      </div>

      <Link
        to="/"
        className={cn(
          "no-drag flex items-center gap-2 px-2 py-2 rounded-md text-sm transition-colors",
          location.pathname === "/"
            ? "bg-[var(--color-pill-hover)] text-[var(--color-text)]"
            : "text-[var(--color-text-muted)] hover:bg-[var(--color-pill-hover)] hover:text-[var(--color-text)]"
        )}
      >
        <HomeIcon size={15} strokeWidth={1.5} />
        <span>Home</span>
      </Link>

      <button
        onClick={newNote}
        className="no-drag flex items-center gap-2 px-2 py-2 rounded-md text-sm text-left text-[var(--color-text-muted)] hover:bg-[var(--color-pill-hover)] hover:text-[var(--color-text)] transition-colors"
        title="⌘N"
      >
        <Plus size={15} strokeWidth={1.5} />
        <span>New note</span>
      </button>

      <div className="nd-label mt-4 px-2">My notes</div>

      <div className="flex-1 overflow-y-auto -mx-1 px-1">
        {grouped.length === 0 && (
          <div className="px-2 py-4 text-sm text-[var(--color-ink-muted)]">No notes yet</div>
        )}
        {grouped.map((g) => (
          <div key={g.label} className="mb-3">
            <div className="nd-label px-2 py-1.5">{g.label}</div>
            {g.items.map((n) => {
              const active = location.pathname === `/note/${n.id}`;
              return (
                <Link
                  key={n.id}
                  to={`/note/${n.id}`}
                  className={cn(
                    "no-drag group flex items-center gap-1 pl-2 pr-1 py-2 rounded-md text-sm transition-colors",
                    active
                      ? "bg-[var(--color-pill-hover)] text-[var(--color-text)]"
                      : "text-[var(--color-text-muted)] hover:bg-[var(--color-pill-hover)] hover:text-[var(--color-text)]"
                  )}
                >
                  <span className="flex-1 truncate">{n.title.trim() || "Untitled"}</span>
                  <button
                    onClick={(e) => deleteNote(e, n.id)}
                    aria-label="Delete note"
                    title="Delete"
                    className="opacity-0 group-hover:opacity-100 focus:opacity-100 shrink-0 p-1 rounded text-[var(--color-text-muted)] hover:text-[var(--color-accent)] hover:bg-[var(--color-pill-hover)] transition-colors"
                  >
                    <Trash2 size={14} strokeWidth={1.5} />
                  </button>
                </Link>
              );
            })}
          </div>
        ))}
      </div>

      <Link
        to="/settings"
        className="no-drag flex items-center gap-2 px-2 py-2 rounded-md text-sm text-[var(--color-text-muted)] hover:bg-[var(--color-pill-hover)] hover:text-[var(--color-text)] transition-colors"
        title="⌘,"
      >
        <SettingsIcon size={15} strokeWidth={1.5} />
        <span>Settings</span>
      </Link>
    </div>
  );
}
