import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { Check, Pencil, Trash2, Plus } from "lucide-react";

// A single reusable selectable-popover primitive (issue #43): a quiet trigger
// that opens a menu of options with a checkmark on the active one. Optionally
// editable — pass onCreate/onRename/onDelete to get an inline "new" row plus
// per-row rename/delete affordances (the Client picker). Leave those off for a
// plain checkmark menu (intended for reuse by the chat Scope popover, #47).
//
// Portal/positioning/dismiss mechanics mirror settings' Select.tsx: the menu
// is `position: fixed` off a one-time measurement of the trigger, flips above
// when there's no room below, and closes on outside-click / Escape / scroll.
// No new UI dependency.

export type PopoverItem = {
  id: string;
  label: string;
  /** Optional muted second line under the label (e.g. a relative date, #62). */
  description?: string;
  /** Optional leading icon rendered before the label (e.g. a scope glyph, #69). */
  icon?: ReactNode;
};

type Props = {
  trigger: ReactNode;
  items: PopoverItem[];
  /** Active item id, or null when the "none" row is selected. */
  activeId: string | null;
  onSelect: (id: string | null) => void;
  /** Label for a leading "unassign" row (e.g. "No client"). Omit to hide it. */
  noneLabel?: string;
  /** Enable the inline create row. */
  onCreate?: (name: string) => void | Promise<void>;
  onRename?: (id: string, name: string) => void | Promise<void>;
  onDelete?: (id: string) => void | Promise<void>;
  createLabel?: string;
  createPlaceholder?: string;
  /** Accessible name for the trigger. */
  ariaLabel?: string;
  /**
   * Horizontal anchor. "start" (default) pins the menu's left edge to the
   * trigger's left edge (existing behavior). "end" pins its right edge to the
   * trigger's right edge, so a right-aligned trigger opens leftward instead of
   * overflowing the viewport (#62).
   */
  align?: "start" | "end";
};

