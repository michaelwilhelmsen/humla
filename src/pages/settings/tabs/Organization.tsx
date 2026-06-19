import { useCallback, useEffect, useState } from "react";
import { Trash2, UserPlus } from "lucide-react";
import {
  cloudApi,
  roleColorVar,
  roleLabel,
  useCloudStore,
  type CloudMember,
  type CloudRole,
} from "../../../lib/cloud";
import { useNotesStore } from "../../../lib/store";
import { Row, Section } from "../components/Section";
import { Btn } from "../components/Btn";
import { Select } from "../components/Select";

const inputCls =
  "flex-1 min-w-0 text-sm px-3 py-2 rounded-md border border-[var(--color-line-visible)] bg-[var(--color-surface)] focus:border-[var(--color-text-muted)] transition-colors";

function RolePill({ role }: { role: CloudRole }) {
  return (
    <span
      className="shrink-0 px-1.5 py-0.5 text-[10px] uppercase tracking-[0.06em] rounded border"
      style={{ fontFamily: "var(--font-mono)", color: roleColorVar(role), borderColor: "var(--color-line)" }}
    >
      {roleLabel(role)}
    </span>
  );
}

export function OrganizationTab() {
  const status = useCloudStore((s) => s.status);
  const refreshCloud = useCloudStore((s) => s.refresh);
  const refreshNotes = useNotesStore((s) => s.refresh);

  const ws = status.current_workspace;
  const canManage = ws?.role === "owner" || ws?.role === "admin";

  const [members, setMembers] = useState<CloudMember[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [addEmail, setAddEmail] = useState("");
  const [busy, setBusy] = useState(false);
  const [newWs, setNewWs] = useState("");

  const loadMembers = useCallback(async (id: string) => {
    setLoading(true);
    setError(null);
    try {
      setMembers(await cloudApi.workspaceMembers(id));
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (ws?.id) loadMembers(ws.id);
    else setMembers([]);
  }, [ws?.id, loadMembers]);

  // Signed out --------------------------------------------------------------
  if (!status.logged_in) {
    return (
      <Section title="Organization">
        <p className="text-sm text-[var(--color-text-muted)]">
          Sign in from the <span className="text-[var(--color-text)]">Account</span> tab to create a
          workspace and invite your team.
        </p>
      </Section>
    );
  }

  // No active workspace -----------------------------------------------------
  if (!ws) {
    async function create() {
      const name = newWs.trim();
      if (!name) return;
      setBusy(true);
      try {
        await cloudApi.createWorkspace(name);
        setNewWs("");
        await refreshCloud();
        await refreshNotes();
      } finally {
        setBusy(false);
      }
    }
    return (
      <Section title="Organization">
        <p className="text-sm text-[var(--color-text-muted)]">
          You're in Personal (local-only) mode. Create a workspace to start collaborating, or pick
          one from the switcher at the top of the sidebar.
        </p>
        {status.workspaces.length > 0 && (
          <Row label="Your workspaces">
            <div className="flex flex-col gap-1">
              {status.workspaces.map((w) => (
                <button
                  key={w.id}
                  onClick={async () => {
                    await cloudApi.selectWorkspace(w.id);
                    await refreshCloud();
                    await refreshNotes();
                  }}
                  className="flex items-center gap-2 px-3 py-2 rounded-md text-sm text-left border border-[var(--color-line)] hover:border-[var(--color-text-muted)] transition-colors"
                >
                  <span className="flex-1 truncate">{w.name}</span>
                  <RolePill role={w.role} />
                </button>
              ))}
            </div>
          </Row>
        )}
        <Row label="New workspace">
          <div className="flex gap-2">
            <input
              className={inputCls}
              value={newWs}
              onChange={(e) => setNewWs(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && create()}
              placeholder="Acme Inc"
            />
            <Btn onClick={create} disabled={busy || !newWs.trim()}>Create</Btn>
          </div>
        </Row>
      </Section>
    );
  }

  // Active workspace → membership management --------------------------------
  async function addMember() {
    const email = addEmail.trim();
    if (!email || !ws) return;
    setBusy(true);
    setError(null);
    try {
      await cloudApi.addMember(ws.id, email);
      setAddEmail("");
      await loadMembers(ws.id);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function changeRole(userId: string, role: CloudRole) {
    if (!ws) return;
    setBusy(true);
    try {
      await cloudApi.setMemberRole(ws.id, userId, role);
      await loadMembers(ws.id);
    } finally {
      setBusy(false);
    }
  }

  async function remove(userId: string) {
    if (!ws) return;
    setBusy(true);
    try {
      await cloudApi.removeMember(ws.id, userId);
      await loadMembers(ws.id);
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <Section title="Workspace">
        <Row label="Name">
          <div className="text-sm">{ws.name}</div>
        </Row>
        <Row label="Your role">
          <div className="flex items-center gap-2">
            <RolePill role={ws.role} />
            <span className="text-xs text-[var(--color-text-muted)]">
              {ws.role === "owner"
                ? "Full control, including billing and deletion."
                : ws.role === "admin"
                ? "Can manage members and settings."
                : "Can create and edit notes."}
            </span>
          </div>
        </Row>
      </Section>

      <Section title={`Members${members.length ? ` · ${members.length}` : ""}`}>
        {loading && <div className="text-sm text-[var(--color-text-muted)]">Loading…</div>}
        {error && <div className="text-xs text-[var(--color-accent)]">{error}</div>}

        <div className="flex flex-col gap-1">
          {members.map((m) => (
            <div
              key={m.id}
              className="flex items-center gap-3 px-3 py-2 rounded-md border border-[var(--color-line)]"
            >
              <div className="shrink-0 grid place-items-center w-8 h-8 rounded-full bg-[var(--color-pill-hover)] text-[var(--color-text-muted)] text-xs uppercase">
                {(m.name || m.email).slice(0, 1)}
              </div>
              <div className="flex-1 min-w-0">
                <div className="text-sm truncate">{m.name || m.email}</div>
                {m.name && <div className="text-xs text-[var(--color-text-muted)] truncate">{m.email}</div>}
              </div>
              {canManage && m.role !== "owner" ? (
                <Select
                  value={m.role}
                  onChange={(v) => changeRole(m.id, v as CloudRole)}
                  options={[
                    { value: "member", label: "Member" },
                    { value: "admin", label: "Admin" },
                  ]}
                />
              ) : (
                <RolePill role={m.role} />
              )}
              {canManage && m.role !== "owner" && (
                <button
                  onClick={() => remove(m.id)}
                  disabled={busy}
                  aria-label="Remove member"
                  title="Remove from workspace"
                  className="shrink-0 p-1.5 rounded text-[var(--color-text-muted)] hover:text-[var(--color-accent)] hover:bg-[var(--color-pill-hover)] transition-colors"
                >
                  <Trash2 size={14} strokeWidth={1.5} />
                </button>
              )}
            </div>
          ))}
        </div>

        {canManage && (
          <Row label="Add member by email">
            <div className="flex gap-2">
              <input
                className={inputCls}
                type="email"
                value={addEmail}
                onChange={(e) => setAddEmail(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && addMember()}
                placeholder="teammate@example.com"
              />
              <Btn onClick={addMember} disabled={busy || !addEmail.trim()}>
                <span className="inline-flex items-center gap-1.5">
                  <UserPlus size={14} strokeWidth={1.5} /> Add
                </span>
              </Btn>
            </div>
            <p className="text-xs text-[var(--color-text-muted)] mt-2">
              They need a Humla account on this server. Email invites for new people are coming soon.
            </p>
          </Row>
        )}
      </Section>
    </>
  );
}
