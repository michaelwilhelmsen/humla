import { useEffect, useRef } from "react";
import { X } from "lucide-react";
import { Settings } from "../Settings";

// Route-backed settings dialog. `/settings?tab=` stays the source of truth
// for what's shown; App renders this over the remembered background view
// instead of swapping the page out.
export function SettingsDialog({ onClose }: { onClose: () => void }) {
  const panelRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  // Focus management: keyboard/AT users land inside the dialog on open and
  // return to whatever triggered it on close.
  useEffect(() => {
    const trigger = document.activeElement;
    panelRef.current?.focus();
    return () => {
      if (trigger instanceof HTMLElement && trigger.isConnected) {
        trigger.focus();
      }
    };
  }, []);

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label="Settings"
      className="fixed inset-0 z-50 flex items-center justify-center"
    >
      <div
        className="absolute inset-0 bg-black/50"
        onClick={onClose}
        data-testid="settings-backdrop"
        aria-hidden
      />
      <div
        ref={panelRef}
        tabIndex={-1}
        className="relative z-10 w-[min(56rem,calc(100vw-3rem))] h-[min(42rem,calc(100vh-4rem))] overflow-hidden rounded-[var(--radius-card)] bg-[var(--color-canvas)] border border-[var(--color-line-visible)] shadow-xl outline-none"
      >
        <button
          type="button"
          onClick={onClose}
          aria-label="Close settings"
          title="Close (Esc)"
          className="absolute top-3 right-3 z-20 grid place-items-center w-8 h-8 rounded-full text-[var(--color-text-muted)] hover:bg-[var(--color-pill-hover)] hover:text-[var(--color-text)] transition-colors"
        >
          <X size={16} strokeWidth={1.8} />
        </button>
        <Settings />
      </div>
    </div>
  );
}
