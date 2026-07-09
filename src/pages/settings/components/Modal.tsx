import { useEffect, useRef, type ReactNode } from "react";
import { createPortal } from "react-dom";

// Selector for the elements a keyboard user can Tab to. Kept in one place so
// initial-focus and the Tab trap agree on what counts as focusable.
const FOCUSABLE_SELECTOR = [
  "a[href]",
  "button:not([disabled])",
  "textarea:not([disabled])",
  "input:not([disabled])",
  "select:not([disabled])",
  '[tabindex]:not([tabindex="-1"])',
].join(",");

// Lightweight centred modal. Click backdrop or press Esc to dismiss;
// inner content is the caller's responsibility. Used by the summary
// prompt editor — kept small and neutral so future modals can reuse
// it without inheriting opinionated styling.
//
// Rendered through a portal to <body> (same idiom as ContextMenu) so the
// overlay + panel escape whatever stacking context the caller sits in.
// Callers like the Note toolbar live deep inside a positioned/z-indexed
// subtree, and a sibling — the Summary/Transcript context panel (`aside`,
// `relative z-30`) — would otherwise paint above this modal: `fixed z-50`
// only outranks siblings *within the same stacking context*, and the
// toolbar's own `relative z-30` traps it below that panel. Portaling to
// <body> lifts it above the whole app so the overlay dims everything.
//
// Focus management (a11y): `aria-modal` alone doesn't make the background
// inert in a webview, so a keyboard user could Tab out into the dimmed app
// behind the overlay. On open we move focus into the dialog, trap Tab /
// Shift-Tab within it (wrap-around), and restore focus to whatever was
// focused before on close.
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
  const panelRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;

    // Remember the trigger so focus can return to it on close.
    const previouslyFocused = document.activeElement as HTMLElement | null;

    const focusables = () =>
      panelRef.current
        ? Array.from(
            panelRef.current.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR),
          )
        : [];

    // Move focus into the dialog: first focusable element, else the panel.
    const initial = focusables();
    if (initial.length > 0) initial[0].focus();
    else panelRef.current?.focus();

    // Capture phase + stopPropagation on Escape: when this modal is nested
    // inside the settings dialog (e.g. the prompt editor), Escape must dismiss
    // only the innermost layer — never reach the dialog's window listener and
    // close everything (discarding in-progress edits). Same layering as Select.
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
        return;
      }
      if (e.key === "Tab") {
        const items = focusables();
        const panel = panelRef.current;
        if (!panel) return;
        if (items.length === 0) {
          // Nothing to land on — pin focus to the panel itself.
          e.preventDefault();
          panel.focus();
          return;
        }
        const first = items[0];
        const last = items[items.length - 1];
        const active = document.activeElement as HTMLElement | null;
        // Wrap-around at both ends; also reel focus back in if it somehow
        // escaped the panel (e.g. focus started on the body).
        if (e.shiftKey) {
          if (active === first || !panel.contains(active)) {
            e.preventDefault();
            last.focus();
          }
        } else if (active === last || !panel.contains(active)) {
          e.preventDefault();
          first.focus();
        }
      }
    };
    document.addEventListener("keydown", onKey, true);
    return () => {
      document.removeEventListener("keydown", onKey, true);
      // Restore focus to the trigger (no-op if it's been unmounted).
      previouslyFocused?.focus?.();
    };
  }, [open, onClose]);

  if (!open) return null;

  return createPortal(
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
      <div
        ref={panelRef}
        tabIndex={-1}
        className="relative z-10 max-w-2xl w-[min(48rem,calc(100vw-3rem))] max-h-[calc(100vh-4rem)] overflow-y-auto bg-[var(--color-canvas)] border border-[var(--color-line-visible)] rounded-lg shadow-xl outline-none"
      >
        {title && (
          <div className="px-6 py-4 border-b border-[var(--color-line)]">
            <h2 className="text-lg font-medium">{title}</h2>
          </div>
        )}
        <div className="px-6 py-5">{children}</div>
      </div>
    </div>,
    document.body,
  );
}
