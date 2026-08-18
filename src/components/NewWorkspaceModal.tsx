import { useEffect, useState } from "react";
import {
  ArrowRight,
  Check,
  Cloud,
  ExternalLink,
  Mail,
  MessageCircle,
  RefreshCw,
  Users,
} from "lucide-react";
import {
  HUMLA_CLOUD_URL,
  cloudApi,
  formatSeatPrice,
  useCloudStore,
  type CloudWorkspace,
} from "../lib/cloud";
import { billingCta, planIsLive, useCheckout } from "../lib/billing";
import { useNotesStore } from "../lib/store";
import { Modal } from "../pages/settings/components/Modal";

// Creating a workspace, end to end, in one sheet.
//
// It replaces a bare text input that lived inside the workspace dropdown and
// committed on blur. The problem with that input was never its looks: a
// workspace is born `plan_status: "none"`, which is READ-ONLY, so naming one
// dropped the user straight into a note wearing a "this workspace needs an
// active subscription" banner — a dead end reached by taking exactly the action
// the app offered. The fix is for the flow that creates a workspace to be the
// same flow that makes it work, which means this sheet has to carry sign-in,
// pricing, checkout and the first invite.
//
// The stage is DERIVED from cloud status, never stored as a step index. Every
// stage's exit condition is a fact on the server (configured → logged in →
// workspace exists → plan live), so a user who completes a step out of band —
// signs in elsewhere, pays in a browser tab, gets a webhook late — lands in the
// right place instead of on a wizard step that no longer applies.

type Stage = "connect" | "auth" | "name" | "trial" | "invite";

type Props = {
  open: boolean;
  onClose: () => void;
  /**
   * Work on an EXISTING workspace instead of creating one — the read-only
   * banner's "Start free trial" passes the stranded workspace here, so the
   * banner resolves itself rather than pointing at Settings. Naming is skipped;
   * the sheet opens on whatever that workspace still needs.
   */
  workspaceId?: string | null;
};

const inputCls =
  "w-full text-sm px-3 py-2 rounded-md border border-[var(--color-line-visible)] bg-[var(--color-surface)] focus:border-[var(--color-text-muted)] outline-none transition-colors";

// ── chrome ───────────────────────────────────────────────────────────────────

/**
 * The workspace this stage is acting on. `justCreated` is the sheet's own doing,
 * so only then may this say "created" — entered from a note's read-only banner
 * the workspace has been around for a while, and claiming otherwise is a lie
 * about what just happened.
 */
function WorkspaceRow({
  ws,
  justCreated,
  note,
}: {
  ws: CloudWorkspace;
  justCreated: boolean;
  note?: string;
}) {
  return (
    <div className="flex items-center gap-2">
      {justCreated ? (
        <span className="grid place-items-center w-5 h-5 rounded-full shrink-0 bg-[var(--color-success)] text-white">
          <Check size={12} strokeWidth={3} />
        </span>
      ) : (
        <span className="grid place-items-center w-5 h-5 rounded shrink-0 bg-[var(--color-pill-hover)] text-[var(--color-text-muted)]">
          <Users size={12} strokeWidth={1.7} />
        </span>
      )}
      <span className="text-sm font-medium truncate">{ws.name}</span>
      {note && <span className="text-xs text-[var(--color-text-muted)]">{note}</span>}
    </div>
  );
}

function Head({ title, blurb }: { title: string; blurb?: string }) {
  return (
    <div className="flex flex-col gap-1.5">
      <h2 className="text-lg font-semibold tracking-tight">{title}</h2>
      {blurb && (
        <p className="text-sm leading-relaxed text-[var(--color-text-muted)]">{blurb}</p>
      )}
    </div>
  );
}

function Footer({ children }: { children: React.ReactNode }) {
  return (
    <div className="flex items-center gap-3 px-6 py-4 border-t border-[var(--color-line)]">
      {children}
    </div>
  );
}

function Body({ children }: { children: React.ReactNode }) {
  return <div className="px-6 py-5 flex flex-col gap-4">{children}</div>;
}

function Err({ children }: { children: string | null }) {
  if (!children) return null;
  return <p className="text-xs text-[var(--color-danger)] break-words">{children}</p>;
}

