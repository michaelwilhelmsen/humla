export function Btn({
  children,
  ...props
}: React.ButtonHTMLAttributes<HTMLButtonElement>) {
  return (
    <button
      {...props}
      className="px-3 py-2 rounded-md text-sm border border-[var(--color-line-visible)] bg-[var(--color-surface)] hover:border-[var(--color-text)] hover:bg-[var(--color-pill-hover)] disabled:opacity-50 disabled:hover:border-[var(--color-line-visible)] transition-colors"
    >
      {children}
    </button>
  );
}
