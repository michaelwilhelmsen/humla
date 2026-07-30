import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import type { ComponentProps } from "react";
import { Check } from "lucide-react";
import { cn } from "../../lib/cn";
import { floatingSurface, rowClass, rowHighlightClass } from "./surface";

// Humla's menu primitive: Radix `DropdownMenu` with our tokens on it (#114).
//
// Radix owns the parts that were previously hand-rolled six times over and
// wrong in a different way each time — portalling, collision-aware placement,
// dismiss on outside pointerdown / Escape, ARIA wiring, focus return to the
// trigger, arrow-key roving and typeahead. This file only dresses it.
//
// Two deliberate defaults:
//   * `modal={false}` — a modal Radix menu sets `pointer-events: none` on the
//     body and aria-hides the rest of the page. None of these menus wants
//     that: several sit inside the settings dialog, and the note context menu
//     must not freeze the page behind it.
//   * `loop` — arrow keys wrap. The one behaviour SpeakerLabels hand-wrote and
//     every other copy lacked entirely; wrapping is now the house default.

/** Root. Controlled (`open`/`onOpenChange`) or uncontrolled. */
export function Menu({ modal = false, ...props }: ComponentProps<typeof DropdownMenu.Root>) {
  return <DropdownMenu.Root modal={modal} {...props} />;
}

export const MenuTrigger = DropdownMenu.Trigger;

export function MenuContent({
  className,
  style,
  maxHeight = 280,
  container,
  ...props
}: ComponentProps<typeof DropdownMenu.Content> & {
  /** Ceiling in px; the surface shrinks further near a viewport edge. */
  maxHeight?: number;
  /** Portal target. Defaults to `<body>`. */
  container?: HTMLElement | null;
}) {
  return (
    <DropdownMenu.Portal container={container ?? undefined}>
      <DropdownMenu.Content
        align="start"
        loop
        {...floatingSurface({ radixPrefix: "dropdown-menu", maxHeight, className, style })}
        {...props}
      />
    </DropdownMenu.Portal>
  );
}

export function MenuItem({
  className,
  danger,
  ...props
}: ComponentProps<typeof DropdownMenu.Item> & { danger?: boolean }) {
  return (
    <DropdownMenu.Item
      className={cn(
        rowClass,
        danger
          ? "text-[var(--color-danger)] data-[highlighted]:bg-[var(--color-pill-hover)]"
          : rowHighlightClass,
        className,
      )}
      {...props}
    />
  );
}

export const MenuRadioGroup = DropdownMenu.RadioGroup;

/**
 * A one-of-many row. Renders a fixed-width check slot so labels stay aligned
 * whether or not the row is the selected one.
 */
export function MenuRadioItem({
  className,
  children,
  ...props
}: ComponentProps<typeof DropdownMenu.RadioItem>) {
  return (
    <DropdownMenu.RadioItem
      className={cn(rowClass, rowHighlightClass, "data-[state=checked]:text-[var(--color-text)]", className)}
      {...props}
    >
      <span className="grid h-3.5 w-3.5 shrink-0 place-items-center" aria-hidden>
        <DropdownMenu.ItemIndicator>
          <Check size={14} strokeWidth={2} className="text-[var(--color-accent-text)]" />
        </DropdownMenu.ItemIndicator>
      </span>
      {children}
    </DropdownMenu.RadioItem>
  );
}

export function MenuLabel({ className, ...props }: ComponentProps<typeof DropdownMenu.Label>) {
  return <DropdownMenu.Label className={cn("nd-label px-2 py-1", className)} {...props} />;
}

export function MenuSeparator({
  className,
  ...props
}: ComponentProps<typeof DropdownMenu.Separator>) {
  return (
    <DropdownMenu.Separator
      className={cn("my-1 border-t border-[var(--color-line)]", className)}
      {...props}
    />
  );
}
