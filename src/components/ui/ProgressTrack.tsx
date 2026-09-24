import { cn } from "../../lib/cn";

/**
 * A progress track. A `null` value is indeterminate in ARIA's own terms — a
 * `progressbar` with no `aria-valuenow` — for work that reports no fraction,
 * so nothing here has to say "no percentage" twice.
 */
export function ProgressTrack({
  value,
  label,
  className,
}: {
  value: number | null;
  /** The whole label, as the bar's own name. Not `aria-labelledby` to the
      visible text, which a narrow layout may shorten. */
  label: string;
  className?: string;
}) {
  const pct = value === null ? 100 : Math.round(value * 100);
  return (
    <div
      role="progressbar"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={value === null ? undefined : pct}
      aria-busy={value === null ? true : undefined}
      className={cn("h-1 rounded-full bg-[var(--color-pill-hover)] overflow-hidden", className)}
    >
      <div
        className={cn(
          "h-full rounded-full bg-[var(--color-accent)] transition-[width] duration-200",
          value === null && "nd-progress-indeterminate",
        )}
        style={{ width: `${pct}%` }}
      />
    </div>
  );
}
