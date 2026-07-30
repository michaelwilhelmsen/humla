import { useEffect, useState, type ReactNode } from "react";
import { Pencil, Trash2, Plus } from "lucide-react";
import {
  Menu,
  MenuContent,
  MenuItem,
  MenuRadioGroup,
  MenuRadioItem,
  MenuTrigger,
} from "./ui/Menu";

// A single reusable selectable-popover primitive (issue #43): a quiet trigger
// that opens a menu of options with a checkmark on the active one. Optionally
// editable — pass onCreate/onRename/onDelete to get an inline "new" row plus
// per-row rename/delete affordances (the Client picker). Leave those off for a
// plain checkmark menu (the chat Scope popover, #47).
//
// Since #114 this is a thin composition over the shared `Menu` primitive:
// Radix owns the portal, collision-aware placement, dismissal, focus return and
// — new here — arrow-key roving and typeahead over the rows. The public shape is
// unchanged, so ChatPanel / ChatHistoryControls / Note are untouched.

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
   * trigger's left edge. "end" pins its right edge to the trigger's right edge,
   * so a right-aligned trigger opens leftward instead of overflowing (#62).
   */
  align?: "start" | "end";
};

// Radix radio values are strings, so the "none" row needs a sentinel that no
// real item id can collide with.
const NONE = "__none__";

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

  // Reset transient edit state whenever the menu closes.
  useEffect(() => {
    if (!open) {
      setEditingId(null);
      setCreating(false);
      setDraft("");
    }
  }, [open]);

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

  // Typed keys must not reach the menu, which would read them as typeahead and
  // move focus off the input mid-word.
  const inputClass = "w-full px-2.5 py-1.5 rounded-md text-sm bg-[var(--color-pill-hover)] outline-none";

  return (
    <Menu open={open} onOpenChange={setOpen}>
      <MenuTrigger aria-label={ariaLabel} className="inline-flex items-center">
        {trigger}
      </MenuTrigger>
      <MenuContent
        align={align}
        className="min-w-[max(var(--radix-dropdown-menu-trigger-width),12.5rem)]"
        // An in-progress rename/create owns Escape: it cancels the edit and
        // leaves the menu open, matching the pre-Radix behaviour.
        onEscapeKeyDown={(e) => {
          if (!editingId && !creating) return;
          e.preventDefault();
          setEditingId(null);
          setCreating(false);
          setDraft("");
        }}
      >
        <MenuRadioGroup
          value={activeId ?? NONE}
          onValueChange={(value) => onSelect(value === NONE ? null : value)}
        >
          {noneLabel && (
            <MenuRadioItem value={NONE}>
              <span className="truncate">{noneLabel}</span>
            </MenuRadioItem>
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
                    e.stopPropagation();
                    if (e.key === "Enter") void commitRename(item.id);
                  }}
                  onBlur={() => void commitRename(item.id)}
                  aria-label="Rename"
                  className={inputClass}
                />
              );
            }
            return (
              <div key={item.id} className="group flex items-center">
                <MenuRadioItem value={item.id} className="flex-1 min-w-0">
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
                </MenuRadioItem>
                {/*
                  Hover-revealed and mouse-only: a Radix menu swallows Tab (the
                  ARIA menu model exits the menu on Tab), so nothing inside a row
                  but the row itself is keyboard-reachable. That matches what
                  shipped before — these buttons only sat in the tab order by
                  accident of the portal being last in <body> — but it is a real
                  gap, and the honest fix is a per-row submenu rather than
                  anything this refactor should smuggle in.
                */}
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
        </MenuRadioGroup>

        {onCreate &&
          (creating ? (
            <input
              autoFocus
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              onKeyDown={(e) => {
                e.stopPropagation();
                if (e.key === "Enter") void commitCreate();
              }}
              onBlur={() => void commitCreate()}
              placeholder={createPlaceholder}
              aria-label={createPlaceholder}
              className={inputClass + " mt-0.5"}
            />
          ) : (
            <MenuItem
              className="mt-0.5 border-t border-[var(--color-line)] rounded-none pt-2"
              // Swap the row for its input rather than closing the menu.
              onSelect={(e) => {
                e.preventDefault();
                setCreating(true);
                setDraft("");
              }}
            >
              <Plus size={14} strokeWidth={1.8} className="shrink-0" />
              <span className="truncate">{createLabel}</span>
            </MenuItem>
          ))}
      </MenuContent>
    </Menu>
  );
}
