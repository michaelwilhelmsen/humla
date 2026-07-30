import { cn } from "../../lib/cn";

// The one floating-surface look shared by every anchored panel in the app
// (#114). Before this, six hand-rolled popovers each re-typed some subset of
// these classes and drifted; now Popover / Menu / Select all render the same
// card, and a call site that genuinely needs a different fill (ContextMenu and
// the speaker merge menu sit on --color-surface, not the canvas) overrides just
// that one class through `cn`.
const surfaceClass =
  "z-50 overflow-y-auto rounded-lg border border-[var(--color-line-visible)] bg-[var(--color-canvas)] shadow-lg p-1";

/** A row inside a floating surface: menu item, option, radio item. */
export const rowClass =
  "flex w-full items-center gap-2 px-2.5 py-1.5 rounded-md text-sm text-left transition-colors cursor-default select-none outline-none";

/**
 * Highlight for the row Radix considers active — hover and keyboard focus are
 * the same visual state, which is what makes arrow-key navigation legible.
 * Radix sets `data-highlighted` on the focused item of a menu or listbox.
 */
export const rowHighlightClass =
  "text-[var(--color-text-muted)] data-[highlighted]:text-[var(--color-text)] data-[highlighted]:bg-[var(--color-pill-hover)]";

/**
 * Placement + dressing shared by the three primitives. Radix keeps its measured
 * sizes in per-primitive custom properties, so each caller passes its own
 * prefix (`dropdown-menu`, `popover`, `select`) and gets the same surface:
 * capped at `maxHeight`, shrinking further near a viewport edge.
 */
export function floatingSurface({
  radixPrefix,
  maxHeight,
  className,
  style,
}: {
  radixPrefix: "dropdown-menu" | "popover" | "select";
  maxHeight: number;
  className?: string;
  style?: React.CSSProperties;
}) {
  const available = `var(--radix-${radixPrefix}-content-available-height, ${maxHeight}px)`;
  return {
    sideOffset: 4,
    collisionPadding: 8,
    className: cn(surfaceClass, "w-max min-w-[10rem] max-w-[320px]", className),
    style: { maxHeight: `min(${maxHeight}px, ${available})`, ...style },
  };
}
