import { useMemo } from "react";
import { Link, useParams, useNavigate } from "react-router-dom";
import { Folder as FolderIcon } from "lucide-react";
import { type Note } from "../lib/ipc";
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

// One-line snippet for a row: strip the Tiptap body HTML (textContent only —
// the element is never inserted live, so no script runs), falling back to the
// transcript for pure voice memos.
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

export function Folder() {
  const { id } = useParams<{ id: string }>();
  const navigate = useNavigate();
  const folders = useNotesStore((s) => s.folders);
  const notes = useNotesStore((s) => s.notes);

  const folder = useMemo(() => folders.find((f) => f.id === id), [folders, id]);
  const folderNotes = useMemo(
    () =>
      notes
        .filter((n) => n.folder_id === id)
        .sort((a, b) => b.updated_at - a.updated_at),
    [notes, id],
  );
  const groups = useMemo(() => groupByDate(folderNotes), [folderNotes]);

  if (!folder) {
    return (
      <div className="h-full flex flex-col items-center justify-center gap-3">
        <div className="text-[var(--color-text-muted)]">Folder not found</div>
        <button onClick={() => navigate("/")} className="nd-btn no-drag">
          Go home
        </button>
      </div>
    );
  }

  const count = folderNotes.length;

  return (
    <div className="h-full flex flex-col overflow-hidden">
      {/* New note + theme toggle live in the floating TopBar (top-right). */}
      <div className="shrink-0">
        <div className="max-w-[880px] mx-auto w-full px-8 pt-14">
          <div className="flex items-center gap-3 px-2">
            <span className="shrink-0 grid place-items-center w-8 h-8 rounded-[9px] bg-[var(--color-surface-raised)] border border-[var(--color-line-visible)] text-[var(--color-text-muted)]">
              <FolderIcon size={16} strokeWidth={1.7} />
            </span>
            <h1 className="text-[25px] font-semibold tracking-[-0.022em] truncate">{folder.name}</h1>
            <span className="text-[14px] text-[var(--color-text-disabled)] tabular-nums shrink-0">{count}</span>
          </div>
        </div>
      </div>

      <div className="flex-1 overflow-y-auto">
        {count === 0 ? (
          <div className="max-w-[880px] mx-auto w-full px-8 pt-16 text-center text-sm text-[var(--color-text-muted)]">
            No notes in this folder yet.
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
                    <NoteRow key={n.id} note={n} />
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

function NoteRow({ note }: { note: Note }) {
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
            <span className="shrink-0 pt-px text-[12px] text-[var(--color-text-disabled)] tabular-nums whitespace-nowrap">
              {formatMeetingTime(note.updated_at)}
            </span>
          </div>
        </div>
      </Link>
    </li>
  );
}
