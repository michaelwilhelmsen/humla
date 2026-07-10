import { type MouseEvent, type KeyboardEvent, useRef } from "react";
import { Link } from "react-router-dom";
import { Folder as FolderIcon } from "lucide-react";
import { type Folder, type Note } from "../lib/ipc";
import { formatMeetingTime, notePreview } from "../lib/noteList";
import { cn } from "../lib/cn";

// Selection intent reported to the parent. Only the shift flag matters (range
// vs toggle); works for a modifier-click, the checkbox, and the keyboard toggle.
export type SelectIntent = { shiftKey: boolean };

// One row in a note-list view (All notes, Folder). Title + one-line snippet on
// the left; created-time and an optional folder chip on the right. The folder
// chip is omitted inside a folder view (where it would be redundant).
//
// Selection (issue #19): a modifier-click (Cmd to toggle, Shift for a range)
// is intercepted for multi-select and must NOT navigate; a plain click still
// follows the link. Ctrl is deliberately excluded: on macOS ctrl-click is the
// OS secondary/context-menu click, so treating it as a selection modifier
// triple-fired (nav suppression + toggle + native context menu).
//
// Discoverability: a checkbox on the left is the visible affordance. It's
// hidden at rest and fades in on row hover (Tailwind group-hover); it stays
// shown for a selected row and, once ANY row is selected (`selectionActive`),
// for every row — so continuing to pick is obvious. The checkbox is a sibling
// of the <Link> (not nested inside the anchor) so toggling it never navigates.
// Shift+checkbox extends a range, exactly like shift-click. The Cmd/Shift-click
// and Space shortcuts stay as the power-user paths.
//
// A11y: the checkbox is a real focusable control with an accessible name
// (`Select <title>`) and reflects checked state. Selection is also exposed via
// `aria-selected` on the row (not colour alone), and is reachable from the
// keyboard — Space on a focused row toggles it (Shift+Space extends a range),
// while Enter stays as the link's native navigate.
export function NoteListRow({
  note,
  folder,
  selected = false,
  selectionActive = false,
  onSelect,
}: {
  note: Note;
  folder?: Folder;
  selected?: boolean;
  // True when any note in the list is selected. Forces every row's checkbox
  // visible (not just on hover) so continuing to pick is obvious.
  selectionActive?: boolean;
  onSelect?: (e: SelectIntent) => void;
}) {
  const preview = notePreview(note);
  const title = note.title.trim() || "Untitled";
  // Force the checkbox visible when the row is selected or selection mode is on;
  // otherwise it's hover-only (fades in via group-hover / on focus).
  const checkboxShown = selected || selectionActive;
  // The checkbox's toggle fires via onChange (which carries no modifier flags),
  // so we stash the click's shift state here for the onChange to read.
  const shiftRef = useRef(false);

  function handleClick(e: MouseEvent) {
    if (onSelect && (e.metaKey || e.shiftKey)) {
      e.preventDefault(); // suppress navigation for modifier-clicks
      onSelect({ shiftKey: e.shiftKey });
    }
  }

  function handleKeyDown(e: KeyboardEvent) {
    // Space toggles selection instead of scrolling / activating the link;
    // Shift+Space extends the range. Enter is left to the link's native
    // navigation so keyboard users can still open a note.
    if (onSelect && e.key === " ") {
      e.preventDefault();
      onSelect({ shiftKey: e.shiftKey });
    }
  }

  function handleCheckboxClick(e: MouseEvent) {
    // Capture the modifier for the onChange that follows; a shift-click on the
    // checkbox should extend the range just like a shift-click on the row.
    shiftRef.current = e.shiftKey;
  }

  function handleCheckboxChange() {
    onSelect?.({ shiftKey: shiftRef.current });
    shiftRef.current = false;
  }

  return (
    <li>
      {/* `group` here (not on the Link) so hovering anywhere in the row — the
          checkbox included — fades the checkbox in. */}
      <div className="group max-w-[880px] mx-auto w-full px-8">
        <div
          className={cn(
            "flex items-start p-3 rounded-[11px] transition-colors",
            selected
              ? "bg-[var(--color-accent-soft)]"
              : "hover:bg-[var(--color-pill-hover)]",
          )}
        >
          {onSelect && (
            // Zero-width slot at rest so the title sits flush-left; it grows
            // (and the checkbox fades in) on row hover / focus, and stays open
            // once the row is selected or selection mode is on. `w-7` matches
            // the checkbox (w-4) plus the trailing gap the row used to reserve.
            <div
              className={cn(
                "shrink-0 overflow-hidden transition-[width,opacity] duration-150 ease-out",
                checkboxShown
                  ? "w-7 opacity-100"
                  : "w-0 opacity-0 group-hover:w-7 group-hover:opacity-100 focus-within:w-7 focus-within:opacity-100",
              )}
            >
              <input
                type="checkbox"
                checked={selected}
                aria-label={`Select ${title}`}
                data-shown={checkboxShown ? "true" : "false"}
                onClick={handleCheckboxClick}
                onChange={handleCheckboxChange}
                className="shrink-0 mt-0.5 w-4 h-4 rounded-[5px] cursor-pointer accent-[var(--color-accent-text)]"
              />
            </div>
          )}
          <Link
            to={`/note/${note.id}`}
            onClick={handleClick}
            onKeyDown={handleKeyDown}
            aria-selected={selected}
            data-selected={selected ? "true" : undefined}
            className="flex-1 min-w-0 flex items-start gap-3"
          >
            <div className="flex-1 min-w-0">
              <div className="text-[14.5px] font-medium text-[var(--color-text)] truncate">
                {title}
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
          </Link>
        </div>
      </div>
    </li>
  );
}
