import { useNavigate, useLocation } from "react-router-dom";
import { Moon, PenLine, Sun } from "lucide-react";
import { ipc } from "../lib/ipc";
import { useNotesStore } from "../lib/store";
import { useThemeStore } from "../lib/theme";

function resolveEffectiveTheme(theme: "system" | "light" | "dark"): "light" | "dark" {
  if (theme !== "system") return theme;
  if (typeof window === "undefined") return "light";
  return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

// Floating action strip in the top-right of the main column. Sits above
// the Layout drag strip so its buttons are clickable, but only renders on
// pages without their own top chrome (Home, Folder). Note and Settings
// pages have their own toolbars and would conflict.
export function TopBar() {
  const location = useLocation();
  const navigate = useNavigate();
  const upsert = useNotesStore((s) => s.upsertLocal);
  const theme = useThemeStore((s) => s.theme);
  const setTheme = useThemeStore((s) => s.setTheme);

  const path = location.pathname;
  const visible = path === "/" || path.startsWith("/folder/");
  if (!visible) return null;

  const effective = resolveEffectiveTheme(theme);

  async function newNote() {
    const note = await ipc.createNote();
    upsert(note);
    navigate(`/note/${note.id}`);
  }

  function flipTheme() {
    // Two-state quick toggle: explicit light ↔ dark. To return to "follow
    // system" the user goes through Settings — keeps this control simple.
    setTheme(effective === "dark" ? "light" : "dark");
  }

  return (
    <div className="absolute top-2 right-4 z-30 flex items-center gap-2">
      <button
        type="button"
        onClick={flipTheme}
        aria-label={effective === "dark" ? "Switch to light" : "Switch to dark"}
        title={effective === "dark" ? "Switch to light" : "Switch to dark"}
        className="no-drag w-9 h-9 flex items-center justify-center rounded-full text-[var(--color-text-muted)] hover:bg-[var(--color-pill-hover)] hover:text-[var(--color-text)] transition-colors"
      >
        {effective === "dark" ? (
          <Sun size={16} strokeWidth={1.5} />
        ) : (
          <Moon size={16} strokeWidth={1.5} />
        )}
      </button>
      <button
        type="button"
        onClick={newNote}
        title="⌘N"
        className="no-drag inline-flex items-center gap-2 pl-3 pr-4 py-1.5 rounded-full border border-[var(--color-line-visible)] bg-[var(--color-surface)] text-[var(--color-text)] text-sm hover:border-[var(--color-text)] transition-colors"
      >
        <PenLine size={14} strokeWidth={1.5} />
        <span>New note</span>
      </button>
    </div>
  );
}
