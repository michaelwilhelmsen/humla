import { useCallback, useEffect, useState } from "react";
import { LogOut, ShieldCheck, Trash2, UserPlus } from "lucide-react";
import { open as openExternal } from "@tauri-apps/plugin-shell";
import {
  cloudApi,
  cloudError,
  formatSeatPrice,
  roleColorVar,
  roleLabel,
  useCloudStore,
  type CloudMember,
  type CloudRole,
  type CloudWorkspace,
} from "../../../lib/cloud";
import { useNotesStore } from "../../../lib/store";
import { Row, Section } from "../components/Section";
import { Btn } from "../components/Btn";
import { Select } from "../../../components/ui/Select";
import { ValuePill } from "../components/ValuePill";
import { Toggle } from "../components/Toggle";
import { ChatKeyPanel } from "./ChatKeyPanel";
import { NewWorkspaceModal } from "../../../components/NewWorkspaceModal";

const inputCls =
  "flex-1 min-w-0 text-sm px-3 py-2 rounded-md border border-[var(--color-line-visible)] bg-[var(--color-surface)] focus:border-[var(--color-text-muted)] transition-colors";

function RolePill({ role }: { role: CloudRole }) {
  return (
    <span
      className="shrink-0 px-1.5 py-0.5 text-[11px] rounded border"
      style={{ color: roleColorVar(role), borderColor: "var(--color-line)" }}
    >
      {roleLabel(role)}
    </span>
  );
}

/** Who has two-factor authentication, by member id; null while loading. */
type TwoFactorRoster = Record<string, boolean> | "failed" | null;

function TwoFactorBadge({ on }: { on: boolean }) {
  return on ? (
    <span
      className="shrink-0 inline-flex items-center gap-1 text-xs"
      style={{ color: "var(--color-success)" }}
      title="Signs in with two-factor authentication"
    >
      <ShieldCheck size={12} strokeWidth={1.8} />
      2FA
    </span>
  ) : (
    <span
      className="shrink-0 text-xs text-[var(--color-text-muted)]"
      title="Hasn't set up two-factor authentication"
    >
      No 2FA
    </span>
  );
}

/** What requiring two-factor authentication would do, or does. */
function requireDescription(
  required: boolean,
  youHaveIt: boolean,
  members: CloudMember[],
  hasTwoFactor: TwoFactorRoster,
): string {
  if (required) {
    return "Members sign in with an authenticator code to open this workspace. Anyone without it can't until they set it up.";
  }
  if (!youHaveIt) {
    return "Turn on two-factor authentication for your own account first, under Two-factor authentication above.";
  }
  const base = "Only members with two-factor authentication can open this workspace.";
  if (hasTwoFactor === "failed") return `${base} Couldn't check which members have it.`;
  const without = hasTwoFactor ? members.filter((m) => hasTwoFactor[m.id] === false).length : 0;
  if (without === 0) return base;
  return `${base} ${without} ${without === 1 ? "member doesn't" : "members don't"} have it yet and would lose access until they set it up.`;
}

function planMeta(status: CloudWorkspace["plan_status"]): { label: string; color: string } {
  switch (status) {
    case "trialing":
      return { label: "Free trial", color: "var(--color-success)" };
    case "active":
      return { label: "Active", color: "var(--color-success)" };
    case "past_due":
      return { label: "Payment past due", color: "var(--color-warning)" };
    case "canceled":
      return { label: "Canceled", color: "var(--color-danger)" };
    default:
      return { label: "Not subscribed", color: "var(--color-text-muted)" };
  }
}

