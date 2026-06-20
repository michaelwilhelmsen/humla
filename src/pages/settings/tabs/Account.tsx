import { useState } from "react";
import { Cloud, CloudOff, LogOut } from "lucide-react";
import { cloudApi, useCloudStore, HUMLA_CLOUD_URL } from "../../../lib/cloud";
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
  const [mode, setMode] = useState<"signin" | "signup">("signin");
  const [name, setName] = useState("");
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

  // One-click path for downloaders: point at the hosted Humla Cloud and continue
  // to sign in / create an account — no URL to type.
  async function useHumlaCloud() {
    setBusy(true);
    setError(null);
    try {
      setServerUrl(HUMLA_CLOUD_URL);
      await cloudApi.configure(HUMLA_CLOUD_URL);
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

  async function signUp() {
    setBusy(true);
    setError(null);
    try {
      await cloudApi.signup(email.trim(), password, name.trim());
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
    const isSignup = mode === "signup";
    const submit = isSignup ? signUp : signIn;
    const canSubmit = !!email.trim() && !!password && (!isSignup || !!name.trim());
    const toggleMode = () => {
      setMode(isSignup ? "signin" : "signup");
      setError(null);
    };
    return (
      <Section title={isSignup ? "Create account" : "Sign in"}>
        <Row label="Server">
          <div className="text-sm text-[var(--color-text-muted)] break-all">{status.base_url}</div>
        </Row>
        {isSignup && (
          <Row label="Name">
            <input className={inputCls} type="text" autoComplete="name" value={name}
              onChange={(e) => setName(e.target.value)} placeholder="Your name" />
          </Row>
        )}
        <Row label="Email">
          <input className={inputCls} type="email" autoComplete="username" value={email}
            onChange={(e) => setEmail(e.target.value)} placeholder="you@example.com" />
        </Row>
        <Row label="Password">
          <input className={inputCls} type="password"
            autoComplete={isSignup ? "new-password" : "current-password"} value={password}
            onChange={(e) => setPassword(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && canSubmit && submit()}
            placeholder={isSignup ? "At least 8 characters" : "••••••••"} />
        </Row>
        {error && <div className="text-xs text-[var(--color-accent)]">{error}</div>}
        <div className="flex gap-2">
          <Btn onClick={submit} disabled={busy || !canSubmit}>
            {busy ? (isSignup ? "Creating…" : "Signing in…") : isSignup ? "Create account" : "Sign in"}
          </Btn>
          <Btn onClick={disconnect} disabled={busy}>Change server</Btn>
        </div>
        <div>
          <button type="button" onClick={toggleMode}
            className="text-xs text-[var(--color-interactive)] hover:underline">
            {isSignup ? "Already have an account? Sign in" : "Need an account? Create one"}
          </button>
        </div>
      </Section>
    );
  }

  // Not configured -----------------------------------------------------------
  return (
    <Section title="Connect to sync">
      <p className="text-sm text-[var(--color-text-muted)]">
        Humla works fully offline. To sync across devices and collaborate with a team, connect to
        <strong> Humla Cloud</strong> (hosted — easiest) or point Humla at your own server.
      </p>
      {error && <div className="text-xs text-[var(--color-accent)]">{error}</div>}
      <div className="flex items-center gap-2">
        <Btn onClick={useHumlaCloud} disabled={busy}>
          <span className="inline-flex items-center gap-1.5">
            <Cloud size={14} strokeWidth={1.5} /> {busy ? "Connecting…" : "Use Humla Cloud"}
          </span>
        </Btn>
        <span className="text-xs text-[var(--color-text-muted)]">14-day free trial · cancel anytime</span>
      </div>

      <p className="text-xs text-[var(--color-text-muted)] pt-2">
        Or connect your own self-hosted server:
      </p>
      <Row label="Server URL">
        <input className={inputCls} type="url" value={serverUrl}
          onChange={(e) => setServerUrl(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && connect()}
          placeholder="https://sync.example.com" />
      </Row>
      <div>
        <Btn onClick={connect} disabled={busy || !serverUrl.trim()}>
          {busy ? "Connecting…" : "Connect"}
        </Btn>
      </div>
    </Section>
  );
}
