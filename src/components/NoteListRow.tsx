import { type MouseEvent } from "react";
import { Link } from "react-router-dom";
import { Folder as FolderIcon } from "lucide-react";
import { type Folder, type Note } from "../lib/ipc";
import { formatMeetingTime, notePreview } from "../lib/noteList";
import { cn } from "../lib/cn";

// One row in a note-list view (All notes, Folder). Title + one-line snippet on
// the left; created-time and an optional folder chip on the right. The folder
// chip is omitted inside a folder view (where it would be redundant).
//
// Selection (issue #19): a modifier-click (Cmd/Ctrl to toggle, Shift for a
// range) is intercepted for multi-select and must NOT navigate; a plain click
// still follows the link.
export function NoteListRow({
  note,
  folder,
  selected = false,
  onSelect,
}: {
  note: Note;
  folder?: Folder;
  selected?: boolean;
  onSelect?: (e: MouseEvent) => void;
}) {
  const preview = notePreview(note);

  function handleClick(e: MouseEvent) {
    if (onSelect && (e.metaKey || e.ctrlKey || e.shiftKey)) {
      e.preventDefault(); // suppress navigation for modifier-clicks
      onSelect(e);
    }
  }

  return (
    <li>
      <Link
        to={`/note/${note.id}`}
        onClick={handleClick}
        data-selected={selected ? "true" : undefined}
        className="group block"
      >
        <div className="max-w-[880px] mx-auto w-full px-8">
          <div
            className={cn(
              "flex items-start gap-3 p-3 rounded-[11px] transition-colors",
              selected
                ? "bg-[var(--color-accent-soft)]"
                : "hover:bg-[var(--color-pill-hover)]",
            )}
          >
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
                {formatMeetingTime(note.created_at)}
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
