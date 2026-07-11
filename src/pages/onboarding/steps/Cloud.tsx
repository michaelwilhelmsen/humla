// Step 6 — Humla Cloud (design/ONBOARDING.md § 6. Humla Cloud).
//
// Cloud = teams only. Every cloud feature sits behind the workspace
// subscription, so the pitch is honest about that up front. The whole point of
// this screen is that it must NOT smell like a paywall: the FREE path is the
// preselected default, and Continue on it does zero network work. Nobody who
// just wants Humla on their Mac is ever nudged toward an account.
//
// The step is a fork with three deliberate options plus a subordinate
// self-hosted escape hatch:
//
//   1. "Just me, on this Mac"  (preselected) — one click onward, no account,
//      nothing phoned home.
//   2. "Set up a team workspace" — the full funnel: configure the hosted server
//      → inline signup → name + create workspace → Stripe Checkout → poll
//      cloud_status until the subscription goes live → success.
//   3. "Join an existing team" — configure → sign in (or create an account via
//      the inline auth toggle) → pick a workspace, or — for a brand-new user
//      whose admin hasn't added them yet — a calm "ask your admin" note with
//      workspace creation demoted to the fallback.
//   + Self-hosted — a small text link (not a card) that reveals a server-URL
//      input; from there the same sign-up / sign-in forms apply.
//
// Everything is skippable at every sub-step: Continue always advances. Cloud
// NEVER nags — a half-finished funnel (signed up but abandoned checkout, say)
// degrades gracefully to Settings → Account, which handles leftover
// billing state. That's why bailing mid-funnel is always allowed here.
//
// Write-through philosophy: there is no local cloud state to persist. Server /
// session / workspace / plan state all live in the backend and are surfaced
// through useCloudStore; this component reads that store on mount (so a re-run
// of the wizard by an already-signed-in user shows the signed-in state) and
// refreshes it after each mutation. The only thing this step "commits" is
// advancing the wizard cursor via ctx.goNext.
//
// ─────────────────────────── sub-state machine ───────────────────────────
// `option`: which card is active — "solo" (default) | "team" | "existing".
//   - "solo": no funnel; Continue advances immediately.
//   - "team"/"existing": a funnel whose STAGE is DERIVED from live cloud
//     status (not stored), so it's inherently resumable and can't drift out of
//     sync with the backend:
//       not configured        → show connect (auto one-click Humla Cloud, or
//                                the self-hosted URL if that link was opened)
//       configured, !loggedIn → auth form (signup for "team", login for
//                                "existing")
//       loggedIn, no workspace→ name+create workspace ("team") / create-or-pick
//                                ("existing")
//       loggedIn, workspace,
//         plan not live        → "Start free trial" → checkout → poll
//       loggedIn, workspace,
//         plan trialing/active → success
// `checkout`: "idle" | "waiting" | "timeout" — overlays the trial sub-stage
//   while we poll cloud_status after opening Stripe. "timeout" is a CALM note,
//   never an error (billing can be finished later in Settings).
import { useEffect, useRef, useState } from "react";
import { open as openExternal } from "@tauri-apps/plugin-shell";
import {
  Cloud,
  User,
  Users,
  LogIn,
  Check,
  ArrowRight,
  ExternalLink,
  Server,
} from "lucide-react";
import {
  cloudApi,
  useCloudStore,
  HUMLA_CLOUD_URL,
  type CloudWorkspace,
} from "../../../lib/cloud";
import type { StepContext } from "../types";
import { StepShell } from "../StepShell";

// Poll cadence + soft ceiling while waiting for Stripe Checkout to complete.
// The ceiling is deliberately generous (checkout can involve typing a card,
// 3D-Secure, etc.) and never turns into an error — just a calm "finish later".
const CHECKOUT_POLL_MS = 3000;
const CHECKOUT_TIMEOUT_MS = 5 * 60 * 1000;

type Option = "solo" | "team" | "existing";
type CheckoutState = "idle" | "waiting" | "timeout";

