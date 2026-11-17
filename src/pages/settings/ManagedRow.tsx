import type { ReactNode } from "react";
import { Lock } from "lucide-react";
import { Popover, PopoverContent, PopoverTrigger } from "../../components/ui/Popover";

/** "Managed by <source>": opens who set the value and why. */
export function ManagedBadge({ source, reason }: { source: string; reason: string }) {
  return (
    <Popover>
      <PopoverTrigger asChild>
        <button
          type="button"
          className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded-md text-xs text-[var(--color-text-muted)] hover:bg-[var(--color-pill-hover)] hover:text-[var(--color-text)] transition-colors"
        >
          <Lock size={12} strokeWidth={2} aria-hidden />
          Managed by {source}
        </button>
      </PopoverTrigger>
      <PopoverContent align="start" className="w-72 p-3 text-xs leading-relaxed">
        <p className="text-sm font-medium mb-1">Set by {source}</p>
        {reason && <p className="text-[var(--color-text-muted)]">{reason}</p>}
        <p className="text-[var(--color-text-muted)] mt-2">Only an administrator can change it.</p>
      </PopoverContent>
    </Popover>
  );
}

/** A settings row whose value an organisation fixes; `control` shows the enforced value. */
export function ManagedRow({
  label,
  source,
  reason,
  control,
}: {
  label: string;
  source: string;
  reason: string;
  control: ReactNode;
}) {
  return (
    <div className="flex items-center justify-between gap-6 py-3.5">
      <div className="min-w-0 flex flex-wrap items-center gap-x-2 gap-y-1">
        <span className="text-sm">{label}</span>
        <ManagedBadge source={source} reason={reason} />
      </div>
      <div className="shrink-0">{control}</div>
    </div>
  );
}