/**
 * What a workspace is for, and what it costs. The three lines are the three
 * things that stop working in Personal mode, in the order people ask about
 * them — not a feature list.
 */
function PricingPanel({
  seatPriceCents,
  seatCurrency,
  trial,
}: {
  seatPriceCents?: number | null;
  seatCurrency?: string | null;
  trial: boolean;
}) {
  const price = typeof seatPriceCents === "number" ? seatPriceCents : 500;
  return (
    <div
      className="rounded-lg p-4 flex flex-col gap-3"
      style={{ background: "var(--color-accent-soft)" }}
    >
      <div className="flex items-center gap-1.5 text-sm font-medium">
        <Cloud size={15} strokeWidth={1.7} /> Humla Cloud
      </div>
      <ul className="flex flex-col gap-2">
        {[
          { Icon: Users, text: "Notes, summaries and transcripts shared with your team" },
          { Icon: RefreshCw, text: "Synced across your devices and backed up off your Mac" },
          { Icon: MessageCircle, text: "Chat across everyone's notes, not just your own" },
        ].map(({ Icon, text }) => (
          <li key={text} className="flex items-start gap-2 text-sm">
            <Icon
              size={14}
              strokeWidth={1.7}
              className="shrink-0 mt-[3px] text-[var(--color-accent-text)]"
            />
            <span className="text-[var(--color-text-muted)] leading-snug">{text}</span>
          </li>
        ))}
      </ul>
      <div className="flex items-baseline gap-1.5 pt-0.5">
        <span className="text-2xl font-semibold tracking-tight">
          {formatSeatPrice(price, seatCurrency)}
        </span>
        <span className="text-xs text-[var(--color-text-muted)]">per seat, per month</span>
      </div>
      <p className="text-xs text-[var(--color-text-muted)]">
        {trial ? "14-day free trial · cancel anytime" : "Cancel anytime"}
      </p>
    </div>
  );
}

// ── stage: connect (no server configured) ────────────────────────────────────

