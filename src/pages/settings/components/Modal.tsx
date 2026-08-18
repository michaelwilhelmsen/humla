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
  showHeader = true,
  size = "md",
  padded = true,
}: {
  open: boolean;
  onClose: () => void;
  children: ReactNode;
  title?: string;
  /** False when the body supplies its own visible heading. `title` still names
   *  the dialog for assistive tech either way — it is the ONE source for that
   *  name, and this only decides whether a header row is drawn. */
  showHeader?: boolean;
  /** Panel width. "md" (default) is the settings-editor width; "sm" is for a
   *  focused single-decision flow, where the full width leaves a short form
   *  stranded in the middle of a very wide sheet. */
  size?: "md" | "sm";
  /** Set false when the body owns its own padding. Not the same as padding zero:
   *  a body that pads its own sections can still let ONE of them reach the panel
   *  edge — a footer rule spanning the full width, say — which a single pad on
   *  this wrapper cannot express however it is valued. */
  padded?: boolean;
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
        className={
          // `--color-surface`, not `--color-canvas`: this panel is borderless, so
          // its background is the only thing separating it from the dimmed app
          // behind. In warm dark, canvas (#0e0c08) against a black/50 backdrop
          // over that same canvas differs by single digits out of 255, and
          // `--color-shadow` is transparent in both modes — the panel simply
          // had no edge. Surface is the next rung up the documented ladder.
          "relative z-10 max-h-[calc(100vh-4rem)] overflow-y-auto bg-[var(--color-surface)] rounded-lg shadow-xl outline-none " +
          (size === "sm"
            ? "max-w-md w-[min(30rem,calc(100vw-3rem))]"
            : "max-w-2xl w-[min(48rem,calc(100vw-3rem))]")
        }
      >
        {title && showHeader && (
          <div className="px-6 py-4 border-b border-[var(--color-line)]">
            <h2 className="text-lg font-medium">{title}</h2>
          </div>
        )}
        <div className={padded ? "px-6 py-5" : ""}>{children}</div>
      </div>
    </div>,
    document.body,
  );
}
