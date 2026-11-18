import { useState } from "react";
import { cloudApi, cloudError, inviteGone, useCloudStore, type OrgInvite } from "../lib/cloud";
import { inviteConsent, inviteFrom } from "../lib/orgInvites";
import { useNotesStore, useRecordingStore } from "../lib/store";

/** One invite to an organisation's department, with Accept and Decline.
 *  Accept first says what joining means. `onOpenAccount` is offered when the
 *  department requires two-factor authentication the user doesn't have yet. */
export function OrgInviteCard({
  invite,
  onAccepted,
  onOpenAccount,
}: {
  invite: OrgInvite;
  onAccepted?: () => void;
  onOpenAccount?: () => void;
}) {
  const refreshCloud = useCloudStore((s) => s.refresh);
  const refreshNotes = useNotesStore((s) => s.refresh);
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<{ reason: string; message: string } | null>(null);
  const consent = inviteConsent(invite);

  const drop = () => {
    const { status, setInvites } = useCloudStore.getState();
    setInvites((status.invites ?? []).filter((i) => i.id !== invite.id));
  };

  async function accept() {
    setBusy(true);
    setError(null);
    try {
      await cloudApi.acceptInvite(invite.id);
      drop();
      await refreshCloud();
      await refreshNotes();
      onAccepted?.();
    } catch (e) {
      const failure = cloudError(e);
      if (inviteGone(failure.reason)) {
        useRecordingStore.getState().pushFlash(failure.message);
        drop();
        return;
      }
      setError(failure);
      setConfirming(false);
      setBusy(false);
    }
  }

  async function decline() {
    setBusy(true);
    setError(null);
    try {
      await cloudApi.declineInvite(invite.id);
      drop();
    } catch (e) {
      setError(cloudError(e));
      setBusy(false);
    }
  }

  return (
    <div className="py-3.5 flex flex-col gap-3" aria-label={`Invite to ${invite.workspace_name}`} role="group">
      <div className="flex items-center justify-between gap-4">
        <div className="min-w-0">
          <div className="text-sm truncate">{invite.workspace_name}</div>
          <div className="text-xs text-[var(--color-text-muted)] truncate">{inviteFrom(invite)}</div>
        </div>
        {!confirming && (
          <div className="flex gap-2 shrink-0">
            <button type="button" className="nd-btn" onClick={decline} disabled={busy}>
              Decline
            </button>
            <button type="button" className="nd-btn" onClick={() => setConfirming(true)} disabled={busy}>
              Accept
            </button>
          </div>
        )}
      </div>
      {confirming && (
        <div className="rounded-md bg-[var(--color-pill-hover)] px-3 py-2.5 flex flex-col gap-2 text-xs">
          <p>{consent.lead}</p>
          {consent.items.length > 0 && (
            <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1">
              {consent.items.map((item) => (
                <div key={item.label} className="contents">
                  <dt className="text-[var(--color-text-muted)]">{item.label}</dt>
                  <dd>{item.value}</dd>
                </div>
              ))}
            </dl>
          )}
          {consent.reason && <p className="text-[var(--color-text-muted)]">{consent.reason}</p>}
          <p>{consent.visibility}</p>
          <div className="flex gap-2 pt-1">
            <button type="button" className="nd-btn nd-btn-primary" onClick={accept} disabled={busy}>
              {busy ? "Joining…" : `Join ${invite.workspace_name}`}
            </button>
            <button type="button" className="nd-btn" onClick={() => setConfirming(false)} disabled={busy}>
              Cancel
            </button>
          </div>
        </div>
      )}
      {error && (
        <div className="flex items-center gap-3 flex-wrap text-xs">
          <span className="text-[var(--color-danger)]">{error.message}</span>
          {error.reason === "two_factor_required" && onOpenAccount && (
            <button
              type="button"
              onClick={onOpenAccount}
              className="text-[var(--color-interactive)] hover:underline"
            >
              Open Settings → Account
            </button>
          )}
        </div>
      )}
    </div>
  );
}