const inputCls =
  "w-full min-w-0 px-3 py-2 rounded-md text-sm bg-[var(--color-input-bg)] border border-[var(--color-line)] focus:border-[var(--color-text-muted)]";

function planIsLive(ws: CloudWorkspace | null | undefined): boolean {
  return ws?.plan_status === "trialing" || ws?.plan_status === "active";
}

export function CloudStep({ ctx }: { ctx: StepContext }) {
  const status = useCloudStore((s) => s.status);
  const ready = useCloudStore((s) => s.ready);
  const refresh = useCloudStore((s) => s.refresh);

  // Free path is the preselected default — this is what keeps the step from
  // reading as a paywall.
  const [option, setOption] = useState<Option>("solo");

  // Pull live cloud status on mount so an already-signed-in user (re-running
  // the wizard) sees their real state, and so the funnel stage derives from
  // truth rather than a stale snapshot. The store may already be `ready` from
  // app boot; refreshing anyway is cheap and keeps this self-contained.
  useEffect(() => {
    void refresh();
  }, [refresh]);

  // If the user arrives already logged in, default the visible option to the
  // relevant funnel so the signed-in state is what they see first — but only
  // once, on the first ready snapshot, so it never fights a later manual pick.
  const seededRef = useRef(false);
  useEffect(() => {
    if (!ready || seededRef.current) return;
    seededRef.current = true;
    if (status.logged_in) {
      // Signed in already → show them the team funnel, which renders their
      // current workspace / plan / create-workspace state.
      setOption("team");
    }
  }, [ready, status.logged_in]);

  if (!ready) {
    return (
      <StepShell
        icon={<Cloud size={26} strokeWidth={1.6} />}
        title="Humla Cloud"
        subtitle="Humla is free on your Mac — Cloud is for teams: $5 per seat/mo after a 14-day trial."
      />
    );
  }

  return (
    <StepShell
      icon={<Cloud size={26} strokeWidth={1.6} />}
      title="Humla Cloud"
      subtitle="Humla is free on your Mac — Cloud is for teams: $5 per seat/mo after a 14-day trial."
    >
      <div className="w-full max-w-md flex flex-col gap-3 text-left">
        {/* Option 1 — Just me (free, preselected). */}
        <OptionCard
          icon={<User size={18} strokeWidth={1.8} />}
          title="Just me, on this Mac"
          blurb="Free forever. No account, nothing leaves your Mac."
          selected={option === "solo"}
          onSelect={() => setOption("solo")}
        />

        {/* Option 2 — Team workspace (the funnel). */}
        <OptionCard
          icon={<Users size={18} strokeWidth={1.8} />}
          title="Set up a team workspace"
          blurb="Sync and collaborate with your team. 14-day trial · $5 per seat/mo · cancel anytime."
          selected={option === "team"}
          onSelect={() => setOption("team")}
        >
          {option === "team" && <TeamFunnel ctx={ctx} mode="signup" />}
        </OptionCard>

        {/* Option 3 — Join a team that already uses Humla (account or not). */}
        <OptionCard
          icon={<LogIn size={18} strokeWidth={1.8} />}
          title="Join an existing team"
          blurb="Your team already uses Humla Cloud. Sign in — or create an account and get added by your workspace admin."
          selected={option === "existing"}
          onSelect={() => setOption("existing")}
        >
          {option === "existing" && <TeamFunnel ctx={ctx} mode="signin" />}
        </OptionCard>
      </div>

      {/* Continue — always enabled. On the free path this advances with zero
          network calls; on a team path it advances regardless of how far the
          funnel got (Settings → Account handles leftover state). Cloud
          never blocks the wizard. */}
      <div className="mt-8 w-full max-w-md flex flex-col items-center gap-3">
        <button
          type="button"
          className="nd-btn nd-btn-primary"
          onClick={ctx.goNext}
        >
          Continue
          <ArrowRight size={15} strokeWidth={2} />
        </button>
        {option === "solo" && (
          <p className="text-xs text-[var(--color-text-muted)] text-center max-w-xs">
            You can set up a team workspace anytime in Settings → Account.
          </p>
        )}
      </div>
    </StepShell>
  );
}

