import { type MouseEvent, type KeyboardEvent, useRef } from "react";
import { Link } from "react-router-dom";
import { Building2, Check, Folder as FolderIcon } from "lucide-react";
import { type Client, type Folder, type Note } from "../lib/ipc";
import { formatNoteDate, noteExcerpt, noteState, type NoteState } from "../lib/noteList";
import { cn } from "../lib/cn";

// Selection intent reported to the parent. Only the shift flag matters (range
// vs toggle); works for a modifier-click, the checkbox, and the keyboard toggle.
export type SelectIntent = { shiftKey: boolean };

// The card carries no legend, so the colour is named rather than left to
// stand on its own.
const STATE: Record<NoteState, { label: string; color: string }> = {
  summarized: { label: "Summarized", color: "var(--color-accent-text)" },
  recorded: { label: "Recorded", color: "var(--color-interactive)" },
  notes: { label: "Notes only", color: "var(--color-text-muted)" },
  empty: { label: "Empty", color: "var(--color-text-disabled)" },
};

// One note in a grid view (All notes, Folder). Nothing above the grid says
// when, so the card carries its own full date.
//
// A modifier-click selects instead of navigating: Cmd toggles, Shift extends a
// range. Ctrl is excluded — on macOS ctrl-click is the OS secondary click, so
// treating it as a selection modifier triple-fires. The checkbox is a sibling
// of the <Link>, never nested inside the anchor, so toggling it cannot
// navigate; Space on a focused card toggles too, leaving Enter to the link.
export function NoteCard({
  note,
  folder,
  client,
  showFolder = true,
  selected = false,
  selectionActive = false,
  onSelect,
}: {
  note: Note;
  folder?: Folder;
  client?: Client;
  /** False inside a folder view, where the note's folder is the view itself. */
  showFolder?: boolean;
  selected?: boolean;
  /** True when any note in the view is selected. Forces every checkbox visible. */
  selectionActive?: boolean;
  onSelect?: (e: SelectIntent) => void;
}) {
  const title = note.title.trim() || "Untitled";
  const excerpt = noteExcerpt(note);
  const state = STATE[noteState(note)];
  const checkboxShown = selected || selectionActive;
  // The checkbox's toggle fires via onChange (which carries no modifier flags),
  // so we stash the click's shift state here for the onChange to read.
  const shiftRef = useRef(false);

  function handleClick(e: MouseEvent) {
    if (onSelect && (e.metaKey || e.shiftKey)) {
      e.preventDefault();
      onSelect({ shiftKey: e.shiftKey });
    }
  }

  function handleKeyDown(e: KeyboardEvent) {
    if (onSelect && e.key === " ") {
      e.preventDefault();
      onSelect({ shiftKey: e.shiftKey });
    }
  }

  function handleCheckboxClick(e: MouseEvent) {
    shiftRef.current = e.shiftKey;
  }

  function handleCheckboxChange() {
    onSelect?.({ shiftKey: shiftRef.current });
    shiftRef.current = false;
  }

  return (
    <li className="nd-notecard group relative" data-selected={selected ? "true" : undefined}>
      <Link
        to={`/note/${note.id}`}
        onClick={handleClick}
        onKeyDown={handleKeyDown}
        aria-selected={selected}
        data-selected={selected ? "true" : undefined}
        className="flex-1 flex flex-col min-w-0 focus:outline-none"
      >
        <div className="text-[11.5px] text-[var(--color-text-disabled)] tabular-nums">
          {formatNoteDate(note.created_at)}
        </div>
        {/* Room for the checkbox in the corner, so a long title can't run under it. */}
        <h3 className="mt-1.5 pr-7 text-[16.5px] font-semibold leading-[1.28] tracking-[-0.015em] text-[var(--color-text)] line-clamp-2">
          {title}
        </h3>
        {excerpt && (
          <p className="mt-2 text-[13.5px] leading-[1.55] text-[var(--color-text-muted)] line-clamp-4">
            {excerpt}
          </p>
        )}
        <div className="mt-auto pt-4 flex items-center gap-x-2.5 gap-y-1.5 flex-wrap text-[11.5px]">
          <span
            className="inline-flex items-center gap-1.5 whitespace-nowrap"
            style={{ color: state.color }}
          >
            <span
              aria-hidden
              className="w-[6px] h-[6px] rounded-full shrink-0"
              style={{ background: state.color }}
            />
            {state.label}
          </span>
          {client && (
            <span className="inline-flex items-center gap-1.5 min-w-0 text-[var(--color-text-muted)]">
              <Building2 size={12} strokeWidth={1.7} aria-hidden className="shrink-0 opacity-70" />
              <span className="truncate max-w-[9rem]">{client.name}</span>
            </span>
          )}
          {showFolder && folder && (
            <span className="inline-flex items-center gap-1.5 min-w-0 text-[var(--color-text-muted)]">
              <FolderIcon size={12} strokeWidth={1.7} aria-hidden className="shrink-0 opacity-70" />
              <span className="truncate max-w-[9rem]">{folder.name}</span>
            </span>
          )}
        </div>
      </Link>

      {onSelect && (
        // The native input is a transparent overlay filling a ~26px hit target;
        // the ring and check are painted on top and are pointer-transparent so
        // clicks reach it.
        <div
          className={cn(
            "absolute top-3.5 right-3.5 w-[26px] h-[26px] grid place-items-center transition-opacity duration-150",
            checkboxShown
              ? "opacity-100"
              : "opacity-0 group-hover:opacity-100 focus-within:opacity-100",
          )}
        >
          <input
            type="checkbox"
            checked={selected}
            aria-label={`Select ${title}`}
            data-shown={checkboxShown ? "true" : "false"}
            onClick={handleCheckboxClick}
            onChange={handleCheckboxChange}
            className="peer absolute inset-0 m-0 w-full h-full appearance-none opacity-0 cursor-pointer"
          />
          <span
            aria-hidden
            className={cn(
              "w-[18px] h-[18px] rounded-full border transition-colors pointer-events-none",
              "border-[var(--color-line-visible)] bg-[var(--color-surface)] peer-hover:border-[var(--color-text-muted)]",
              "peer-checked:border-[var(--color-accent-text)] peer-checked:bg-[var(--color-accent-text)]",
              "peer-focus-visible:ring-2 peer-focus-visible:ring-[var(--color-accent-text)] peer-focus-visible:ring-offset-1 peer-focus-visible:ring-offset-[var(--color-surface)]",
            )}
          />
          <Check
            aria-hidden
            size={12}
            strokeWidth={3}
            className="absolute opacity-0 peer-checked:opacity-100 text-[var(--color-canvas)] pointer-events-none"
          />
        </div>
      )}
    </li>
  );
}
