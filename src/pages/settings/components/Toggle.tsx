// iOS-style switch. Geometry (track size, thumb size) and the on-state colour
// are contract tokens, so a theme states its own switch — `graphite`'s compact
// 34 × 20 ink switch is a different object from `warm`'s 38 × 22 gold one. The
// thumb travel is derived from the three so no theme has to state a fourth
// number.
export function Toggle({
  checked,
  onChange,
  label,
  disabled,
}: {
  checked: boolean;
  onChange: (next: boolean) => void;
  label: string;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      disabled={disabled}
      onClick={() => onChange(!checked)}
      className={
        "relative inline-flex h-[var(--switch-h)] w-[var(--switch-w)] shrink-0 items-center rounded-full border transition-colors disabled:opacity-40 disabled:cursor-not-allowed " +
        (checked
          ? "bg-[var(--color-switch-on)] border-[var(--color-switch-on)]"
          : "bg-[var(--color-pill-hover)] border-[var(--color-line-visible)]")
      }
    >
      <span
        aria-hidden
        // Travel is derived: track width, less the 1px border each side, less
        // the thumb, less the resting inset. A theme resizing the switch keeps
        // the thumb inside the track without stating a travel distance.
        style={{
          transform: checked
            ? "translateX(calc(var(--switch-w) - 2px - var(--switch-thumb) - var(--switch-inset, 2px)))"
            : "translateX(var(--switch-inset, 2px))",
        }}
        className="inline-block h-[var(--switch-thumb)] w-[var(--switch-thumb)] rounded-full bg-white shadow-sm transition-transform"
      />
    </button>
  );
}