function ConnectStage({ onClose }: { onClose: () => void }) {
  const refresh = useCloudStore((s) => s.refresh);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function useHumlaCloud() {
    setBusy(true);
    setError(null);
    try {
      await cloudApi.configure(HUMLA_CLOUD_URL);
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <Body>
        <Head
          title="Create a team workspace"
          blurb="Humla works fully offline and your notes live on your Mac. A workspace is how you share them with other people."
        />
        <PricingPanel trial />
        <Err>{error}</Err>
      </Body>
      <Footer>
        <button
          type="button"
          onClick={useHumlaCloud}
          disabled={busy}
          className="nd-btn nd-btn-primary"
        >
          <Cloud size={14} strokeWidth={2} />
          {busy ? "Connecting…" : "Use Humla Cloud"}
        </button>
        <span className="flex-1" />
        <button
          type="button"
          onClick={onClose}
          className="text-xs text-[var(--color-text-muted)] hover:text-[var(--color-text)] transition-colors"
        >
          Not now
        </button>
      </Footer>
      <p className="px-6 pb-4 -mt-1 text-[11px] text-[var(--color-text-muted)]">
        Running your own server? Connect it under Settings → Account.
      </p>
    </>
  );
}

// ── stage: auth (configured, signed out) ─────────────────────────────────────

function AuthStage() {
  const status = useCloudStore((s) => s.status);
  const refresh = useCloudStore((s) => s.refresh);
  const [mode, setMode] = useState<"signin" | "signup">("signup");
  const [name, setName] = useState("");
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const isSignup = mode === "signup";
  const canSubmit = !!email.trim() && !!password && (!isSignup || !!name.trim());

  async function submit() {
    if (!canSubmit) return;
    setBusy(true);
    setError(null);
    try {
      if (isSignup) await cloudApi.signup(email.trim(), password, name.trim());
      else await cloudApi.login(email.trim(), password);
      setPassword("");
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <Body>
        <Head
          title={isSignup ? "Create your account" : "Sign in"}
          blurb={
            isSignup
              ? "A workspace belongs to an account, so this comes first. One step, then you name the workspace."
              : "Sign in to the account your workspaces belong to."
          }
        />
        <div className="flex flex-col gap-2.5">
          {isSignup && (
            <input
              className={inputCls}
              type="text"
              autoComplete="name"
              aria-label="Your name"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="Your name"
            />
          )}
          <input
            className={inputCls}
            type="email"
            autoComplete="username"
            aria-label="Email"
            value={email}
            onChange={(e) => setEmail(e.target.value)}
            placeholder="you@example.com"
          />
          <input
            className={inputCls}
            type="password"
            autoComplete={isSignup ? "new-password" : "current-password"}
            aria-label="Password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && submit()}
            placeholder={isSignup ? "At least 8 characters" : "••••••••"}
          />
        </div>
        <Err>{error}</Err>
        {/* Branch, don't assert: a self-hosted server bills nothing, so quoting a
            per-seat price here would be the app inventing a charge. */}
        <p className="text-xs text-[var(--color-text-muted)]">
          {status.billing_enabled
            ? `${formatSeatPrice(status.seat_price_cents ?? 500, status.seat_currency)} per seat/mo after a 14-day free trial · cancel anytime`
            : "Your own server — Humla bills nothing for it."}
        </p>
      </Body>
      <Footer>
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
          <ArrowRight size={14} strokeWidth={2} />
        </button>
        <span className="flex-1" />
        <button
          type="button"
          onClick={() => {
            setMode(isSignup ? "signin" : "signup");
            setError(null);
          }}
          className="text-xs text-[var(--color-interactive)] hover:underline"
        >
          {isSignup ? "I already have an account" : "Create an account instead"}
        </button>
      </Footer>
    </>
  );
}

// ── stage: name ──────────────────────────────────────────────────────────────

function NameStage({
  onCreated,
}: {
  onCreated: (ws: CloudWorkspace) => void;
}) {
  const status = useCloudStore((s) => s.status);
  const refreshCloud = useCloudStore((s) => s.refresh);
  const refreshNotes = useNotesStore((s) => s.refresh);
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function create() {
    const trimmed = name.trim();
    // Guarded rather than blur-committed: the old input created a workspace as
    // a side effect of clicking away from it.
    if (!trimmed || busy) return;
    setBusy(true);
    setError(null);
    try {
      const ws = await cloudApi.createWorkspace(trimmed);
      // Creating selects it backend-side, so both stores are now stale.
      await refreshCloud();
      await refreshNotes();
      onCreated(ws);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <Body>
        <Head
          title="Name your workspace"
          blurb="Usually your company or team name. You can rename it later."
        />
        <input
          autoFocus
          className={inputCls}
          aria-label="Workspace name"
          value={name}
          disabled={busy}
          onChange={(e) => setName(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && create()}
          placeholder="Acme Inc"
        />
        {status.billing_enabled ? (
          <PricingPanel
            seatPriceCents={status.seat_price_cents}
            seatCurrency={status.seat_currency}
            trial
          />
        ) : (
          <p className="text-xs leading-relaxed text-[var(--color-text-muted)]">
            This is your own server, so there's no billing — the workspace is
            usable the moment it exists.
          </p>
        )}
        <Err>{error}</Err>
      </Body>
      <Footer>
        <button
          type="button"
          onClick={create}
          disabled={busy || !name.trim()}
          className="nd-btn nd-btn-primary"
        >
          {busy ? "Creating…" : "Create workspace"}
          <ArrowRight size={14} strokeWidth={2} />
        </button>
      </Footer>
    </>
  );
}

// ── stage: trial ─────────────────────────────────────────────────────────────

function TrialStage({
  ws,
  justCreated,
  onClose,
  onStartOver,
}: {
  ws: CloudWorkspace;
  justCreated: boolean;
  onClose: () => void;
  /** Offered only on a RESUMED sheet — abandoning the first workspace's checkout
   *  shouldn't mean the sheet can only ever return to it. */
  onStartOver?: () => void;
}) {
  const status = useCloudStore((s) => s.status);
  const { state, busy, error, start } = useCheckout(ws.id);
  const { kind, trial, label } = billingCta(ws);
  const pastDue = kind === "portal";

  return (
    <>
      <Body>
        <WorkspaceRow ws={ws} justCreated={justCreated} note={justCreated ? "created" : undefined} />
        <Head
          title={pastDue ? "Fix the payment" : trial ? "Start your free trial" : "Subscribe"}
          blurb={
            pastDue
              ? "A payment for this workspace didn't go through, so it's read-only for everyone in it until the payment method is updated."
              : "A workspace is read-only until its plan is live — nothing syncs and nobody can edit. This unlocks it for everyone in it."
          }
        />
        {!pastDue && (
          <PricingPanel
            seatPriceCents={status.seat_price_cents}
            seatCurrency={status.seat_currency}
            trial={trial}
          />
        )}

        {state === "waiting" && (
          <p className="text-xs text-[var(--color-text-muted)] flex items-center gap-2">
            <span className="inline-block w-2 h-2 rounded-full bg-[var(--color-warning)] animate-pulse" />
            Waiting for {pastDue ? "payment" : "checkout"} — finish it in your browser.
          </p>
        )}
        {state === "timeout" && (
          <div
            className="rounded-[var(--radius)] px-3 py-3"
            style={{ background: "var(--color-accent-soft)" }}
          >
            <p className="text-xs leading-relaxed text-[var(--color-text-muted)]">
              No rush — <strong className="font-medium text-[var(--color-text)]">{ws.name}</strong>{" "}
              is saved. You can finish billing any time in Settings → Account.
            </p>
          </div>
        )}
        <Err>{error}</Err>
      </Body>
      <Footer>
        <button
          type="button"
          onClick={() => void start(kind)}
          disabled={busy}
          className="nd-btn nd-btn-primary"
        >
          <ExternalLink size={14} strokeWidth={2} />
          {busy
            ? "Opening…"
            : state !== "idle"
              ? pastDue
                ? "Reopen billing"
                : "Reopen checkout"
              : label}
        </button>
        <span className="flex-1" />
        {onStartOver && (
          <button
            type="button"
            onClick={onStartOver}
            className="text-xs text-[var(--color-text-muted)] hover:text-[var(--color-text)] transition-colors"
          >
            Create a different one
          </button>
        )}
        <button
          type="button"
          onClick={onClose}
          className="text-xs text-[var(--color-text-muted)] hover:text-[var(--color-text)] transition-colors"
        >
          Later
        </button>
      </Footer>
      <p className="px-6 pb-4 -mt-1 text-[11px] text-[var(--color-text-muted)]">
        Billing opens Stripe in your browser. This updates itself when you're done.
      </p>
    </>
  );
}

// ── stage: invite ────────────────────────────────────────────────────────────

type Invited = { email: string; status: "added" | "invited" };

function InviteStage({
  ws,
  justCreated,
  onClose,
}: {
  ws: CloudWorkspace;
  justCreated: boolean;
  onClose: () => void;
}) {
  const billed = useCloudStore((s) => s.status.billing_enabled);
  const [email, setEmail] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [sent, setSent] = useState<Invited[]>([]);

  async function invite() {
    const addr = email.trim();
    if (!addr || busy) return;
    setBusy(true);
    setError(null);
    try {
      const result = await cloudApi.inviteMember(ws.id, addr);
      setSent((prev) => [
        ...prev,
        { email: addr, status: result === "invited" ? "invited" : "added" },
      ]);
      setEmail("");
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  const live = planIsLive(ws);

  return (
    <>
      <Body>
        <WorkspaceRow
          ws={ws}
          justCreated={justCreated}
          note={live ? (ws.plan_status === "trialing" ? "trial started" : "active") : "ready"}
        />
        <Head
          title="Invite your team"
          blurb={
            // The seat sentence is only true where something bills; `stageFor`
            // routes a self-hosted workspace straight here, so it reaches this
            // copy without ever passing a price.
            billed
              ? "Anyone you add can read and write the workspace's notes. Each member is a seat on the next invoice, prorated."
              : "Anyone you add can read and write the workspace's notes."
          }
        />
        <div className="flex gap-2">
          <input
            autoFocus
            className={inputCls}
            type="email"
            aria-label="Teammate's email"
            value={email}
            disabled={busy}
            onChange={(e) => setEmail(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && invite()}
            placeholder="teammate@example.com"
          />
          <button
            type="button"
            onClick={invite}
            disabled={busy || !email.trim()}
            className="nd-btn shrink-0"
          >
            <Mail size={14} strokeWidth={2} />
            {busy ? "Sending…" : "Invite"}
          </button>
        </div>
        {sent.length > 0 && (
          <ul className="flex flex-col gap-1.5">
            {sent.map((s) => (
              <li key={s.email} className="flex items-center gap-2 text-xs">
                <Check
                  size={13}
                  strokeWidth={2.5}
                  className="shrink-0 text-[var(--color-success)]"
                />
                <span className="truncate">{s.email}</span>
                <span className="text-[var(--color-text-muted)] shrink-0">
                  {/* "invited" has no account yet, so nothing happens until they
                      sign up AND verify — saying "added" there would be a lie. */}
                  {s.status === "invited" ? "invited by email" : "added"}
                </span>
              </li>
            ))}
          </ul>
        )}
        <Err>{error}</Err>
      </Body>
      <Footer>
        <button type="button" onClick={onClose} className="nd-btn nd-btn-primary">
          Done
        </button>
        <span className="flex-1" />
        <span className="text-[11px] text-[var(--color-text-muted)]">
          You can invite more people any time in Settings → Account.
        </span>
      </Footer>
    </>
  );
}

// ── the sheet ────────────────────────────────────────────────────────────────

export function stageFor(
  status: {
    configured: boolean;
    logged_in: boolean;
    billing_enabled: boolean;
  },
  ws: CloudWorkspace | null,
): Stage {
  if (!status.configured) return "connect";
  if (!status.logged_in) return "auth";
  if (!ws) return "name";
  // Self-hosted servers don't bill, so a workspace is usable the moment it
  // exists — there is no plan to wait on.
  if (status.billing_enabled && !planIsLive(ws)) return "trial";
  return "invite";
}

export function NewWorkspaceModal({ open, onClose, workspaceId = null }: Props) {
  const status = useCloudStore((s) => s.status);
  const [created, setCreated] = useState<CloudWorkspace | null>(null);

  // Reopening RESUMES a workspace this sheet created but never got live, and
  // starts fresh otherwise. Without this, dismissing the sheet mid-checkout and
  // clicking "Create team workspace" again built a SECOND workspace while the
  // first sat unsubscribed and read-only — the one hole left in "you land in a
  // working workspace, never in the read-only state". Resuming is scoped to this
  // sheet's own unfinished work: an existing workspace that happens to be
  // unpaid must not hijack a deliberate request for a new one, which is why the
  // condition is `created`, not "any workspace whose plan isn't live".
  useEffect(() => {
    if (!open) return;
    setCreated((prev) => {
      if (!prev) return null;
      const live = planIsLive(
        useCloudStore.getState().status.workspaces.find((w) => w.id === prev.id) ?? prev,
      );
      return live ? null : prev;
    });
  }, [open]);

  const targetId = workspaceId ?? created?.id ?? null;
  // Server state wins over the object `createWorkspace` returned — that one is
  // always `plan_status: "none"`, and by the time checkout lands it is stale.
  const ws =
    (targetId
      ? (status.workspaces.find((w) => w.id === targetId) ??
        (status.current_workspace?.id === targetId ? status.current_workspace : null) ??
        (created?.id === targetId ? created : null))
      : null) ?? null;

  const stage = stageFor(status, ws);
  // Only true when THIS sheet made the workspace, not merely opened one.
  const justCreated = !!created && created.id === ws?.id;

  return (
    <Modal
      open={open}
      onClose={onClose}
      size="sm"
      padded={false}
      showHeader={false}
      title="Create a team workspace"
    >
      {stage === "connect" && <ConnectStage onClose={onClose} />}
      {stage === "auth" && <AuthStage />}
      {stage === "name" && <NameStage onCreated={setCreated} />}
      {stage === "trial" && ws && (
        <TrialStage
          ws={ws}
          justCreated={justCreated}
          onClose={onClose}
          // Only when this sheet is resuming its own creation — with an id passed
          // in (the note banner) there is nothing to start over from.
          onStartOver={!workspaceId && created ? () => setCreated(null) : undefined}
        />
      )}
      {stage === "invite" && ws && (
        <InviteStage ws={ws} justCreated={justCreated} onClose={onClose} />
      )}
    </Modal>
  );
}
