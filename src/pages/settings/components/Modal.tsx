import { useEffect, type ReactNode } from "react";

// Lightweight centred modal. Click backdrop or press Esc to dismiss;
// inner content is the caller's responsibility. Used by the summary
// prompt editor — kept small and neutral so future modals can reuse
// it without inheriting opinionated styling.
export function Modal({
  open,
  onClose,
  children,
  title,
}: {
  open: boolean;
  onClose: () => void;
  children: ReactNode;
  title?: string;
}) {
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  if (!open) return null;

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label={title}
      className="fixed inset-0 z-50 flex items-center justify-center"
    >
      <div
        className="absolute inset-0 bg-black/50"
        onClick={onClose}
        aria-hidden
      />
      <div className="relative z-10 max-w-2xl w-[min(48rem,calc(100vw-3rem))] max-h-[calc(100vh-4rem)] overflow-y-auto bg-[var(--color-canvas)] border border-[var(--color-line-visible)] rounded-lg shadow-xl">
        {title && (
          <div className="px-6 py-4 border-b border-[var(--color-line)]">
            <h2 className="text-lg font-medium">{title}</h2>
          </div>
        )}
        <div className="px-6 py-5">{children}</div>
      </div>
    </div>
  );
}
