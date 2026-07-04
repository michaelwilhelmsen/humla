// Shared per-step layout primitive. Centres a step's content in the takeover
// canvas with an optional glyph, a title, and a subtitle. Steps compose their
// own controls into `children`. Later work-package steps should reuse this so
// the wizard stays visually coherent as files are swapped in one at a time.
import type { ReactNode } from "react";

export function StepShell({
  icon,
  title,
  subtitle,
  children,
  align = "center",
}: {
  icon?: ReactNode;
  title: string;
  subtitle?: ReactNode;
  children?: ReactNode;
  // Most steps read best centred; content-heavy steps (a long option list)
  // can opt into left alignment.
  align?: "center" | "left";
}) {
  const centered = align === "center";
  return (
    <div
      className={
        "w-full max-w-lg flex flex-col " +
        (centered ? "items-center text-center" : "items-start text-left")
      }
    >
      {icon && (
        <div
          className="grid place-items-center w-14 h-14 rounded-2xl mb-6 text-[var(--color-accent-text)]"
          style={{ background: "var(--color-accent-soft)" }}
        >
          {icon}
        </div>
      )}
      <h1 className="text-[28px] font-semibold tracking-[-0.02em] leading-tight text-[var(--color-text-display)]">
        {title}
      </h1>
      {subtitle && (
        <p className="mt-3 text-[15px] leading-relaxed text-[var(--color-text-muted)] max-w-md">
          {subtitle}
        </p>
      )}
      {children && (
        <div className={"mt-8 w-full " + (centered ? "flex flex-col items-center" : "")}>
          {children}
        </div>
      )}
    </div>
  );
}
