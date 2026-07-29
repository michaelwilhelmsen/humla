import { useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";
import { Check, ChevronsUpDown, Cloud, CloudOff, Plus, RefreshCw, User, Users } from "lucide-react";
import { cloudApi, roleLabel, useCloudStore, type CloudRole } from "../lib/cloud";
import { useNotesStore } from "../lib/store";
import type { SyncStatus } from "../lib/ipc";

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

const PILL_CLS =
  "shrink-0 px-1 text-[9px] uppercase tracking-[0.08em] rounded border border-[var(--color-line)] text-[var(--color-text-muted)]";

function RolePill({ role }: { role: CloudRole }) {
  return <span className={PILL_CLS}>{roleLabel(role)}</span>;
}

// The only ambient hint that team workspaces exist. It occupies the trigger
// row's pill slot, which is dead space on Personal — and it disappears for good
// once there's a workspace, because the slot becomes the RolePill. So there's no
// dismiss state to persist and nothing to nag: having a team removes the hint.
// Deliberately says nothing about price or the trial (that lives in Settings →
// Account, next to the button that actually starts one) and avoids "upgrade" —
// Personal is a choice, not a lesser tier.
function AddTeamPill() {
  const navigate = useNavigate();
  return (
    <span
      role="button"
      tabIndex={0}
      title="Sync notes across your team"
      onClick={(e) => {
        // The trigger button wraps this, so its onClick would toggle the
        // dropdown right back open behind the navigation.
        e.stopPropagation();
        navigate("/settings?tab=account");
      }}
      onKeyDown={(e) => {
        if (e.key !== "Enter" && e.key !== " ") return;
        e.stopPropagation();
        e.preventDefault();
        navigate("/settings?tab=account");
      }}
      className={`${PILL_CLS} cursor-pointer hover:text-[var(--color-text)] hover:border-[var(--color-text-muted)] transition-colors`}
    >
      Add team
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
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onClick = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && setOpen(false);
    window.addEventListener("mousedown", onClick);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", onClick);
      window.removeEventListener("keydown", onKey);
    };
  }, [open]);

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
    <div className="no-drag relative" ref={ref}>
      <button
        onClick={() => setOpen((v) => !v)}
        className="w-full flex items-center gap-2 px-2 h-9 rounded-md text-sm border border-[var(--color-line)] bg-[var(--color-surface)] hover:border-[var(--color-text-muted)] transition-colors"
      >
        <span className="shrink-0 grid place-items-center w-5 h-5 rounded bg-[var(--color-pill-hover)] text-[var(--color-text-muted)]">
          {current ? <Users size={13} strokeWidth={1.5} /> : <User size={13} strokeWidth={1.5} />}
        </span>
        <span className="flex-1 min-w-0 text-left truncate">{label}</span>
        {current && syncStatus && <SyncIndicator status={syncStatus} />}
        {current ? <RolePill role={current.role} /> : <AddTeamPill />}
        <ChevronsUpDown size={14} strokeWidth={1.5} className="shrink-0 text-[var(--color-text-muted)]" />
      </button>

      {open && (
        <div className="absolute left-0 right-0 top-full mt-1 z-30 rounded-md border border-[var(--color-line-visible)] bg-[var(--color-canvas)] shadow-xl py-1">
          {/* Personal (local-only) */}
          <button
            onClick={() => select("")}
            className="w-full flex items-center gap-2 px-2 py-1.5 text-sm text-left hover:bg-[var(--color-pill-hover)] transition-colors"
          >
            <User size={14} strokeWidth={1.5} className="shrink-0 text-[var(--color-text-muted)]" />
            <span className="flex-1 min-w-0 truncate">Personal</span>
            {!current && <Check size={14} strokeWidth={1.5} className="shrink-0" />}
          </button>

          {status.logged_in && status.workspaces.length > 0 && (
            <>
              <div className="nd-label px-2 pt-2 pb-1">Workspaces</div>
              {status.workspaces.map((w) => (
                <button
                  key={w.id}
                  onClick={() => select(w.id)}
                  className="w-full flex items-center gap-2 px-2 py-1.5 text-sm text-left hover:bg-[var(--color-pill-hover)] transition-colors"
                >
                  <Users size={14} strokeWidth={1.5} className="shrink-0 text-[var(--color-text-muted)]" />
                  <span className="flex-1 min-w-0 truncate">{w.name}</span>
                  <RolePill role={w.role} />
                  {current?.id === w.id && <Check size={14} strokeWidth={1.5} className="shrink-0" />}
                </button>
              ))}
            </>
          )}

          <div className="my-1 border-t border-[var(--color-line)]" />

          {status.logged_in ? (
            creating ? (
              <div className="px-2 py-1">
                <input
                  autoFocus
                  value={newName}
                  disabled={busy}
                  onChange={(e) => setNewName(e.target.value)}
                  onKeyDown={(e) => {
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
              <button
                onClick={() => setCreating(true)}
                className="w-full flex items-center gap-2 px-2 py-1.5 text-sm text-left text-[var(--color-text-muted)] hover:bg-[var(--color-pill-hover)] hover:text-[var(--color-text)] transition-colors"
              >
                <Plus size={14} strokeWidth={1.5} className="shrink-0" />
                <span>New workspace</span>
              </button>
            )
          ) : (
            <button
              onClick={() => {
                setOpen(false);
                navigate("/settings?tab=account");
              }}
              className="w-full flex items-center gap-2 px-2 py-1.5 text-sm text-left text-[var(--color-interactive)] hover:bg-[var(--color-pill-hover)] transition-colors"
            >
              <Users size={14} strokeWidth={1.5} className="shrink-0" />
              <span>Sign in to sync &amp; collaborate</span>
            </button>
          )}
        </div>
      )}
    </div>
  );
}