export function SelectablePopover({
  trigger,
  items,
  activeId,
  onSelect,
  noneLabel,
  onCreate,
  onRename,
  onDelete,
  createLabel = "New",
  createPlaceholder = "Name",
  ariaLabel,
  align = "start",
}: Props) {
  const [open, setOpen] = useState(false);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [draft, setDraft] = useState("");
  const rootRef = useRef<HTMLDivElement>(null);
  const popoverRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const [pos, setPos] = useState<{
    top?: number;
    bottom?: number;
    // Exactly one of left/right is set, per `align`.
    left?: number;
    right?: number;
    minWidth: number;
    maxHeight: number;
  } | null>(null);

  // Reset transient edit state whenever the menu closes.
  useEffect(() => {
    if (!open) {
      setEditingId(null);
      setCreating(false);
      setDraft("");
    }
  }, [open]);

  useLayoutEffect(() => {
    if (!open) {
      setPos(null);
      return;
    }
    const trigger = rootRef.current;
    if (!trigger) return;
    const rect = trigger.getBoundingClientRect();
    const gap = 4;
    const edgeMargin = 8;
    const preferredMax = 280;
    const spaceBelow = window.innerHeight - rect.bottom - gap - edgeMargin;
    const spaceAbove = rect.top - gap - edgeMargin;
    // Anchor the left edge to the trigger's left ("start") or the right edge to
    // the trigger's right ("end") so a right-aligned trigger opens leftward.
    const anchor =
      align === "end" ? { right: window.innerWidth - rect.right } : { left: rect.left };
    const minWidth = Math.max(rect.width, 200);
    if (spaceBelow >= Math.min(preferredMax, 150) || spaceBelow >= spaceAbove) {
      setPos({ ...anchor, top: rect.bottom + gap, minWidth, maxHeight: Math.max(120, Math.min(preferredMax, spaceBelow)) });
    } else {
      setPos({ ...anchor, bottom: window.innerHeight - rect.top + gap, minWidth, maxHeight: Math.max(120, Math.min(preferredMax, spaceAbove)) });
    }
  }, [open, align]);

  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      const target = e.target as Node;
      if (rootRef.current?.contains(target) || popoverRef.current?.contains(target)) return;
      setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        // Escape cancels an in-progress edit first, then closes the menu.
        if (editingId || creating) {
          setEditingId(null);
          setCreating(false);
          setDraft("");
        } else {
          closeAndRestore();
        }
      }
    };
    const onScroll = (e: Event) => {
      if (popoverRef.current?.contains(e.target as Node)) return;
      setOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey, true);
    document.addEventListener("scroll", onScroll, true);
    window.addEventListener("resize", onScroll);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey, true);
      document.removeEventListener("scroll", onScroll, true);
      window.removeEventListener("resize", onScroll);
    };
  }, [open, editingId, creating]);

  // Close and return focus to the trigger. Used only for deliberate,
  // keyboard-reachable closes (item select, Escape) so keyboard users don't drop
  // to <body> when the menu unmounts (#64). Incidental closes — click-away and
  // scroll — use plain setOpen(false) so they don't yank focus from a mouse user
  // who clicked elsewhere.
  const closeAndRestore = () => {
    setOpen(false);
    triggerRef.current?.focus();
  };

  const editable = !!(onCreate || onRename || onDelete);

  async function commitCreate() {
    const name = draft.trim();
    if (!name || !onCreate) {
      setCreating(false);
      setDraft("");
      return;
    }
    await onCreate(name);
    setCreating(false);
    setDraft("");
    setOpen(false);
  }

  async function commitRename(id: string) {
    const name = draft.trim();
    if (name && onRename) await onRename(id, name);
    setEditingId(null);
    setDraft("");
  }

  const rowBase =
    "flex w-full items-center gap-2 px-2.5 py-1.5 rounded-md text-sm text-left transition-colors";

  return (
    <div ref={rootRef} className="relative inline-flex">
      <button
        ref={triggerRef}
        type="button"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label={ariaLabel}
        onClick={() => setOpen((o) => !o)}
        className="inline-flex items-center"
      >
        {trigger}
      </button>
      {open &&
        pos &&
        createPortal(
          <div
            ref={popoverRef}
            role="menu"
            style={{
              position: "fixed",
              top: pos.top,
              bottom: pos.bottom,
              left: pos.left,
              right: pos.right,
              minWidth: pos.minWidth,
              maxHeight: pos.maxHeight,
            }}
            className="z-50 w-max max-w-[320px] overflow-y-auto rounded-lg border border-[var(--color-line-visible)] bg-[var(--color-canvas)] shadow-lg p-1"
          >
            {noneLabel && (
              <button
                type="button"
                role="menuitemradio"
                aria-checked={activeId === null}
                onClick={() => {
                  onSelect(null);
                  closeAndRestore();
                }}
                className={
                  rowBase +
                  " justify-between " +
                  (activeId === null
                    ? "text-[var(--color-text)]"
                    : "text-[var(--color-text-muted)] hover:text-[var(--color-text)]") +
                  " hover:bg-[var(--color-pill-hover)]"
                }
              >
                <span className="truncate">{noneLabel}</span>
                {activeId === null && (
                  <Check size={14} strokeWidth={2} className="shrink-0 text-[var(--color-accent-text)]" aria-hidden />
                )}
              </button>
            )}

            {items.map((item) => {
              if (editingId === item.id) {
                return (
                  <input
                    key={item.id}
                    autoFocus
                    value={draft}
                    onChange={(e) => setDraft(e.target.value)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter") void commitRename(item.id);
                    }}
                    onBlur={() => void commitRename(item.id)}
                    aria-label="Rename"
                    className="w-full px-2.5 py-1.5 rounded-md text-sm bg-[var(--color-pill-hover)] outline-none"
                  />
                );
              }
              const selected = item.id === activeId;
              return (
                <div key={item.id} className="group flex items-center">
                  <button
                    type="button"
                    role="menuitemradio"
                    aria-checked={selected}
                    onClick={() => {
                      onSelect(item.id);
                      closeAndRestore();
                    }}
                    className={
                      rowBase +
                      " flex-1 min-w-0 " +
                      (selected
                        ? "text-[var(--color-text)]"
                        : "text-[var(--color-text-muted)] hover:text-[var(--color-text)]") +
                      " hover:bg-[var(--color-pill-hover)]"
                    }
                  >
                    <Check
                      size={14}
                      strokeWidth={2}
                      aria-hidden
                      className={"shrink-0 " + (selected ? "text-[var(--color-accent-text)]" : "opacity-0")}
                    />
                    {item.icon && (
                      <span className="shrink-0 inline-flex" aria-hidden="true">
                        {item.icon}
                      </span>
                    )}
                    <span className="flex min-w-0 flex-col">
                      <span className="truncate">{item.label}</span>
                      {item.description && (
                        <span className="truncate text-xs text-[var(--color-text-muted)]">
                          {item.description}
                        </span>
                      )}
                    </span>
                  </button>
                  {editable && (
                    <span className="flex items-center opacity-0 group-hover:opacity-100 transition-opacity pr-1">
                      {onRename && (
                        <button
                          type="button"
                          title="Rename"
                          aria-label={`Rename ${item.label}`}
                          onClick={(e) => {
                            e.stopPropagation();
                            setEditingId(item.id);
                            setDraft(item.label);
                          }}
                          className="p-1 rounded text-[var(--color-text-muted)] hover:text-[var(--color-text)] hover:bg-[var(--color-pill-hover)]"
                        >
                          <Pencil size={13} strokeWidth={1.8} />
                        </button>
                      )}
                      {onDelete && (
                        <button
                          type="button"
                          title="Delete"
                          aria-label={`Delete ${item.label}`}
                          onClick={(e) => {
                            e.stopPropagation();
                            void onDelete(item.id);
                          }}
                          className="p-1 rounded text-[var(--color-text-muted)] hover:text-[var(--color-accent-text)] hover:bg-[var(--color-pill-hover)]"
                        >
                          <Trash2 size={13} strokeWidth={1.8} />
                        </button>
                      )}
                    </span>
                  )}
                </div>
              );
            })}

            {onCreate &&
              (creating ? (
                <input
                  autoFocus
                  value={draft}
                  onChange={(e) => setDraft(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") void commitCreate();
                  }}
                  onBlur={() => void commitCreate()}
                  placeholder={createPlaceholder}
                  aria-label={createPlaceholder}
                  className="w-full px-2.5 py-1.5 rounded-md text-sm bg-[var(--color-pill-hover)] outline-none mt-0.5"
                />
              ) : (
                <button
                  type="button"
                  role="menuitem"
                  onClick={() => {
                    setCreating(true);
                    setDraft("");
                  }}
                  className={
                    rowBase +
                    " mt-0.5 border-t border-[var(--color-line)] rounded-none pt-2 text-[var(--color-text-muted)] hover:text-[var(--color-text)] hover:bg-[var(--color-pill-hover)]"
                  }
                >
                  <Plus size={14} strokeWidth={1.8} className="shrink-0" />
                  <span className="truncate">{createLabel}</span>
                </button>
              ))}
          </div>,
          document.body,
        )}
    </div>
  );
}
