export function Section({
  title,
  children,
}: {
  title: string;
  children: React.ReactNode;
}) {
  return (
    <section className="mb-12">
      <h2 className="nd-label mb-5">{title}</h2>
      <div className="flex flex-col gap-5">{children}</div>
    </section>
  );
}

export function Row({
  label,
  children,
}: {
  label: string;
  children: React.ReactNode;
}) {
  return (
    <div>
      <div className="text-sm mb-1.5">{label}</div>
      {children}
    </div>
  );
}
