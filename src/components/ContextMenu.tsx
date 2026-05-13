import { useEffect, useRef } from "react";
import { createPortal } from "react-dom";
import { cn } from "../lib/cn";

// Small floating menu anchored at viewport (x, y). Closes on any click,
// right-click outside, Escape, or scroll. Items render via the children
// prop — pass <ContextMenuItem /> rows.
export function ContextMenu({
  x,
  y,
  onClose,
  children,
}: {
  x: number;
  y: number;
  onClose: () => void;
  children: React.ReactNode;
}) {
  const ref = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    function onDown(e: MouseEvent) {
      if (ref.current && ref.current.contains(e.target as Node)) return;
      onClose();
    }
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") onClose();
    }
    function onScroll() {
      onClose();
    }
    // Use mousedown + keydown + scroll so the menu dismisses on the
    // first interaction outside it (including the right-click that
    // would open a new menu).
    document.addEventListener("mousedown", onDown);
    document.addEventListener("contextmenu", onDown);
    document.addEventListener("keydown", onKey);
    document.addEventListener("scroll", onScroll, true);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("contextmenu", onDown);
      document.removeEventListener("keydown", onKey);
      document.removeEventListener("scroll", onScroll, true);
    };
  }, [onClose]);

  // Keep the menu inside the viewport — if too close to the right or
  // bottom edge, anchor from the opposite side.
  const vw = typeof window !== "undefined" ? window.innerWidth : 1200;
  const vh = typeof window !== "undefined" ? window.innerHeight : 800;
  const w = 200; // approximate width — used only for edge nudging
  const h = 100; // approximate height
  const left = x + w > vw ? Math.max(8, vw - w - 8) : x;
  const top = y + h > vh ? Math.max(8, vh - h - 8) : y;

  return createPortal(
    <div
      ref={ref}
      role="menu"
      className="fixed z-50 min-w-[160px] p-1 rounded-md border border-[var(--color-line-visible)] bg-[var(--color-surface)] shadow-lg"
      style={{ top, left }}
    >
      {children}
    </div>,
    document.body,
  );
}

export function ContextMenuItem({
  onClick,
  children,
  danger,
}: {
  onClick: () => void;
  children: React.ReactNode;
  danger?: boolean;
}) {
  return (
    <button
      type="button"
      role="menuitem"
      onClick={onClick}
      className={cn(
        "w-full text-left px-3 py-1.5 text-sm rounded-sm transition-colors",
        danger
          ? "text-[var(--color-accent)] hover:bg-[var(--color-pill-hover)]"
          : "text-[var(--color-text)] hover:bg-[var(--color-pill-hover)]",
      )}
    >
      {children}
    </button>
  );
}