// ─────────────────────────────── Option card ────────────────────────────────

function OptionCard({
  icon,
  title,
  blurb,
  selected,
  onSelect,
  children,
}: {
  icon: React.ReactNode;
  title: string;
  blurb: string;
  selected: boolean;
  onSelect: () => void;
  children?: React.ReactNode;
}) {
  return (
    <div
      className={
        "rounded-[var(--radius)] border px-4 py-4 transition-colors " +
        (selected
          ? "border-[var(--color-accent)] bg-[var(--color-accent-soft)]"
          : "border-[var(--color-line)] bg-[var(--color-surface)]")
      }
    >
      <button type="button" onClick={onSelect} className="text-left w-full">
        <div className="flex items-start gap-3">
          <span className="mt-0.5 shrink-0 text-[var(--color-text-muted)]">
            {icon}
          </span>
          <div className="min-w-0 flex-1">
            <span className="text-sm font-semibold text-[var(--color-text)]">
              {title}
            </span>
            <p className="mt-1 text-xs leading-relaxed text-[var(--color-text-muted)]">
              {blurb}
            </p>
          </div>
        </div>
      </button>
      {children && <div className="mt-4">{children}</div>}
    </div>
  );
}

// ─────────────────────────────── Team funnel ────────────────────────────────
// One component drives both the "team" (signup) and "existing" (signin) paths.
// The visible stage is DERIVED from live cloud status, so it's resumable and
// self-healing: whatever the backend actually reflects is what we render.

function TeamFunnel({
  ctx,
  mode,
}: {
  ctx: StepContext;
  mode: "signup" | "signin";
}) {
  const status = useCloudStore((s) => s.status);
  const ws = status.current_workspace;

  // Stage 1 — server not configured.
  if (!status.configured) {
    return <ConnectStage />;
  }

  // Stage 2 — configured but signed out → auth.
  if (!status.logged_in) {
    return <AuthStage mode={mode} />;
  }

  // Stage 3 — signed in, no active workspace → create (or pick, if any exist).
  // On the join path, an empty workspace list means "wait for your admin", so
  // the stage needs to know which card it's serving.
  if (!ws) {
    return <WorkspaceStage mode={mode} />;
  }

  // Stage 4 — signed in with a workspace whose plan isn't live yet → trial.
  if (!planIsLive(ws)) {
    return <TrialStage ws={ws} ctx={ctx} />;
  }

  // Stage 5 — done: subscription is live.
  return <SuccessStage ws={ws} />;
}

// ── Stage: connect the server ────────────────────────────────────────────────
// Mirrors Account.tsx's one-click "Use Humla Cloud". The self-hosted case is a
// subordinate text link that reveals a URL input (visually below the primary
// action, never a card).

