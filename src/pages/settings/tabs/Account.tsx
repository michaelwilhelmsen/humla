import { useState } from "react";
import { Cloud, CloudOff, LogOut } from "lucide-react";
import { cloudApi, useCloudStore, HUMLA_CLOUD_URL } from "../../../lib/cloud";
import { Row, Section } from "../components/Section";
import { Btn } from "../components/Btn";
import { ValuePill } from "../components/ValuePill";
import { Toggle } from "../components/Toggle";
import type { SettingsHook } from "../useSettings";

const inputCls =
  "w-full text-sm px-3 py-2 rounded-md border border-[var(--color-line-visible)] bg-[var(--color-surface)] focus:border-[var(--color-text-muted)] transition-colors";

// Account section: connect to a Humla Cloud server, sign in/out. The server
// URL + credentials drive everything else (workspace management renders
// below, from Organization.tsx, once signed in). Local-only use needs none
// of this — it's entirely opt-in.
export function AccountTab({ s, update }: Pick<SettingsHook, "s" | "update">) {
  const status = useCloudStore((s) => s.status);
  const refresh = useCloudStore((s) => s.refresh);

  const [serverUrl, setServerUrl] = useState(status.base_url);
  const [mode, setMode] = useState<"signin" | "signup">("signin");
  const [name, setName] = useState("");
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [verifyMsg, setVerifyMsg] = useState<string | null>(null);
  const [resetMsg, setResetMsg] = useState<string | null>(null);

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
      // Don't mirror the URL into the self-hosted input — that field is for
      // custom servers only; the hosted URL showing up there reads as a bug.
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

  async function resendVerification() {
    setBusy(true);
    setVerifyMsg(null);
    try {
      await cloudApi.resendVerification();
      setVerifyMsg("Verification email sent — check your inbox.");
    } catch (e) {
      setVerifyMsg(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function forgotPassword() {
    const addr = email.trim();
    if (!addr) {
      setResetMsg("Enter your email above first, then try again.");
      return;
    }
    setBusy(true);
    setResetMsg(null);
    try {
      await cloudApi.requestPasswordReset(addr);
      // The server answers 204 even for unknown addresses (no account
      // enumeration), so this only promises the request went out.
      setResetMsg(`Password reset email sent to ${addr} — check your inbox.`);
    } catch (e) {
      setResetMsg(String(e));
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
  // (The active workspace is shown and managed in the Workspace section
  // rendered below this one — not duplicated here.)
  if (status.logged_in && status.user) {
    return (
      <Section title="Account">
        <div className="flex items-center justify-between gap-3 py-3.5">
          <div className="flex items-center gap-3 min-w-0">
            <div className="shrink-0 grid place-items-center w-10 h-10 rounded-full bg-[var(--color-pill-hover)] text-[var(--color-text-muted)]">
              <Cloud size={18} strokeWidth={1.5} />
            </div>
            <div className="min-w-0">
              <div className="text-sm truncate">{status.user.name || status.user.email}</div>
              <div className="text-xs text-[var(--color-text-muted)] truncate">{status.user.email}</div>
            </div>
          </div>
          <Btn onClick={signOut} disabled={busy}>
            <span className="inline-flex items-center gap-1.5">
              <LogOut size={14} strokeWidth={1.5} /> Sign out
            </span>
          </Btn>
        </div>
        {!status.user.verified && (
          <div className="py-3.5">
            <div className="rounded-md border border-[var(--color-warning)] bg-[var(--color-pill-hover)] px-3 py-2 text-xs flex flex-col gap-2">
              <div>
                <span style={{ color: "var(--color-warning)" }}>⚠</span>{" "}
                Your email isn't verified yet. Check your inbox for the verification link — pending
                team invites only take effect once you verify.
              </div>
              <div className="flex items-center gap-2 flex-wrap">
                <Btn onClick={resendVerification} disabled={busy}>
                  {busy ? "Sending…" : "Resend verification email"}
                </Btn>
                {verifyMsg && <span className="text-[var(--color-text-muted)]">{verifyMsg}</span>}
              </div>
            </div>
          </div>
        )}
        <Row label="Server" control={<ValuePill>{status.base_url}</ValuePill>} />
        <Row
          label="Upload recording audio"
          description="Send each recording's audio to its workspace note so teammates can play it back. Transcripts and summaries always sync; turning this off keeps the audio itself on your Mac. Personal notes never leave the device either way."
          control={
            <Toggle
              label="Upload recording audio"
              checked={s.sync_audio !== "false"}
              onChange={(on) => update("sync_audio", on ? "true" : "false")}
            />
          }
        />
        <Row
          label="Disconnect server"
          description="Forget this server and sign out. Cloud sync is opt-in — your notes always live locally first."
          control={
            <Btn onClick={disconnect} disabled={busy} aria-label="Disconnect server">
              <span className="inline-flex items-center gap-1.5">
                <CloudOff size={14} strokeWidth={1.5} /> Disconnect
              </span>
            </Btn>
          }
        />
      </Section>
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
        <Row label="Server" control={<ValuePill>{status.base_url}</ValuePill>} />
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
        <div className="py-3.5 flex flex-col gap-3">
          {error && <div className="text-xs text-[var(--color-danger)]">{error}</div>}
          <div className="flex gap-2">
            <Btn onClick={submit} disabled={busy || !canSubmit}>
              {busy ? (isSignup ? "Creating…" : "Signing in…") : isSignup ? "Create account" : "Sign in"}
            </Btn>
            <Btn onClick={disconnect} disabled={busy}>Change server</Btn>
          </div>
          <div className="flex items-center gap-4">
            <button type="button" onClick={toggleMode}
              className="text-xs text-[var(--color-interactive)] hover:underline">
              {isSignup ? "Already have an account? Sign in" : "Need an account? Create one"}
            </button>
            {!isSignup && (
              <button type="button" onClick={forgotPassword} disabled={busy}
                className="text-xs text-[var(--color-interactive)] hover:underline disabled:opacity-50">
                Forgot password?
              </button>
            )}
          </div>
          {resetMsg && (
            <div className="text-xs text-[var(--color-text-muted)]">{resetMsg}</div>
          )}
        </div>
      </Section>
    );
  }

  // Not configured -----------------------------------------------------------
  return (
    <Section title="Connect to sync">
      <div className="py-3.5 flex flex-col gap-3">
        <p className="text-sm text-[var(--color-text-muted)]">
          Humla works fully offline. Your notes always live on your Mac first.
        </p>
        {error && <div className="text-xs text-[var(--color-danger)]">{error}</div>}
        {/* Humla Cloud pitch: the one place the hosted offer gets to sell
            itself — accent-washed so it stands out from the surrounding
            settings chrome without shouting. */}
        <div className="rounded-lg border border-[var(--color-accent)] bg-[var(--color-accent-soft)] p-4 flex flex-col gap-1.5">
          <div className="flex items-center gap-1.5 text-sm font-medium">
            <Cloud size={15} strokeWidth={1.7} /> Humla Cloud
          </div>
          <p className="text-sm text-[var(--color-text-muted)]">
            Share notes across your team — synced to every device, backed up,
            and ready for teammates.
          </p>
          <div className="flex items-baseline gap-1.5 pt-1">
            <span className="text-2xl font-semibold tracking-tight">$5</span>
            <span className="text-xs text-[var(--color-text-muted)]">
              per seat, per month
            </span>
          </div>
          <div className="flex flex-wrap items-center gap-x-3 gap-y-1.5 pt-1.5">
            <button
              type="button"
              onClick={useHumlaCloud}
              disabled={busy}
              className="nd-btn nd-btn-primary"
            >
              {busy ? "Connecting…" : "Upgrade to Humla Cloud"}
            </button>
            <span className="text-xs text-[var(--color-text-muted)] whitespace-nowrap">
              14-day free trial · cancel anytime
            </span>
          </div>
        </div>
      </div>
      <Row label="Self-hosted server">
        <div className="flex gap-2">
          <input className={inputCls} type="url" value={serverUrl}
            onChange={(e) => setServerUrl(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && connect()}
            placeholder="https://sync.example.com" />
          <Btn onClick={connect} disabled={busy || !serverUrl.trim()}>
            {busy ? "Connecting…" : "Connect"}
          </Btn>
        </div>
      </Row>
    </Section>
  );
}
