import { useEffect, useRef, useState } from "react";
import { FolderInput, Trash2, X } from "lucide-react";
import { type Folder } from "../lib/ipc";

// Slim bottom action bar for multi-select in the note lists (issue #19).
// Appears once 2+ notes are selected. The folder picker is a small
// purpose-built popover — the note-toolbar's FolderPicker is fused to that
// toolbar's chrome (and its inline "+ new folder" creation), so a dedicated
// list reads cleaner here than an extraction.
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
  const [pickerOpen, setPickerOpen] = useState(false);
  const pickerRef = useRef<HTMLDivElement>(null);

  // Click-away closes the folder popover (Esc is handled by the parent's
  // clear-selection listener, which also tears down the whole bar).
  useEffect(() => {
    if (!pickerOpen) return;
    const onDown = (e: MouseEvent) => {
      if (pickerRef.current && !pickerRef.current.contains(e.target as Node)) {
        setPickerOpen(false);
      }
    };
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, [pickerOpen]);

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

        <div ref={pickerRef} className="relative">
          <button
            onClick={() => setPickerOpen((o) => !o)}
            disabled={busy}
            aria-haspopup="true"
            aria-expanded={pickerOpen}
            className="inline-flex items-center gap-1.5 px-3 py-1.5 rounded-full text-[13px] text-[var(--color-text)] hover:bg-[var(--color-pill-hover)] transition-colors disabled:opacity-50"
          >
            <FolderInput size={14} strokeWidth={1.7} />
            Move to folder
          </button>
          {pickerOpen && (
            // Plain list of focusable buttons rather than a role="menu": they're
            // already Tab-reachable and honest about being buttons. A false
            // ARIA-menu contract would promise arrow-key roving we don't wire.
            <div
              className="absolute bottom-full mb-2 left-1/2 -translate-x-1/2 min-w-[12rem] max-h-64 overflow-y-auto py-1 rounded-lg bg-[var(--color-canvas)] border border-[var(--color-line-visible)] shadow-xl"
            >
              <button
                onClick={() => {
                  setPickerOpen(false);
                  onMove(null);
                }}
                className="block w-full text-left px-3 py-1.5 text-[13px] text-[var(--color-text-muted)] hover:bg-[var(--color-pill-hover)] hover:text-[var(--color-text)] transition-colors"
              >
                No folder
              </button>
              {folders.map((f) => (
                <button
                  key={f.id}
                  onClick={() => {
                    setPickerOpen(false);
                    onMove(f.id);
                  }}
                  className="block w-full text-left px-3 py-1.5 text-[13px] text-[var(--color-text)] hover:bg-[var(--color-pill-hover)] transition-colors truncate"
                >
                  {f.name}
                </button>
              ))}
            </div>
          )}
        </div>

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
