import { useState } from "react";
import { Check, ChevronsUpDown, Cloud, CloudOff, Plus, RefreshCw, User, Users } from "lucide-react";
import { cloudApi, roleLabel, useCloudStore, type CloudRole } from "../lib/cloud";
import { useNotesStore } from "../lib/store";
import type { SyncStatus } from "../lib/ipc";
import { Menu, MenuContent, MenuItem, MenuLabel, MenuSeparator, MenuTrigger } from "./ui/Menu";
import { NewWorkspaceModal } from "./NewWorkspaceModal";

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
// active workspace (or "Personal" for local-only), and a dropdown to switch or
// create. Signed out it still offers both rows — "Create team workspace" opens
// the create sheet, which asks for an account as its first stage — so the chrome
// is identical across cloud states and no state is a dead end.
export function WorkspaceSwitcher() {
  const status = useCloudStore((s) => s.status);
  const syncStatus = useCloudStore((s) => s.syncStatus);
  const refreshCloud = useCloudStore((s) => s.refresh);
  const refreshNotes = useNotesStore((s) => s.refresh);
  const [open, setOpen] = useState(false);
  const [creatingOpen, setCreatingOpen] = useState(false);

  const current = status.current_workspace;
  const label = current?.name ?? "Personal";

  async function select(id: string) {
    try {
      await cloudApi.selectWorkspace(id);
      await refreshCloud();
      await refreshNotes();
    } catch {
      /* surfaced elsewhere; keep the switcher resilient */
    } finally {
      setOpen(false);
    }
  }

  return (
    <>
      {/* Portalled through the shared `Menu` (#114): this list used to be an
          in-flow absolute child, one `overflow: hidden` ancestor away from being
          clipped inside the sidebar card. */}
      <Menu open={open} onOpenChange={setOpen}>
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

          {/* Offered whether or not you're signed in: signing in is the sheet's
              first stage, so there's one path from "I want a workspace" to having
              a working one. The row used to be swapped for a bare text input that
              committed on blur — and, signed out, was replaced by a pointer to
              Settings. "Team" is what distinguishes this from a folder. */}
          <MenuItem className="px-2" onSelect={() => setCreatingOpen(true)}>
            <Plus size={14} strokeWidth={1.5} className="shrink-0" />
            <span>Create team workspace</span>
          </MenuItem>
        </MenuContent>
      </Menu>
      <NewWorkspaceModal open={creatingOpen} onClose={() => setCreatingOpen(false)} />
    </>
  );
}
