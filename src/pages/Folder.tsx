import { useMemo } from "react";
import { useParams, useNavigate } from "react-router-dom";
import { Folder as FolderIcon } from "lucide-react";
import { useNotesStore } from "../lib/store";
import { groupByDate } from "../lib/noteList";
import { NoteListRow } from "../components/NoteListRow";

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
        .sort((a, b) => b.created_at - a.created_at),
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
                    <NoteListRow key={n.id} note={n} />
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
