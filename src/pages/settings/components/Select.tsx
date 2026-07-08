import { useEffect, useRef, useState } from "react";
import { Check, ChevronDown } from "lucide-react";

// Compact popover select (Claude-desktop style): a quiet trigger showing the
// current value + chevron, opening a styled listbox with a check on the
// selected row. Same public API as the old native <select> wrapper.
export function Select({
  value,
  onChange,
  options,
}: {
  value: string;
  onChange: (v: string) => void;
  options: { value: string; label: string }[];
}) {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);
  const current = options.find((o) => o.value === value);

  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (!rootRef.current?.contains(e.target as Node)) {
        setOpen(false);
        // Swallow the click this mousedown produces: dismissing the popover
        // must not also activate whatever is underneath (the dialog backdrop
        // would close all of settings). Matches native macOS menu behavior.
        document.addEventListener(
          "click",
          (ce) => ce.stopPropagation(),
          { capture: true, once: true },
        );
      }
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
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey, true);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey, true);
    };
  }, [open]);

  return (
    <div ref={rootRef} className="relative inline-block">
      <button
        type="button"
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
      {open && (
        <div
          role="listbox"
          className="absolute right-0 top-full mt-1 z-50 min-w-full w-max max-w-[300px] max-h-64 overflow-y-auto rounded-lg border border-[var(--color-line-visible)] bg-[var(--color-canvas)] shadow-lg p-1"
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
                  // view. Scroll ONLY the listbox — scrollIntoView would also
                  // scroll the settings panel behind the popover.
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
        </div>
      )}
    </div>
  );
}
