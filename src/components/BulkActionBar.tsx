import { FolderInput, Trash2, X } from "lucide-react";
import { type Folder } from "../lib/ipc";
import { Menu, MenuContent, MenuItem, MenuTrigger } from "./ui/Menu";

// Slim bottom action bar for multi-select in the note lists (issue #19).
// Appears once 2+ notes are selected. The folder picker is the shared `Menu`
// (#114) — it was hand-rolled here, with neither a portal nor arrow keys, and
// opens upward off a bar pinned to the bottom of the window.
export function BulkActionBar({
  count,
  folders,
  busy,
  onDelete,
  onMove,
  onCancel,
}: {
  count: number;
  folders: Folder[];
  busy: boolean;
  onDelete: () => void;
  onMove: (folderId: string | null) => void;
  onCancel: () => void;
}) {
  return (
    <div className="fixed bottom-6 left-1/2 -translate-x-1/2 z-40 no-drag">
      <div className="flex items-center gap-1 pl-4 pr-1.5 py-1.5 rounded-full bg-[var(--color-surface-raised)] border border-[var(--color-line-visible)] shadow-xl">
        <span
          aria-live="polite"
          className="text-[13px] text-[var(--color-text-muted)] tabular-nums whitespace-nowrap pr-1"
        >
          {count} selected
        </span>

        <button
          onClick={onDelete}
          disabled={busy}
          className="inline-flex items-center gap-1.5 px-3 py-1.5 rounded-full text-[13px] text-[var(--color-text)] hover:text-[var(--color-danger)] hover:bg-[var(--color-pill-hover)] transition-colors disabled:opacity-50"
        >
          <Trash2 size={14} strokeWidth={1.7} />
          Delete
        </button>

        <Menu>
          <MenuTrigger
            disabled={busy}
            className="inline-flex items-center gap-1.5 px-3 py-1.5 rounded-full text-[13px] text-[var(--color-text)] hover:bg-[var(--color-pill-hover)] transition-colors disabled:opacity-50"
          >
            <FolderInput size={14} strokeWidth={1.7} />
            Move to folder
          </MenuTrigger>
          <MenuContent
            side="top"
            align="center"
            sideOffset={8}
            maxHeight={256}
            aria-label="Move to folder"
            className="min-w-[12rem] shadow-xl py-1"
          >
            <MenuItem className="px-3 text-[13px]" onSelect={() => onMove(null)}>
              No folder
            </MenuItem>
            {folders.map((f) => (
              <MenuItem
                key={f.id}
                className="px-3 text-[13px] text-[var(--color-text)]"
                onSelect={() => onMove(f.id)}
              >
                <span className="truncate">{f.name}</span>
              </MenuItem>
            ))}
          </MenuContent>
        </Menu>

        <button
          onClick={onCancel}
          disabled={busy}
          aria-label="Cancel"
          className="grid place-items-center w-8 h-8 rounded-full text-[var(--color-text-muted)] hover:text-[var(--color-text)] hover:bg-[var(--color-pill-hover)] transition-colors disabled:opacity-50"
        >
          <X size={15} strokeWidth={1.8} />
        </button>
      </div>
    </div>
  );
}
