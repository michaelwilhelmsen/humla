import { Link, useNavigate, useLocation } from "react-router-dom";
import { Menu, Plus, Home as HomeIcon, Settings as SettingsIcon } from "lucide-react";
import { ipc } from "../lib/ipc";
import { useNotesStore } from "../lib/store";
import { cn } from "../lib/cn";

// Narrow icon-only sidebar shown when the main sidebar is collapsed.
// Keeps the chrome consistent — instead of vanishing, the column
// shrinks to ~48 px and exposes the day-to-day actions: toggle back
// open, create a note, jump home, jump to Settings. Top padding
// matches the traffic-light hot zone so the hamburger doesn't sit
// behind the macOS window controls (the bug that motivated this).
export function SidebarCollapsed({ onExpand }: { onExpand: () => void }) {
  const navigate = useNavigate();
  const location = useLocation();
  const upsert = useNotesStore((s) => s.upsertLocal);

  async function newNote() {
    const note = await ipc.createNote();
    upsert(note);
    navigate(`/note/${note.id}`);
  }

  const onSettings = location.pathname.startsWith("/settings");
  const onHome = location.pathname === "/";

  return (
    <div className="flex flex-col items-center gap-1 pt-14 pb-3 h-full relative z-20">
      <IconBtn label="Open sidebar" onClick={onExpand}>
        <Menu size={18} />
      </IconBtn>
      <div className="h-2" />
      <IconBtn label="New note" onClick={newNote}>
        <Plus size={18} />
      </IconBtn>
      <IconLink label="Home" to="/" active={onHome}>
        <HomeIcon size={18} />
      </IconLink>
      <div className="flex-1" />
      <IconLink label="Settings" to="/settings" active={onSettings}>
        <SettingsIcon size={18} />
      </IconLink>
    </div>
  );
}

function IconBtn({
  label,
  onClick,
  children,
}: {
  label: string;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      onClick={onClick}
      data-tauri-drag-region="false"
      className="no-drag w-9 h-9 flex items-center justify-center rounded-md text-[var(--color-text-muted)] hover:bg-[var(--color-pill-hover)] hover:text-[var(--color-text)]"
    >
      {children}
    </button>
  );
}

function IconLink({
  label,
  to,
  active,
  children,
}: {
  label: string;
  to: string;
  active: boolean;
  children: React.ReactNode;
}) {
  return (
    <Link
      to={to}
      aria-label={label}
      title={label}
      data-tauri-drag-region="false"
      className={cn(
        "no-drag w-9 h-9 flex items-center justify-center rounded-md hover:bg-[var(--color-pill-hover)]",
        active
          ? "text-[var(--color-text)] bg-[var(--color-pill-hover)]"
          : "text-[var(--color-text-muted)] hover:text-[var(--color-text)]",
      )}
    >
      {children}
    </Link>
  );
}
