// iOS-style switch. On-state uses the shipped accent token (Humla gold),
// not the pre-redesign "ink" wording from the PRD.
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
        "relative inline-flex h-[22px] w-[38px] shrink-0 items-center rounded-full border transition-colors disabled:opacity-40 disabled:cursor-not-allowed " +
        (checked
          ? "bg-[var(--color-accent)] border-[var(--color-accent)]"
          : "bg-[var(--color-pill-hover)] border-[var(--color-line-visible)]")
      }
    >
      <span
        aria-hidden
        className={
          "inline-block h-[16px] w-[16px] rounded-full bg-white shadow-sm transition-transform " +
          (checked ? "translate-x-[18px]" : "translate-x-[3px]")
        }
      />
    </button>
  );
}
