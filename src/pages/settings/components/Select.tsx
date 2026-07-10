import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { Check, ChevronDown } from "lucide-react";

// Compact popover select (Claude-desktop style): a quiet trigger showing the
// current value + chevron, opening a styled listbox with a check on the
// selected row. Same public API as the old native <select> wrapper.
//
// The listbox portals out of its in-flow position (same idiom as
// ContextMenu/Modal) rather than rendering as an absolutely-positioned child
// of the trigger. Several callers (e.g. ImportDialog, SettingsDialog) place
// this inside a dialog whose *content panel* is `overflow-y-auto` /
// `overflow-hidden` so long settings content can scroll — that overflow
// clips any in-flow absolutely-positioned child, cutting off the listbox
// when it opens near the panel's edge. The dialog's outer `role="dialog"`
// wrapper (both Modal.tsx and SettingsDialog.tsx) is itself unclipped, so we
// portal into the nearest `[role="dialog"]` ancestor when there is one —
// that escapes the panel's clip while staying inside `within(dialog)`
// queries in tests. Falls back to <body> when there's no dialog ancestor.
export function Select({
  value,
  onChange,
  options,
  id,
}: {
  value: string;
  onChange: (v: string) => void;
  options: { value: string; label: string }[];
  // Optional id placed on the trigger <button> (a labelable element) so an
  // external <label htmlFor> can name this control.
  id?: string;
}) {
  const [open, setOpen] = useState(false);
  // Wraps only the trigger button now that the listbox portals elsewhere —
  // used both as the "is this an outside click" boundary and as the anchor
  // whose position we measure for the floating listbox.
  const rootRef = useRef<HTMLDivElement>(null);
  const popoverRef = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<{
    top?: number;
    bottom?: number;
    right: number;
    minWidth: number;
    maxHeight: number;
  } | null>(null);
  const current = options.find((o) => o.value === value);

  // Measure the trigger and place the listbox in fixed coordinates, flipping
  // above the trigger when there isn't enough room below (e.g. opened near
  // the bottom edge of a modal). Right-aligned to the trigger's right edge
  // to match the previous in-flow "right-0" positioning.
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
    const preferredMax = 256; // matches the old max-h-64
    const spaceBelow = window.innerHeight - rect.bottom - gap - edgeMargin;
    const spaceAbove = rect.top - gap - edgeMargin;
    const right = window.innerWidth - rect.right;
    const minWidth = rect.width;

    if (spaceBelow >= Math.min(preferredMax, 150) || spaceBelow >= spaceAbove) {
      setPos({
        top: rect.bottom + gap,
        right,
        minWidth,
        maxHeight: Math.max(120, Math.min(preferredMax, spaceBelow)),
      });
    } else {
      setPos({
        bottom: window.innerHeight - rect.top + gap,
        right,
        minWidth,
        maxHeight: Math.max(120, Math.min(preferredMax, spaceAbove)),
      });
    }
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      const target = e.target as Node;
      // Contained in either the trigger or the (portaled) listbox counts as
      // "inside" — the listbox no longer lives under rootRef in the DOM.
      if (rootRef.current?.contains(target) || popoverRef.current?.contains(target)) {
        return;
      }
      setOpen(false);
      // Swallow the click this mousedown produces: dismissing the popover
      // must not also activate whatever is underneath (the dialog backdrop
      // would close all of settings). Matches native macOS menu behavior.
      document.addEventListener(
        "click",
        (ce) => ce.stopPropagation(),
        { capture: true, once: true },
      );
    };
    // Capture phase + stopPropagation: Escape dismisses the listbox only —
    // it must never bubble to the settings dialog's window listener and
    // close the whole dialog underneath the user.
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        setOpen(false);
      }
    };
    // The listbox is now `position: fixed` from a one-time measurement, so
    // it goes stale if an ancestor scrolls (e.g. the modal panel itself).
    // Close on any scroll except one that originates inside the listbox's
    // own internal scroll (long options lists). Capture phase: `scroll`
    // doesn't bubble, so listening on document only sees it if capturing.
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
  }, [open]);

  return (
    <div ref={rootRef} className="relative inline-block">
      <button
        type="button"
        id={id}
        aria-haspopup="listbox"
        aria-expanded={open}
        onClick={() => setOpen((o) => !o)}
        className="inline-flex items-center gap-1.5 max-w-[260px] px-2.5 py-1.5 rounded-md text-sm border border-[var(--color-line-visible)] bg-[var(--color-surface)] hover:bg-[var(--color-pill-hover)] transition-colors"
      >
        <span className="truncate">{current?.label ?? value}</span>
        <ChevronDown
          size={13}
          strokeWidth={1.8}
          className={
            "shrink-0 text-[var(--color-text-muted)] transition-transform " +
            (open ? "rotate-180" : "")
          }
          aria-hidden
        />
      </button>
      {open &&
        pos &&
        createPortal(
          <div
            ref={popoverRef}
            role="listbox"
            style={{
              position: "fixed",
              top: pos.top,
              bottom: pos.bottom,
              right: pos.right,
              minWidth: pos.minWidth,
              maxHeight: pos.maxHeight,
            }}
            className="z-50 w-max max-w-[300px] overflow-y-auto rounded-lg border border-[var(--color-line-visible)] bg-[var(--color-canvas)] shadow-lg p-1"
          >
            {options.map((o) => {
              const selected = o.value === value;
              return (
                <button
                  key={o.value}
                  type="button"
                  role="option"
                  aria-selected={selected}
                  ref={(node) => {
                    // Long lists (languages) open with the current value in
                    // view. Scroll ONLY the listbox — scrollIntoView would
                    // also scroll whatever ancestor happens to be scrollable.
                    if (!selected || !node) return;
                    const lb = node.closest('[role="listbox"]');
                    if (lb instanceof HTMLElement) {
                      lb.scrollTop =
                        node.offsetTop - lb.clientHeight / 2 + node.clientHeight / 2;
                    }
                  }}
                  onClick={() => {
                    onChange(o.value);
                    setOpen(false);
                  }}
                  className={
                    "flex w-full items-center justify-between gap-3 px-2.5 py-1.5 rounded-md text-sm text-left transition-colors " +
                    (selected
                      ? "text-[var(--color-text)]"
                      : "text-[var(--color-text-muted)] hover:text-[var(--color-text)]") +
                    " hover:bg-[var(--color-pill-hover)]"
                  }
                >
                  <span className="truncate">{o.label}</span>
                  {selected && (
                    <Check
                      size={14}
                      strokeWidth={2}
                      className="shrink-0 text-[var(--color-accent-text)]"
                      aria-hidden
                    />
                  )}
                </button>
              );
            })}
          </div>,
          // Nearest dialog wrapper if there is one (escapes its content
          // panel's clip while staying "within" it for tests/AT), else body.
          rootRef.current?.closest('[role="dialog"]') ?? document.body,
        )}
    </div>
  );
}
