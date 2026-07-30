import { useRef, useState } from "react";
import * as RadixSelect from "@radix-ui/react-select";
import { Check, ChevronDown } from "lucide-react";
import { cn } from "../../lib/cn";
import { floatingSurface, rowClass, rowHighlightClass } from "./surface";

// Compact popover select (Claude-desktop style): a quiet trigger showing the
// current value + chevron, opening a styled listbox with a check on the
// selected row. Same public API as ever.
//
// Radix `Select` underneath since #114 — it brings the arrow keys and typeahead
// this control never had (picking a language out of ~100 options was
// mouse-or-nothing), plus collision-aware placement and the ARIA listbox
// wiring, replacing ~130 lines of hand-rolled measurement and dismissal.
//
// The listbox is portalled. Several callers (ImportDialog, SettingsDialog) sit
// inside a dialog whose *content panel* is `overflow-y-auto`, which clips an
// in-flow absolutely-positioned child; portalling escapes that. We aim the
// portal at the nearest `[role="dialog"]` ancestor when there is one — that
// clears the panel's clip while staying inside `within(dialog)` queries in
// tests and inside the dialog for assistive tech. Falls back to <body>.

// Radix reserves the empty string: `Select.Root value=""` means "nothing
// chosen", and an `Item` with an empty value throws outright. Several callers
// legitimately have one — the "Choose a model…" / "Choose a member…" placeholder
// row — so encode it on the way in and decode on the way out, leaving the
// public API a plain string.
const NO_SELECTION = "__empty__";
const toRadixValue = (v: string) => (v === "" ? NO_SELECTION : v);
const fromRadixValue = (v: string) => (v === NO_SELECTION ? "" : v);

export function Select({
  value,
  onChange,
  options,
  id,
}: {
  value: string;
  onChange: (v: string) => void;
  options: { value: string; label: string }[];
  // Optional id placed on the trigger (a labelable element) so an external
  // <label htmlFor> can name this control.
  id?: string;
}) {
  const triggerRef = useRef<HTMLButtonElement>(null);
  // Resolved when the listbox opens rather than in a ref callback, which would
  // re-run on every render.
  const [container, setContainer] = useState<HTMLElement | null>(null);
  const current = options.find((o) => o.value === value);

  return (
    <RadixSelect.Root
      value={toRadixValue(value)}
      onValueChange={(v) => onChange(fromRadixValue(v))}
      onOpenChange={(open) => {
        if (open) setContainer(triggerRef.current?.closest<HTMLElement>('[role="dialog"]') ?? null);
      }}
    >
      <RadixSelect.Trigger
        id={id}
        // A `combobox` takes no name from its own content, so the trigger would
        // be nameless where the caller doesn't wrap it in a <label htmlFor>.
        // Fall back to the current value — the same name the old plain <button>
        // derived from its text — and stay out of the way when there IS a label.
        aria-label={id ? undefined : (current?.label ?? value)}
        ref={triggerRef}
        className="inline-flex items-center gap-1.5 max-w-[260px] px-2.5 py-1.5 rounded-md text-sm border border-[var(--color-line-visible)] bg-[var(--color-surface)] hover:bg-[var(--color-pill-hover)] transition-colors"
      >
        <span className="truncate">{current?.label ?? value}</span>
        <RadixSelect.Icon asChild>
          <ChevronDown
            size={13}
            strokeWidth={1.8}
            className="shrink-0 text-[var(--color-text-muted)] transition-transform data-[state=open]:rotate-180"
            aria-hidden
          />
        </RadixSelect.Icon>
      </RadixSelect.Trigger>
      <RadixSelect.Portal container={container ?? undefined}>
        <RadixSelect.Content
          position="popper"
          align="end"
          {...floatingSurface({
            radixPrefix: "select",
            maxHeight: 256,
            className: "max-w-[300px] min-w-[var(--radix-select-trigger-width)]",
          })}
          // Escape dismisses the listbox only. Radix's handler sits on
          // `document`, so stopping propagation here keeps the key away from
          // the settings dialog's window listener — which would otherwise close
          // the whole dialog out from under the user.
          onEscapeKeyDown={(e) => e.stopPropagation()}
        >
          <RadixSelect.Viewport>
            {options.map((o) => (
              <RadixSelect.Item
                key={o.value}
                value={toRadixValue(o.value)}
                className={cn(
                  rowClass,
                  rowHighlightClass,
                  "justify-between gap-3 data-[state=checked]:text-[var(--color-text)]",
                )}
              >
                <RadixSelect.ItemText>
                  <span className="truncate">{o.label}</span>
                </RadixSelect.ItemText>
                <RadixSelect.ItemIndicator>
                  <Check
                    size={14}
                    strokeWidth={2}
                    className="shrink-0 text-[var(--color-accent-text)]"
                    aria-hidden
                  />
                </RadixSelect.ItemIndicator>
              </RadixSelect.Item>
            ))}
          </RadixSelect.Viewport>
        </RadixSelect.Content>
      </RadixSelect.Portal>
    </RadixSelect.Root>
  );
}
