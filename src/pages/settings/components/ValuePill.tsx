import type { ReactNode } from "react";

// Static value chip (server URLs, plan states, IDs): mono, muted, bordered.
// Display-only — interactive values belong in Select/Toggle/Btn.
export function ValuePill({
  children,
  color,
}: {
  children: ReactNode;
  color?: string;
}) {
  return (
    <span
      className="inline-block max-w-full truncate px-2 py-0.5 text-[11px] rounded border border-[var(--color-line)]"
      style={{
        fontFamily: "var(--font-mono)",
        color: color ?? "var(--color-text-muted)",
      }}
    >
      {children}
    </span>
  );
}
