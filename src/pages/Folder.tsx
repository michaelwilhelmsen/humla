import { useMemo } from "react";
import { useParams, useNavigate } from "react-router-dom";
import { Folder as FolderIcon } from "lucide-react";
import { type Client } from "../lib/ipc";
import { useNotesStore } from "../lib/store";
import { NoteCard } from "../components/NoteCard";

export function Folder() {
  const { id } = useParams<{ id: string }>();
  const navigate = useNavigate();
  const folders = useNotesStore((s) => s.folders);
  const notes = useNotesStore((s) => s.notes);
  const clients = useNotesStore((s) => s.clients);

  const folder = useMemo(() => folders.find((f) => f.id === id), [folders, id]);
  const folderNotes = useMemo(
    () =>
      notes
        .filter((n) => n.folder_id === id)
        .sort((a, b) => b.created_at - a.created_at),
    [notes, id],
  );
  const clientById = useMemo(() => {
    const map = new Map<string, Client>();
    for (const c of clients) map.set(c.id, c);
    return map;
  }, [clients]);

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
      {/* One scrolling well, matching All notes: the grid runs under a
          translucent title bar. New note + theme toggle live in the TopBar. */}
      <div className="flex-1 overflow-y-auto nd-well">
        <div className="nd-well-bar">
          <div className="max-w-[1180px] mx-auto w-full px-8 pt-14 pb-3">
            <div className="flex items-center gap-3 px-1">
              <span className="shrink-0 grid place-items-center w-8 h-8 rounded-[9px] bg-[var(--color-surface-raised)] border border-[var(--color-line-visible)] text-[var(--color-text-muted)]">
                <FolderIcon size={16} strokeWidth={1.7} />
              </span>
              <h1 className="nd-heading truncate">{folder.name}</h1>
              <span className="text-[14px] text-[var(--color-text-disabled)] tabular-nums shrink-0">{count}</span>
            </div>
          </div>
        </div>

        {count === 0 ? (
          <div className="max-w-[1180px] mx-auto w-full px-8 pt-16 text-center text-sm text-[var(--color-text-muted)]">
            No notes in this folder yet.
          </div>
        ) : (
          <div className="max-w-[1180px] mx-auto w-full px-8 pt-6 pb-24">
            <ul className="nd-notegrid">
              {folderNotes.map((n) => (
                // The folder is the view, so a folder chip on every card would
                // say the same thing as the heading.
                <NoteCard
                  key={n.id}
                  note={n}
                  client={n.client_id ? clientById.get(n.client_id) : undefined}
                  showFolder={false}
                />
              ))}
            </ul>
          </div>
        )}
      </div>
    </div>
  );
}
