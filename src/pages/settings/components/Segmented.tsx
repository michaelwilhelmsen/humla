// 2–3-way segmented control. Radio semantics; selected segment fills with
// the soft accent, matching the shipped theme/palette pickers.
export function Segmented<T extends string>({
  label,
  value,
  onChange,
  options,
}: {
  label: string;
  value: T;
  onChange: (next: T) => void;
  options: { value: T; label: string }[];
}) {
  return (
    <div
      role="radiogroup"
      aria-label={label}
      className="flex gap-1 p-1 rounded-md border border-[var(--color-line-visible)] bg-[var(--color-surface)] w-fit"
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
              "px-3 py-1 rounded text-sm transition-colors " +
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
