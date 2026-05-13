import { useMemo } from "react";
import { Link, useParams, useNavigate } from "react-router-dom";
import { Folder as FolderIcon } from "lucide-react";
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

  if (!folder) {
    return (
      <div className="h-full flex flex-col items-center justify-center gap-3">
        <div className="text-[var(--color-text-muted)]">Folder not found</div>
        <button onClick={() => navigate("/")} className="nd-action">
          Go home
        </button>
      </div>
    );
  }

  const count = folderNotes.length;
  const countLabel = count === 1 ? "1 note" : `${count} notes`;

  return (
    <div className="h-full flex flex-col overflow-hidden">
      <div className="max-w-3xl mx-auto w-full px-12 pt-16 pb-6 flex items-center justify-between gap-6">
        <div className="flex items-center gap-4 min-w-0">
          <div className="w-12 h-12 rounded-md bg-[var(--color-surface)] flex items-center justify-center text-[var(--color-text-muted)] shrink-0">
            <FolderIcon size={26} strokeWidth={1.2} />
          </div>
          <h1 className="text-5xl font-serif tracking-tight truncate">
            {folder.name}
          </h1>
        </div>
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
          <div className="max-w-3xl mx-auto w-full px-12 py-10 text-sm text-[var(--color-text-muted)]">
            No notes in this folder yet.
          </div>
        ) : (
          <ul className="pt-2">
            {folderNotes.map((n) => (
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
        )}
      </div>
    </div>
  );
}
