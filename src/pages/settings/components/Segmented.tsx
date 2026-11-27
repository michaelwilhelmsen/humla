// 2–3-way segmented control. Radio semantics; selected segment fills with
// the soft accent, matching the shipped theme/palette pickers.
export function Segmented<T extends string>({
  label,
  value,
  onChange,
  options,
  size = "md",
}: {
  label: string;
  /** `sm` sits in a 32px toolbar row. */
  size?: "md" | "sm";
  value: T;
  onChange: (next: T) => void;
  options: { value: T; label: string }[];
}) {
  return (
    <div
      role="radiogroup"
      aria-label={label}
      className={
        "flex rounded-md border border-[var(--color-line-visible)] bg-[var(--color-surface)] w-fit " +
        (size === "sm" ? "gap-0.5 p-0.5" : "gap-1 p-1")
      }
    >
      {options.map((opt) => {
        const selected = opt.value === value;
        return (
          <button
            key={opt.value}
            type="button"
            role="radio"
            aria-checked={selected}
            onClick={() => onChange(opt.value)}
            className={
              (size === "sm" ? "h-[26px] px-2 text-[13px] " : "px-3 py-1 text-sm ") +
              "rounded whitespace-nowrap transition-colors " +
              (selected
                ? "bg-[var(--color-accent-soft)] text-[var(--color-accent-text)]"
                : "text-[var(--color-text-muted)] hover:text-[var(--color-text)]")
            }
          >
            {opt.label}
          </button>
        );
      })}
    </div>
  );
}
