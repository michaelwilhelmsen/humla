import { useMemo, useState } from "react";
import { Link, useNavigate } from "react-router-dom";
import { Folder as FolderIcon, Plus } from "lucide-react";
import { ipc, type Folder, type Note } from "../lib/ipc";
import { useNotesStore } from "../lib/store";
import { cn } from "../lib/cn";

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

// A note counts as "recorded" once it has any transcript text (Recorded
// filter); "summarized" once a summary has been generated (Summarized filter).
function isRecorded(n: Note): boolean {
  return n.transcript.trim().length > 0;
}

function isSummarized(n: Note): boolean {
  return n.summary.trim().length > 0;
}

// One-line snippet for the row. The body is Tiptap HTML, so strip tags via a
// detached element (textContent only — never inserted live, so no script
// runs); fall back to the transcript for pure voice memos.
function notePreview(n: Note): string {
  let text = "";
  const html = n.body?.trim();
  if (html) {
    const el = document.createElement("div");
    el.innerHTML = html;
    text = el.textContent || "";
  }
  if (!text.trim() && n.transcript) text = n.transcript;
  return text.replace(/\s+/g, " ").trim();
}

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
    () => [...notes].sort((a, b) => b.updated_at - a.updated_at),
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
                      ? "bg-[var(--color-accent)] text-[var(--color-on-accent)] border-[var(--color-accent)]"
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
                    <NoteRow
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

function NoteRow({ note, folder }: { note: Note; folder?: Folder }) {
  const preview = notePreview(note);
  return (
    <li>
      <Link to={`/note/${note.id}`} className="group block">
        <div className="max-w-[880px] mx-auto w-full px-8">
          <div className="flex items-start gap-3 p-3 rounded-[11px] hover:bg-[var(--color-pill-hover)] transition-colors">
            <div className="flex-1 min-w-0">
              <div className="text-[14.5px] font-medium text-[var(--color-text)] truncate">
                {note.title.trim() || "Untitled"}
              </div>
              {preview && (
                <div className="mt-0.5 text-[13px] text-[var(--color-text-muted)] truncate">
                  {preview}
                </div>
              )}
            </div>
            <div className="shrink-0 flex flex-col items-end gap-1.5 pt-px">
              <span className="text-[12px] text-[var(--color-text-disabled)] tabular-nums whitespace-nowrap">
                {formatMeetingTime(note.updated_at)}
              </span>
              {folder && (
                <span className="inline-flex items-center gap-1.5 max-w-[12rem] text-[11.5px] text-[var(--color-text-muted)] border border-[var(--color-line-visible)] rounded-md px-1.5 py-0.5">
                  <FolderIcon size={12} strokeWidth={1.6} className="shrink-0 opacity-70" />
                  <span className="truncate">{folder.name}</span>
                </span>
              )}
            </div>
          </div>
        </div>
      </Link>
    </li>
  );
}
