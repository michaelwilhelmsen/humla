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
      // Hard width cap, not max-w-full: Row's control slot is shrink-0, so a
      // percentage cap never binds and long values (self-hosted server URLs)
      // would blow the card open. Full value stays reachable via tooltip.
      className="inline-block max-w-[260px] truncate px-2 py-0.5 text-[11px] rounded border border-[var(--color-line)]"
      title={typeof children === "string" ? children : undefined}
      style={{
        fontFamily: "var(--font-mono)",
        color: color ?? "var(--color-text-muted)",
      }}
    >
      {children}
    </span>
  );
}
