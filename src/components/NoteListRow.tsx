import { Link } from "react-router-dom";
import { Folder as FolderIcon } from "lucide-react";
import { type Folder, type Note } from "../lib/ipc";
import { formatMeetingTime, notePreview } from "../lib/noteList";

// One row in a note-list view (All notes, Folder). Title + one-line snippet on
// the left; updated-time and an optional folder chip on the right. The folder
// chip is omitted inside a folder view (where it would be redundant).
export function NoteListRow({ note, folder }: { note: Note; folder?: Folder }) {
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
