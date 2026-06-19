import { useState } from "react";
import { Cloud, CloudOff, LogOut } from "lucide-react";
import { cloudApi, useCloudStore } from "../../../lib/cloud";
import { Row, Section } from "../components/Section";
import { Btn } from "../components/Btn";

const inputCls =
  "w-full text-sm px-3 py-2 rounded-md border border-[var(--color-line-visible)] bg-[var(--color-surface)] focus:border-[var(--color-text-muted)] transition-colors";

// Account tab: connect to a Humla Cloud server, sign in/out. The server URL +
// credentials drive everything else (workspaces, members). Local-only use
// needs none of this — it's entirely opt-in.
export function AccountTab() {
  const status = useCloudStore((s) => s.status);
  const refresh = useCloudStore((s) => s.refresh);

  const [serverUrl, setServerUrl] = useState(status.base_url);
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function connect() {
    setBusy(true);
    setError(null);
    try {
      await cloudApi.configure(serverUrl.trim());
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function signIn() {
    setBusy(true);
    setError(null);
    try {
      await cloudApi.login(email.trim(), password);
      setPassword("");
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function signOut() {
    setBusy(true);
    try {
      await cloudApi.logout();
      await refresh();
    } finally {
      setBusy(false);
    }
  }

  async function disconnect() {
    setBusy(true);
    try {
      await cloudApi.logout();
      await cloudApi.configure("");
      setServerUrl("");
      await refresh();
    } finally {
      setBusy(false);
    }
  }

  // Signed in ----------------------------------------------------------------
  if (status.logged_in && status.user) {
    return (
      <>
        <Section title="Account">
          <div className="flex items-center gap-3">
            <div className="shrink-0 grid place-items-center w-11 h-11 rounded-full bg-[var(--color-pill-hover)] text-[var(--color-text-muted)]">
              <Cloud size={20} strokeWidth={1.5} />
            </div>
            <div className="min-w-0">
              <div className="text-sm truncate">{status.user.name || status.user.email}</div>
              <div className="text-xs text-[var(--color-text-muted)] truncate">{status.user.email}</div>
            </div>
          </div>
          <Row label="Server">
            <div className="text-sm text-[var(--color-text-muted)] break-all">{status.base_url}</div>
          </Row>
          <Row label="Active workspace">
            <div className="text-sm text-[var(--color-text-muted)]">
              {status.current_workspace ? status.current_workspace.name : "Personal (local only)"}
            </div>
          </Row>
          <div className="flex gap-2">
            <Btn onClick={signOut} disabled={busy}>
              <span className="inline-flex items-center gap-1.5">
                <LogOut size={14} strokeWidth={1.5} /> Sign out
              </span>
            </Btn>
            <Btn onClick={disconnect} disabled={busy}>
              <span className="inline-flex items-center gap-1.5">
                <CloudOff size={14} strokeWidth={1.5} /> Disconnect server
              </span>
            </Btn>
          </div>
        </Section>
        <p className="text-xs text-[var(--color-text-muted)] -mt-6">
          Cloud sync is opt-in. Your notes always live locally first; signing in lets you sync them
          to a workspace and collaborate with a team.
        </p>
      </>
    );
  }

  // Configured but signed out ------------------------------------------------
  if (status.configured) {
    return (
      <Section title="Sign in">
        <Row label="Server">
          <div className="text-sm text-[var(--color-text-muted)] break-all">{status.base_url}</div>
        </Row>
        <Row label="Email">
          <input className={inputCls} type="email" autoComplete="username" value={email}
            onChange={(e) => setEmail(e.target.value)} placeholder="you@example.com" />
        </Row>
        <Row label="Password">
          <input className={inputCls} type="password" autoComplete="current-password" value={password}
            onChange={(e) => setPassword(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && signIn()} placeholder="••••••••" />
        </Row>
        {error && <div className="text-xs text-[var(--color-accent)]">{error}</div>}
        <div className="flex gap-2">
          <Btn onClick={signIn} disabled={busy || !email.trim() || !password}>
            {busy ? "Signing in…" : "Sign in"}
          </Btn>
          <Btn onClick={disconnect} disabled={busy}>Change server</Btn>
        </div>
      </Section>
    );
  }

  // Not configured -----------------------------------------------------------
  return (
    <Section title="Connect to Humla Cloud">
      <p className="text-sm text-[var(--color-text-muted)]">
        Humla works fully offline. Connect a Humla Cloud server to sync your notes across devices
        and collaborate with a team. You host it yourself — your data, your server.
      </p>
      <Row label="Server URL">
        <input className={inputCls} type="url" value={serverUrl}
          onChange={(e) => setServerUrl(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && connect()}
          placeholder="https://sync.example.com" />
      </Row>
      {error && <div className="text-xs text-[var(--color-accent)]">{error}</div>}
      <div>
        <Btn onClick={connect} disabled={busy || !serverUrl.trim()}>
          {busy ? "Connecting…" : "Connect"}
        </Btn>
      </div>
    </Section>
  );
}