function ConnectStage() {
  const refresh = useCloudStore((s) => s.refresh);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [showSelfHost, setShowSelfHost] = useState(false);
  const [serverUrl, setServerUrl] = useState("");

  async function connect(url: string) {
    const trimmed = url.trim();
    if (!trimmed) return;
    setBusy(true);
    setError(null);
    try {
      await cloudApi.configure(trimmed);
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex flex-col gap-3">
      <button
        type="button"
        onClick={() => connect(HUMLA_CLOUD_URL)}
        disabled={busy}
        className="nd-btn nd-btn-primary self-start"
      >
        <Cloud size={14} strokeWidth={2} />
        {busy ? "Connecting…" : "Use Humla Cloud"}
      </button>

      {error && <p className="text-xs text-[var(--color-danger)] break-all">{error}</p>}

      {/* Self-hosted — subordinate text link, not a card. */}
      {!showSelfHost ? (
        <button
          type="button"
          onClick={() => setShowSelfHost(true)}
          className="self-start text-xs text-[var(--color-text-muted)] hover:text-[var(--color-text)] inline-flex items-center gap-1.5 transition-colors"
        >
          <Server size={12} strokeWidth={2} />
          Use a self-hosted server instead
        </button>
      ) : (
        <div className="flex flex-col gap-2 pt-1">
          <p className="text-xs text-[var(--color-text-muted)]">
            Point Humla at your own server:
          </p>
          <div className="flex gap-2">
            <input
              type="url"
              value={serverUrl}
              onChange={(e) => setServerUrl(e.target.value)}
              onKeyDown={(e) =>
                e.key === "Enter" && serverUrl.trim() && connect(serverUrl)
              }
              placeholder="https://sync.example.com"
              className={inputCls}
            />
            <button
              type="button"
              onClick={() => connect(serverUrl)}
              disabled={busy || !serverUrl.trim()}
              className="nd-btn shrink-0"
            >
              {busy ? "Connecting…" : "Connect"}
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

// ── Stage: auth (signup for the team path, login for the existing path) ───────
// Reuses the exact command wrappers + error-message patterns from Account.tsx.
// The card only picks the STARTING form: a new user on the join path has no
// account yet, and an existing user can land on the team card — either way the
// other form must be one toggle away, never a dead end.

function AuthStage({ mode: initialMode }: { mode: "signup" | "signin" }) {
  const status = useCloudStore((s) => s.status);
  const refresh = useCloudStore((s) => s.refresh);

  const [mode, setMode] = useState(initialMode);
  const [name, setName] = useState("");
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const isSignup = mode === "signup";

  function toggleMode() {
    setMode(isSignup ? "signin" : "signup");
    setError(null);
  }
  const canSubmit =
    !!email.trim() && !!password && (!isSignup || !!name.trim());

  async function submit() {
    if (!canSubmit) return;
    setBusy(true);
    setError(null);
    try {
      if (isSignup) {
        await cloudApi.signup(email.trim(), password, name.trim());
      } else {
        await cloudApi.login(email.trim(), password);
      }
      setPassword("");
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex flex-col gap-3">
      <p className="text-xs text-[var(--color-text-muted)] break-all">
        Server: {status.base_url}
      </p>
      {isSignup && (
        <input
          type="text"
          autoComplete="name"
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="Your name"
          className={inputCls}
        />
      )}
      <input
        type="email"
        autoComplete="username"
        value={email}
        onChange={(e) => setEmail(e.target.value)}
        placeholder="you@example.com"
        className={inputCls}
      />
      <input
        type="password"
        autoComplete={isSignup ? "new-password" : "current-password"}
        value={password}
        onChange={(e) => setPassword(e.target.value)}
        onKeyDown={(e) => e.key === "Enter" && submit()}
        placeholder={isSignup ? "At least 8 characters" : "••••••••"}
        className={inputCls}
      />
      {error && <p className="text-xs text-[var(--color-danger)] break-all">{error}</p>}
      <div className="flex items-center gap-3">
        <button
          type="button"
          onClick={submit}
          disabled={busy || !canSubmit}
          className="nd-btn nd-btn-primary"
        >
          {busy
            ? isSignup
              ? "Creating…"
              : "Signing in…"
            : isSignup
              ? "Create account"
              : "Sign in"}
        </button>
        <button
          type="button"
          onClick={toggleMode}
          className="text-xs text-[var(--color-text-muted)] hover:text-[var(--color-text)] underline transition-colors"
        >
          {isSignup ? "Already have an account? Sign in" : "New here? Create an account"}
        </button>
      </div>
    </div>
  );
}

// ── Stage: workspace (create, or pick one you already belong to) ─────────────
// Mirrors Organization.tsx's create flow: create → selects automatically on the
// backend (cloud_create_workspace returns + the store refresh surfaces it as
// current_workspace). If any workspaces already exist, list them and select on
// click (cloud_select_workspace), matching Organization.tsx.

function WorkspaceStage({ mode }: { mode: "signup" | "signin" }) {
  const status = useCloudStore((s) => s.status);
  const refresh = useCloudStore((s) => s.refresh);

  const [newWs, setNewWs] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Join path with nothing to pick: the expected next actor is the team's
  // workspace admin, not this user. Say so calmly and demote creation to a
  // fallback — the store's workspace watcher surfaces the "Added to…" moment
  // whenever the admin gets around to it.
  const joining = mode === "signin" && status.workspaces.length === 0;

  async function create() {
    const name = newWs.trim();
    if (!name) return;
    setBusy(true);
    setError(null);
    try {
      const created = await cloudApi.createWorkspace(name);
      setNewWs("");
      // Refresh surfaces current_workspace. If the API didn't auto-select the
      // new workspace, select it explicitly (Organization.tsx relies on the
      // refresh alone, but selecting is idempotent and covers both servers).
      await refresh();
      if (useCloudStore.getState().status.current_workspace?.id !== created.id) {
        await cloudApi.selectWorkspace(created.id);
        await refresh();
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function pick(id: string) {
    setBusy(true);
    setError(null);
    try {
      await cloudApi.selectWorkspace(id);
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex flex-col gap-3">
      {joining && (
        <div className="flex flex-col gap-1.5">
          <p className="text-xs text-[var(--color-text-muted)] flex items-center gap-2">
            <Check size={14} strokeWidth={2.5} className="text-[var(--color-success)] shrink-0" />
            Signed in{status.user ? ` as ${status.user.email}` : ""}.
          </p>
          <p className="text-xs leading-relaxed text-[var(--color-text-muted)]">
            Ask your workspace admin to add{" "}
            {status.user ? status.user.email : "your email"} — workspaces you're
            added to appear automatically. You can finish setup now.
          </p>
        </div>
      )}

      {status.workspaces.length > 0 && (
        <div className="flex flex-col gap-1">
          <p className="text-xs text-[var(--color-text-muted)]">Your workspaces:</p>
          {status.workspaces.map((w) => (
            <button
              key={w.id}
              type="button"
              onClick={() => pick(w.id)}
              disabled={busy}
              className="flex items-center gap-2 px-3 py-2 rounded-md text-sm text-left border border-[var(--color-line)] hover:border-[var(--color-text-muted)] transition-colors disabled:opacity-50"
            >
              <span className="flex-1 truncate">{w.name}</span>
              <span className="text-xs text-[var(--color-text-muted)] capitalize">
                {w.role}
              </span>
            </button>
          ))}
        </div>
      )}

      <div className="flex flex-col gap-2">
        <p className="text-xs text-[var(--color-text-muted)]">
          {status.workspaces.length > 0 || joining
            ? "Or create a new workspace:"
            : "Name your workspace:"}
        </p>
        <div className="flex gap-2">
          <input
            type="text"
            value={newWs}
            onChange={(e) => setNewWs(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && newWs.trim() && create()}
            placeholder="Acme Inc"
            className={inputCls}
          />
          <button
            type="button"
            onClick={create}
            disabled={busy || !newWs.trim()}
            className="nd-btn nd-btn-primary shrink-0"
          >
            {busy ? "Creating…" : "Create"}
          </button>
        </div>
      </div>

      {error && <p className="text-xs text-[var(--color-danger)] break-all">{error}</p>}
    </div>
  );
}

// ── Stage: trial (Stripe Checkout + poll-until-live) ─────────────────────────
// "Start free trial" → billingCheckout returns a hosted Stripe URL → open it in
// the browser → poll cloud_status every ~3s until this workspace's plan_status
// flips to trialing/active. Poll is torn down on unmount / when the plan goes
// live, and has a soft ~5 min ceiling after which it shows a CALM "finish in
// Settings later" note — never an error. Continue (in the parent) still works
// throughout.

function TrialStage({ ws, ctx }: { ws: CloudWorkspace; ctx: StepContext }) {
  const refresh = useCloudStore((s) => s.refresh);
  const [checkout, setCheckout] = useState<CheckoutState>("idle");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Poll cloud_status while "waiting". Cancellation-flag + interval cleanup so
  // this can't leak past unmount or an option change (which unmounts us).
  useEffect(() => {
    if (checkout !== "waiting") return;
    let cancelled = false;
    const startedAt = Date.now();

    const tick = async () => {
      try {
        await refresh();
      } catch {
        // Transient — keep polling; the ceiling handles a persistent outage.
      }
      if (cancelled) return;
      const live = planIsLive(
        useCloudStore
          .getState()
          .status.workspaces.find((w) => w.id === ws.id) ??
          useCloudStore.getState().status.current_workspace,
      );
      if (live) {
        // Stage derivation in TeamFunnel will now render SuccessStage.
        setCheckout("idle");
        return;
      }
      if (Date.now() - startedAt >= CHECKOUT_TIMEOUT_MS) {
        setCheckout("timeout");
      }
    };

    const timer = window.setInterval(() => void tick(), CHECKOUT_POLL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [checkout, refresh, ws.id]);

  async function startTrial() {
    setBusy(true);
    setError(null);
    try {
      const url = await cloudApi.billingCheckout(ws.id);
      await openExternal(url);
      setCheckout("waiting");
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex flex-col gap-3">
      <div className="flex items-center gap-2">
        <Check size={14} strokeWidth={2.5} className="text-[var(--color-success)]" />
        <span className="text-xs text-[var(--color-text-muted)]">
          Workspace “{ws.name}” is ready.
        </span>
      </div>

      <p className="text-xs leading-relaxed text-[var(--color-text-muted)]">
        Start a 14-day free trial to unlock syncing and editing for everyone in
        it. $5 per seat/mo · cancel anytime.
      </p>

      {checkout === "idle" && (
        <button
          type="button"
          onClick={startTrial}
          disabled={busy}
          className="nd-btn nd-btn-primary self-start"
        >
          <ExternalLink size={14} strokeWidth={2} />
          {busy ? "Opening…" : "Start free trial"}
        </button>
      )}

      {checkout === "waiting" && (
        <div className="flex flex-col gap-2">
          <p className="text-xs text-[var(--color-text-muted)] flex items-center gap-2">
            <span className="inline-block w-2 h-2 rounded-full bg-[var(--color-warning)] animate-pulse" />
            Waiting for checkout… complete it in your browser.
          </p>
          <button
            type="button"
            onClick={startTrial}
            disabled={busy}
            className="self-start text-xs text-[var(--color-text-muted)] hover:text-[var(--color-text)] underline transition-colors"
          >
            Reopen checkout
          </button>
        </div>
      )}

      {checkout === "timeout" && (
        <div
          className="rounded-[var(--radius)] px-3 py-3"
          style={{ background: "var(--color-accent-soft)" }}
        >
          <p className="text-xs leading-relaxed text-[var(--color-text-muted)]">
            No rush — you can finish setting up billing anytime in Settings →
            Organization. Your workspace is saved.
          </p>
          <button
            type="button"
            onClick={ctx.goNext}
            className="mt-2 text-xs text-[var(--color-text)] hover:text-[var(--color-accent-text)] underline transition-colors"
          >
            Continue for now
          </button>
        </div>
      )}

      {error && <p className="text-xs text-[var(--color-danger)] break-all">{error}</p>}
    </div>
  );
}

// ── Stage: success ───────────────────────────────────────────────────────────

function SuccessStage({ ws }: { ws: CloudWorkspace }) {
  const label = ws.plan_status === "trialing" ? "Free trial started" : "Subscription active";
  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-center gap-2">
        <span className="grid place-items-center w-6 h-6 rounded-full bg-[var(--color-success)] text-white shrink-0">
          <Check size={14} strokeWidth={3} />
        </span>
        <span className="text-sm font-semibold text-[var(--color-text)]">
          {ws.name}
        </span>
      </div>
      <p className="text-xs text-[var(--color-success)]">{label} — you're all set.</p>
      <p className="text-xs leading-relaxed text-[var(--color-text-muted)]">
        Manage members and billing anytime in Settings → Account.
      </p>
    </div>
  );
}