// Per-workspace billing (shown only when the server enforces billing). The owner
// starts a 14-day trial / manages the subscription via Stripe (opened in the
// browser); everyone else sees the status. The server is the source of truth, so
// this is a thin control surface — status comes from the workspace's plan_status.
function BillingPanel({
  ws,
  seatPriceCents,
  seatCurrency,
  onChanged,
}: {
  ws: CloudWorkspace;
  seatPriceCents?: number | null;
  seatCurrency?: string | null;
  onChanged: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const isOwner = ws.role === "owner";
  const active = ws.plan_status === "active" || ws.plan_status === "trialing";
  // A past-due subscription still exists on Stripe — we must NOT offer checkout
  // (that creates a second subscription → double billing). The owner fixes the
  // failed payment through the same Customer Portal as "Manage billing".
  const pastDue = ws.plan_status === "past_due";
  // The server grants the 14-day trial only to workspaces that never had a
  // subscription (humla-cloud billing.pb.js) — mirror that so the CTA never
  // promises a trial that checkout won't include.
  const firstSub = ws.plan_status === "none";
  const meta = planMeta(ws.plan_status);
  // Seat/price rows show only for a workspace with a real subscription (the
  // seat count is meaningless otherwise). "past_due" is included so an owner
  // fixing payment still sees what they're being billed for.
  const seats = typeof ws.seats === "number" && ws.seats > 0 ? ws.seats : null;
  const seatsBilled =
    ws.plan_status === "active" || ws.plan_status === "trialing" || ws.plan_status === "past_due";

  async function go(kind: "checkout" | "portal") {
    setBusy(true);
    setErr(null);
    try {
      const url =
        kind === "checkout"
          ? await cloudApi.billingCheckout(ws.id, "settings_organization")
          : await cloudApi.billingPortal(ws.id);
      await openExternal(url);
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex flex-col gap-3 py-3.5">
      <div className="flex items-center justify-between gap-6">
        <div className="text-sm">Plan</div>
        <ValuePill color={meta.color}>{meta.label}</ValuePill>
      </div>
      {seats != null && seatsBilled && (
        <>
          <div className="flex items-center justify-between gap-6">
            <div className="text-sm">Seats</div>
            <ValuePill>{seats}</ValuePill>
          </div>
          {typeof seatPriceCents === "number" && (
            <>
              <div className="flex items-center justify-between gap-6">
                <div className="text-sm">Per seat</div>
                <ValuePill>{`${formatSeatPrice(seatPriceCents, seatCurrency)}/mo`}</ValuePill>
              </div>
              <div className="flex items-center justify-between gap-6">
                <div className="text-sm">Total</div>
                <ValuePill>{`${formatSeatPrice(seatPriceCents * seats, seatCurrency)}/mo`}</ValuePill>
              </div>
            </>
          )}
          <p className="text-xs text-[var(--color-text-muted)] leading-relaxed">
            Each member is a seat. Changes are prorated.
          </p>
        </>
      )}
      {pastDue ? (
        <p className="text-xs text-[var(--color-text-muted)] leading-relaxed">
          A payment for this workspace didn't go through.{" "}
          {isOwner
            ? "Update your payment method to keep syncing and editing active for everyone in it."
            : "Ask the workspace owner to update the payment method."}
        </p>
      ) : (
        !active && (
          <p className="text-xs text-[var(--color-text-muted)] leading-relaxed">
            This workspace is read-only until it has an active subscription.{" "}
            {isOwner
              ? firstSub
                ? "Start a 14-day free trial to unlock syncing and editing for everyone in it."
                : "Subscribe to unlock syncing and editing for everyone in it."
              : "Ask the workspace owner to subscribe."}
          </p>
        )
      )}
      {isOwner && (
        <div className="flex items-center gap-2">
          {active ? (
            <Btn onClick={() => go("portal")} disabled={busy}>
              {busy ? "Opening…" : "Manage billing"}
            </Btn>
          ) : pastDue ? (
            <Btn onClick={() => go("portal")} disabled={busy}>
              {busy ? "Opening…" : "Fix payment"}
            </Btn>
          ) : (
            <Btn onClick={() => go("checkout")} disabled={busy}>
              {busy ? "Opening…" : firstSub ? "Start 14-day free trial" : "Subscribe"}
            </Btn>
          )}
          <Btn onClick={onChanged} disabled={busy}>
            Refresh
          </Btn>
        </div>
      )}
      {err && <p className="text-xs text-[var(--color-danger)] break-all">{err}</p>}
      <p className="text-[11px] text-[var(--color-text-muted)]">
        Opens Stripe in your browser.
      </p>
    </div>
  );
}

export function OrganizationTab() {
  const status = useCloudStore((s) => s.status);
  const refreshCloud = useCloudStore((s) => s.refresh);
  const refreshNotes = useNotesStore((s) => s.refresh);

  const ws = status.current_workspace;
  const canManage = ws?.role === "owner" || ws?.role === "admin";
  const twoFactor = status.two_factor;
  const manageTwoFactor = canManage && !!twoFactor?.available;

  const [members, setMembers] = useState<CloudMember[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [addEmail, setAddEmail] = useState("");
  const [busy, setBusy] = useState(false);
  const [creatingOpen, setCreatingOpen] = useState(false);
  const [nameDraft, setNameDraft] = useState("");
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [confirmLeave, setConfirmLeave] = useState(false);
  const [transferTo, setTransferTo] = useState("");
  const [confirmTransfer, setConfirmTransfer] = useState(false);
  // Who has two-factor authentication, by member id: what an owner or admin
  // needs to see before requiring it.
  const [hasTwoFactor, setHasTwoFactor] = useState<TwoFactorRoster>(null);

  // Reseed the rename field and reset the danger confirms whenever the active
  // workspace (or its name, after a rename) changes.
  useEffect(() => {
    setNameDraft(ws?.name ?? "");
    setConfirmDelete(false);
    setConfirmLeave(false);
    setTransferTo("");
    setConfirmTransfer(false);
  }, [ws?.id, ws?.name]);

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

  useEffect(() => {
    setHasTwoFactor(null);
    if (!ws?.id || !manageTwoFactor) return;
    let cancelled = false;
    cloudApi
      .twoFactorMembers(ws.id)
      .then((map) => !cancelled && setHasTwoFactor(map))
      .catch(() => !cancelled && setHasTwoFactor("failed"));
    return () => {
      cancelled = true;
    };
  }, [ws?.id, manageTwoFactor]);

  // Signed out: nothing to manage. The sign-in UI renders directly above in
  // the same Account section (post-merge), so no pointer stub is needed.
  if (!status.logged_in) {
    return null;
  }

  // No active workspace -----------------------------------------------------
  if (!ws) {
    const locked = status.locked_current_workspace;
    return (
      <Section title="Workspace">
        <p className="text-sm text-[var(--color-text-muted)] py-3.5">
          {locked
            ? `${locked.name} requires two-factor authentication before you can open it. Turn it on under Two-factor authentication, above.`
            : "You're in Personal (local-only). Create a workspace, or pick one in the sidebar."}
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
        {/* One create flow, not two: this used to be a second bare name input
            (the switcher had the other), and both left the new workspace
            read-only with no hint of what to do next. */}
        <Row label="Team workspace">
          <Btn onClick={() => setCreatingOpen(true)}>Create team workspace…</Btn>
        </Row>
        <NewWorkspaceModal open={creatingOpen} onClose={() => setCreatingOpen(false)} />
      </Section>
    );
  }

  // Active workspace → membership management --------------------------------
  async function addMember() {
    const email = addEmail.trim();
    if (!email || !ws) return;
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const status = await cloudApi.inviteMember(ws.id, email);
      setAddEmail("");
      setNotice(
        status === "invited"
          ? `Invitation emailed to ${email} — they'll join automatically when they sign up and verify their email.`
          : `Added ${email} to the workspace — they've been notified by email.`,
      );
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

  async function setRequireTwoFactor(require: boolean) {
    if (!ws) return;
    setBusy(true);
    setError(null);
    try {
      await cloudApi.setWorkspaceRequire2fa(ws.id, require);
      await refreshCloud();
    } catch (e) {
      setError(cloudError(e).message);
    } finally {
      setBusy(false);
    }
  }

  async function rename() {
    if (!ws) return;
    const name = nameDraft.trim();
    if (!name || name === ws.name) return;
    setBusy(true);
    setError(null);
    try {
      await cloudApi.renameWorkspace(ws.id, name);
      await refreshCloud();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function transferOwnership() {
    if (!ws || !transferTo) return;
    setBusy(true);
    setError(null);
    try {
      await cloudApi.transferWorkspace(ws.id, transferTo);
      setTransferTo("");
      setConfirmTransfer(false);
      // Roles changed (owner → admin for us), so refresh status + roster.
      await refreshCloud();
      await loadMembers(ws.id);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function deleteWorkspace() {
    if (!ws) return;
    setBusy(true);
    setError(null);
    try {
      await cloudApi.deleteWorkspace(ws.id);
      setConfirmDelete(false);
      await refreshCloud();
      await refreshNotes();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function leaveWorkspace() {
    if (!ws) return;
    setBusy(true);
    setError(null);
    try {
      await cloudApi.leaveWorkspace(ws.id);
      setConfirmLeave(false);
      await refreshCloud();
      await refreshNotes();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <Section title="Workspace">
        <Row label="Name">
          {canManage ? (
            <div className="flex gap-2">
              <input
                className={inputCls}
                value={nameDraft}
                onChange={(e) => setNameDraft(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && rename()}
                placeholder="Workspace name"
              />
              <Btn
                onClick={rename}
                disabled={busy || !nameDraft.trim() || nameDraft.trim() === ws.name}
              >
                Save
              </Btn>
            </div>
          ) : (
            <div className="text-sm">{ws.name}</div>
          )}
        </Row>
        <Row
          label="Your role"
          description={
            ws.role === "owner"
              ? "Full control, including billing and deletion."
              : ws.role === "admin"
              ? "Can manage members and settings."
              : "Can create and edit notes."
          }
          control={<RolePill role={ws.role} />}
        />
        {manageTwoFactor && (
          <Row
            label="Require two-factor authentication"
            description={requireDescription(ws.require_2fa ?? false, twoFactor.enabled, members, hasTwoFactor)}
            control={
              <Toggle
                label="Require two-factor authentication"
                checked={ws.require_2fa ?? false}
                disabled={busy || (!twoFactor.enabled && !ws.require_2fa)}
                onChange={setRequireTwoFactor}
              />
            }
          />
        )}
      </Section>

      {status.billing_enabled && ws && (
        <Section title="Billing">
          <BillingPanel
            ws={ws}
            seatPriceCents={status.seat_price_cents}
            seatCurrency={status.seat_currency}
            onChanged={refreshCloud}
          />
        </Section>
      )}

      <Section title="Workspace chat">
        <ChatKeyPanel ws={ws} />
      </Section>

      <Section title={`Members${members.length ? ` · ${members.length}` : ""}`}>
        {loading && <div className="text-sm text-[var(--color-text-muted)] py-3.5">Loading…</div>}
        {error && <div className="text-xs text-[var(--color-danger)] py-3.5">{error}</div>}

        <div className="flex flex-col gap-1 py-3.5">
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
              {hasTwoFactor && hasTwoFactor !== "failed" && m.id in hasTwoFactor && (
                <TwoFactorBadge on={hasTwoFactor[m.id]} />
              )}
              {canManage && m.role !== "owner" ? (
                // Non-growing slot so the name/email block (flex-1 min-w-0)
                // keeps its space; the popover Select trigger sizes to content.
                <div className="shrink-0">
                  <Select
                    value={m.role}
                    onChange={(v) => changeRole(m.id, v as CloudRole)}
                    options={[
                      { value: "viewer", label: "Viewer" },
                      { value: "member", label: "Member" },
                      // Only the owner can grant admin (the server reverts an
                      // admin's change to the admins set). Keep the option visible
                      // when the member already is an admin so it displays.
                      ...(ws?.role === "owner" || m.role === "admin"
                        ? [{ value: "admin", label: "Admin" }]
                        : []),
                    ]}
                  />
                </div>
              ) : (
                <RolePill role={m.role} />
              )}
              {canManage && m.role !== "owner" && (
                <button
                  onClick={() => remove(m.id)}
                  disabled={busy}
                  aria-label="Remove member"
                  title="Remove from workspace"
                  className="shrink-0 p-1.5 rounded text-[var(--color-text-muted)] hover:text-[var(--color-danger)] hover:bg-[var(--color-pill-hover)] transition-colors"
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
                  <UserPlus size={14} strokeWidth={1.5} /> Invite
                </span>
              </Btn>
            </div>
            {notice && <p className="text-xs text-[var(--color-success)] mt-2">{notice}</p>}
            <p className="text-xs text-[var(--color-text-muted)] mt-2">
              Everyone gets an email. New people join when they sign up with this address.
            </p>
          </Row>
        )}
      </Section>

      {ws.role === "owner" && members.some((m) => m.id !== status.user?.id) && (
        <Section title="Ownership">
          <Row label="Transfer ownership">
            <div className="flex flex-col gap-2">
              <div className="flex flex-wrap items-center gap-2">
                <Select
                  value={transferTo}
                  onChange={(v) => {
                    setTransferTo(v);
                    setConfirmTransfer(false);
                  }}
                  options={[
                    { value: "", label: "Choose a member…" },
                    ...members
                      .filter((m) => m.id !== status.user?.id)
                      .map((m) => ({ value: m.id, label: m.name || m.email })),
                  ]}
                />
                {!confirmTransfer ? (
                  <Btn onClick={() => setConfirmTransfer(true)} disabled={busy || !transferTo}>
                    Transfer
                  </Btn>
                ) : (
                  <>
                    <button
                      onClick={transferOwnership}
                      disabled={busy}
                      className="px-3 py-2 rounded-md text-sm border border-[var(--color-danger)] disabled:opacity-50 transition-opacity hover:opacity-90"
                      style={{ background: "var(--color-danger)", color: "#fff" }}
                    >
                      Confirm transfer
                    </button>
                    <Btn onClick={() => setConfirmTransfer(false)} disabled={busy}>
                      Cancel
                    </Btn>
                  </>
                )}
              </div>
              <p className="text-xs text-[var(--color-text-muted)]">
                You stay on as an admin.
              </p>
            </div>
          </Row>
        </Section>
      )}

      {ws.role === "owner" && (
        <Section title="Danger zone">
          <Row label="Delete workspace">
            <div className="flex flex-col gap-2">
              {!confirmDelete ? (
                <button
                  onClick={() => setConfirmDelete(true)}
                  disabled={busy}
                  className="self-start px-3 py-2 rounded-md text-sm border border-[var(--color-danger)] text-[var(--color-danger)] hover:bg-[var(--color-pill-hover)] disabled:opacity-50 transition-colors"
                >
                  <span className="inline-flex items-center gap-1.5">
                    <Trash2 size={14} strokeWidth={1.5} /> Delete workspace
                  </span>
                </button>
              ) : (
                <div className="flex flex-wrap items-center gap-2">
                  <span className="text-xs text-[var(--color-text-muted)]">
                    Delete “{ws.name}” for everyone?
                  </span>
                  <button
                    onClick={deleteWorkspace}
                    disabled={busy}
                    className="px-3 py-2 rounded-md text-sm border border-[var(--color-danger)] disabled:opacity-50 transition-opacity hover:opacity-90"
                    style={{ background: "var(--color-danger)", color: "#fff" }}
                  >
                    Delete permanently
                  </button>
                  <Btn onClick={() => setConfirmDelete(false)} disabled={busy}>
                    Cancel
                  </Btn>
                </div>
              )}
              <p className="text-xs text-[var(--color-text-muted)]">
                Removes the workspace and its notes, folders and prompts from the server for all
                members. Your local copies stay on this device.
              </p>
            </div>
          </Row>
        </Section>
      )}

      {ws.role !== "owner" && (
        <Section title="Danger zone">
          <Row label="Leave workspace">
            <div className="flex flex-col gap-2">
              {!confirmLeave ? (
                <button
                  onClick={() => setConfirmLeave(true)}
                  disabled={busy}
                  className="self-start px-3 py-2 rounded-md text-sm border border-[var(--color-danger)] text-[var(--color-danger)] hover:bg-[var(--color-pill-hover)] disabled:opacity-50 transition-colors"
                >
                  <span className="inline-flex items-center gap-1.5">
                    <LogOut size={14} strokeWidth={1.5} /> Leave workspace
                  </span>
                </button>
              ) : (
                <div className="flex flex-wrap items-center gap-2">
                  <span className="text-xs text-[var(--color-text-muted)]">Leave “{ws.name}”?</span>
                  <button
                    onClick={leaveWorkspace}
                    disabled={busy}
                    className="px-3 py-2 rounded-md text-sm border border-[var(--color-danger)] disabled:opacity-50 transition-opacity hover:opacity-90"
                    style={{ background: "var(--color-danger)", color: "#fff" }}
                  >
                    Leave
                  </button>
                  <Btn onClick={() => setConfirmLeave(false)} disabled={busy}>
                    Cancel
                  </Btn>
                </div>
              )}
              <p className="text-xs text-[var(--color-text-muted)]">
                You lose access to its shared notes. Your local copies stay.
              </p>
            </div>
          </Row>
        </Section>
      )}
    </>
  );
}
