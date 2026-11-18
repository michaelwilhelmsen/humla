// Settings group chrome: sentence-case header above an inset card, rows
// separated by hairlines. Row supports two shapes:
//   - control rows (new kit): label + muted description left, control right
//   - block rows (legacy tabs): label on top, free-form children below
// The legacy shape stays until PRD 2/2 rebuilds the remaining bodies.
// `scope` names the workspace a section's settings apply to, for the sections
// an organisation's policy can govern.
export function Section({
  title,
  scope,
  children,
}: {
  title: string;
  scope?: string;
  children: React.ReactNode;
}) {
  return (
    <section className="mb-8">
      <h2 className="text-sm font-medium mb-2.5 flex items-baseline gap-2">
        {title}
        {scope && <span className="text-xs font-normal text-[var(--color-text-muted)]">{scope}</span>}
      </h2>
      {/* py-2 keeps legacy block children (which carry no vertical padding
          of their own) off the card edges until PRD 2/2 rebuilds them onto
          Rows; kit Rows add their own py on top of it. */}
      <div className="flex flex-col divide-y divide-[var(--color-line)] rounded-[var(--radius-card)] border border-[var(--color-line-visible)] bg-[var(--color-surface)] px-4 py-2">
        {children}
      </div>
    </section>
  );
}

export function Row({
  label,
  description,
  control,
  children,
}: {
  label: string;
  description?: React.ReactNode;
  control?: React.ReactNode;
  children?: React.ReactNode;
}) {
  if (control) {
    return (
      <div className="flex items-center justify-between gap-6 py-3.5">
        <div className="min-w-0">
          <div className="text-sm">{label}</div>
          {description && (
            <p className="text-xs text-[var(--color-text-muted)] mt-0.5">
              {description}
            </p>
          )}
        </div>
        <div className="shrink-0">{control}</div>
      </div>
    );
  }
  return (
    <div className="py-3.5">
      <div className="text-sm mb-1.5">{label}</div>
      {children}
    </div>
  );
}
