import { useState } from "react";
import { useNavigate } from "react-router-dom";
import { Check, ChevronsUpDown, Cloud, CloudOff, Plus, RefreshCw, User, Users } from "lucide-react";
import { cloudApi, roleLabel, useCloudStore, type CloudRole } from "../lib/cloud";
import { useNotesStore } from "../lib/store";
import type { SyncStatus } from "../lib/ipc";
import { Menu, MenuContent, MenuItem, MenuLabel, MenuSeparator, MenuTrigger } from "./ui/Menu";

// Small sync-state indicator shown next to the active workspace. Surfaces what
// was previously invisible: whether sync is running, done, or failing.
function SyncIndicator({ status }: { status: SyncStatus }) {
  const map = {
    syncing: { Icon: RefreshCw, cls: "animate-spin text-[var(--color-text-muted)]", title: "Syncing…" },
    idle: { Icon: Cloud, cls: "text-[var(--color-success)]", title: "Synced" },
    error: { Icon: CloudOff, cls: "text-[var(--color-danger)]", title: "Sync error — will retry" },
  }[status];
  const { Icon } = map;
  return (
    <span title={map.title} aria-label={map.title} className="shrink-0 grid place-items-center">
      <Icon size={13} strokeWidth={1.5} className={map.cls} />
    </span>
  );
}

function RolePill({ role }: { role: CloudRole }) {
  return (
    <span
      className="shrink-0 px-1 text-[9px] uppercase tracking-[0.08em] rounded border border-[var(--color-line)] text-[var(--color-text-muted)]"
    >
      {roleLabel(role)}
    </span>
  );
}

// Workspace / organization switcher. Sits at the top of the sidebar. Shows the
// active workspace (or "Personal" for local-only), and a dropdown to switch,
// create, or sign in. Inert-but-present when signed out (offers "Personal" +
// a sign-in affordance) so the chrome is consistent across cloud states.
export function WorkspaceSwitcher() {
  const navigate = useNavigate();
  const status = useCloudStore((s) => s.status);
  const syncStatus = useCloudStore((s) => s.syncStatus);
  const refreshCloud = useCloudStore((s) => s.refresh);
  const refreshNotes = useNotesStore((s) => s.refresh);
  const [open, setOpen] = useState(false);
  const [creating, setCreating] = useState(false);
  const [newName, setNewName] = useState("");
  const [busy, setBusy] = useState(false);

  const current = status.current_workspace;
  const label = current?.name ?? "Personal";

  async function select(id: string) {
    setBusy(true);
    try {
      await cloudApi.selectWorkspace(id);
      await refreshCloud();
      await refreshNotes();
    } catch {
      /* surfaced elsewhere; keep the switcher resilient */
    } finally {
      setBusy(false);
      setOpen(false);
    }
  }

  async function createWorkspace() {
    const name = newName.trim();
    if (!name) {
      setCreating(false);
      setNewName("");
      return;
    }
    setBusy(true);
    try {
      await cloudApi.createWorkspace(name);
      await refreshCloud();
      await refreshNotes();
    } finally {
      setBusy(false);
      setCreating(false);
      setNewName("");
      setOpen(false);
    }
  }

  return (
    // Portalled through the shared `Menu` (#114): this list used to be an
    // in-flow absolute child, one `overflow: hidden` ancestor away from being
    // clipped inside the sidebar card.
    <Menu
      open={open}
      onOpenChange={(next) => {
        setOpen(next);
        if (!next) {
          setCreating(false);
          setNewName("");
        }
      }}
    >
      <MenuTrigger
        className="no-drag w-full flex items-center gap-2 px-2 h-9 rounded-md text-sm border border-[var(--color-line)] bg-[var(--color-surface)] hover:border-[var(--color-text-muted)] transition-colors"
      >
        <span className="shrink-0 grid place-items-center w-5 h-5 rounded bg-[var(--color-pill-hover)] text-[var(--color-text-muted)]">
          {current ? <Users size={13} strokeWidth={1.5} /> : <User size={13} strokeWidth={1.5} />}
        </span>
        <span className="flex-1 min-w-0 text-left truncate">{label}</span>
        {current && syncStatus && <SyncIndicator status={syncStatus} />}
        {current && <RolePill role={current.role} />}
        <ChevronsUpDown size={14} strokeWidth={1.5} className="shrink-0 text-[var(--color-text-muted)]" />
      </MenuTrigger>

      <MenuContent
        aria-label="Switch workspace"
        className="min-w-[var(--radix-dropdown-menu-trigger-width)] max-w-none rounded-md shadow-xl py-1"
      >
        {/* Personal (local-only) */}
        <MenuItem className="px-2" onSelect={() => select("")}>
          <User size={14} strokeWidth={1.5} className="shrink-0 text-[var(--color-text-muted)]" />
          <span className="flex-1 min-w-0 truncate">Personal</span>
          {!current && <Check size={14} strokeWidth={1.5} className="shrink-0" />}
        </MenuItem>

        {status.logged_in && status.workspaces.length > 0 && (
          <>
            <MenuLabel className="pt-2 pb-1">Workspaces</MenuLabel>
            {status.workspaces.map((w) => (
              <MenuItem key={w.id} className="px-2" onSelect={() => select(w.id)}>
                <Users size={14} strokeWidth={1.5} className="shrink-0 text-[var(--color-text-muted)]" />
                <span className="flex-1 min-w-0 truncate">{w.name}</span>
                <RolePill role={w.role} />
                {current?.id === w.id && <Check size={14} strokeWidth={1.5} className="shrink-0" />}
              </MenuItem>
            ))}
          </>
        )}

        <MenuSeparator />

        {status.logged_in ? (
          creating ? (
            <div className="px-2 py-1">
              <input
                autoFocus
                value={newName}
                disabled={busy}
                onChange={(e) => setNewName(e.target.value)}
                // Keys stop here: the surrounding menu would otherwise read
                // them as typeahead and yank focus onto a matching row.
                onKeyDown={(e) => {
                  e.stopPropagation();
                  if (e.key === "Enter") createWorkspace();
                  else if (e.key === "Escape") {
                    setCreating(false);
                    setNewName("");
                  }
                }}
                onBlur={createWorkspace}
                placeholder="Workspace name"
                className="w-full text-sm px-2 py-1.5 rounded border border-[var(--color-text-muted)] bg-[var(--color-surface)]"
              />
            </div>
          ) : (
            <MenuItem
              className="px-2"
              // Swap the row for its input rather than closing the menu.
              onSelect={(e) => {
                e.preventDefault();
                setCreating(true);
              }}
            >
              <Plus size={14} strokeWidth={1.5} className="shrink-0" />
              <span>New workspace</span>
            </MenuItem>
          )
        ) : (
          <MenuItem
            className="px-2 text-[var(--color-interactive)]"
            onSelect={() => navigate("/settings?tab=account")}
          >
            <Users size={14} strokeWidth={1.5} className="shrink-0" />
            <span>Sign in to sync &amp; collaborate</span>
          </MenuItem>
        )}
      </MenuContent>
    </Menu>
  );
}
