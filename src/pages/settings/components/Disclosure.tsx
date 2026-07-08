import { useState, type ReactNode } from "react";
import { ChevronRight } from "lucide-react";

// Progressive-disclosure primitive: one "Advanced" per section keeps expert
// knobs out of sight until asked for. Collapsed by default; not persisted.
export function Disclosure({
  label,
  children,
}: {
  label: string;
  children: ReactNode;
}) {
  const [open, setOpen] = useState(false);
  return (
    <div className="border-t border-[var(--color-line)] pt-3 mt-1">
      <button
        type="button"
        aria-expanded={open}
        onClick={() => setOpen((o) => !o)}
        className="flex items-center gap-1.5 text-sm text-[var(--color-text-muted)] hover:text-[var(--color-text)] transition-colors"
      >
        <ChevronRight
          size={14}
          strokeWidth={1.8}
          className={"transition-transform " + (open ? "rotate-90" : "")}
          aria-hidden
        />
        {label}
      </button>
      {open && <div className="mt-4 flex flex-col gap-5">{children}</div>}
    </div>
  );
}
