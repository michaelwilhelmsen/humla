import * as RadixPopover from "@radix-ui/react-popover";
import type { ComponentProps } from "react";
import { floatingSurface } from "./surface";

// The plain anchored-panel primitive (#114): a trigger, a portalled surface
// that avoids the viewport edges, dismiss on outside pointerdown / Escape, and
// focus returned to the trigger on close.
//
// `Menu` is the right choice whenever the panel is a list of choices — it adds
// arrow-key roving and typeahead that this one deliberately doesn't have.
// Reach for `Popover` when the content is arbitrary: a form, an input, a
// filtered listbox that owns its own key handling (#116's Combobox), or a
// detail card (#115's Client pin).

export const Popover = RadixPopover.Root;
export const PopoverTrigger = RadixPopover.Trigger;
export const PopoverAnchor = RadixPopover.Anchor;
export const PopoverClose = RadixPopover.Close;

export function PopoverContent({
  className,
  style,
  maxHeight = 280,
  container,
  ...props
}: ComponentProps<typeof RadixPopover.Content> & {
  /** Ceiling in px; the surface shrinks further near a viewport edge. */
  maxHeight?: number;
  /** Portal target. Defaults to `<body>`. */
  container?: HTMLElement | null;
}) {
  return (
    <RadixPopover.Portal container={container ?? undefined}>
      <RadixPopover.Content
        align="start"
        {...floatingSurface({ radixPrefix: "popover", maxHeight, className, style })}
        {...props}
      />
    </RadixPopover.Portal>
  );
}
