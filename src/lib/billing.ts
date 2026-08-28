// Stripe Checkout / Customer Portal handoff, shared by every surface that has
// to get a workspace's plan live: the onboarding wizard's Cloud step and the
// New workspace modal.
//
// The shape of the interaction is fixed by Stripe being *hosted*: we can only
// open a URL in the user's browser and then wait to be told it worked. There is
// no callback into the app, so "wait" means polling `cloud_status` until the
// server reports the workspace's plan flipped. That poll is the whole reason
// this lives in one place — a second copy would be a second set of timings, a
// second teardown, and a second chance to leak an interval past unmount.

import { useCallback, useEffect, useState } from "react";
import { open as openExternal } from "@tauri-apps/plugin-shell";
import { cloudApi, useCloudStore, type CheckoutSource, type CloudWorkspace } from "./cloud";

export const CHECKOUT_POLL_MS = 3000;
/** Soft ceiling on the poll. Past it we stop watching and say so CALMLY — the
 *  workspace is saved either way, so an abandoned checkout is not an error. */
export const CHECKOUT_TIMEOUT_MS = 5 * 60 * 1000;

/** "idle" = nothing in flight · "waiting" = polling after opening Stripe ·
 *  "timeout" = gave up watching (never an error state). */
export type CheckoutState = "idle" | "waiting" | "timeout";

/** A workspace whose plan lets people sync and edit. Trialing counts. */
export function planIsLive(ws: CloudWorkspace | null | undefined): boolean {
  return ws?.plan_status === "trialing" || ws?.plan_status === "active";
}

/**
 * Which Stripe surface a workspace needs, given its plan:
 *
 * - `none` → Checkout, and the server grants the 14-day trial only to a
 *   workspace that never had a subscription, so this is the ONLY state whose
 *   CTA may promise a trial.
 * - `canceled` → Checkout, but re-subscribing carries no new trial.
 * - `past_due` → the Customer **Portal**, never Checkout: the subscription
 *   still exists on Stripe, and a fresh Checkout would create a second one and
 *   bill the owner twice.
 */
export function checkoutKind(ws: CloudWorkspace): "checkout" | "portal" {
  return ws.plan_status === "past_due" ? "portal" : "checkout";
}

/** True when this workspace's CTA may promise the 14-day trial. */
export function offersTrial(ws: CloudWorkspace): boolean {
  return ws.plan_status === "none";
}

/**
 * The one place a workspace's plan turns into a call to action. Both the sheet's
 * trial stage and a note's read-only banner ask for it, so neither re-derives
 * the cascade — the previous shape had `past_due` spelled out in three separate
 * conditionals, which is how "past_due opens the Portal, not Checkout" quietly
 * becomes untrue in one of them and double-bills the owner.
 */
export function billingCta(ws: CloudWorkspace): {
  kind: "checkout" | "portal";
  trial: boolean;
  label: string;
} {
  const kind = checkoutKind(ws);
  const trial = offersTrial(ws);
  return {
    kind,
    trial,
    label: kind === "portal" ? "Fix payment" : trial ? "Start free trial" : "Subscribe",
  };
}

/**
 * Drive one workspace's checkout: open Stripe, then poll until its plan goes
 * live. Returns `state` for the caller to render around; the caller is expected
 * to derive "we're done" from the workspace's own `plan_status` rather than
 * from anything here, so a plan that goes live by some other route (a teammate,
 * a webhook landing late, the Portal) lands in the same place.
 *
 * `source` is REQUIRED: this hook is the funnel's only choke point, and every
 * surface that reaches Stripe through it must say which one it is. Making it
 * optional here is how attribution goes blank without anything breaking — the
 * server drops an absent source silently, by design, so nothing would complain.
 */
export function useCheckout(workspaceId: string | null, source: CheckoutSource) {
  const refresh = useCloudStore((s) => s.refresh);
  const [state, setState] = useState<CheckoutState>("idle");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const start = useCallback(
    async (kind: "checkout" | "portal") => {
      if (!workspaceId) return;
      setBusy(true);
      setError(null);
      try {
        const url =
          kind === "portal"
            ? await cloudApi.billingPortal(workspaceId)
            : await cloudApi.billingCheckout(workspaceId, source);
        await openExternal(url);
        setState("waiting");
      } catch (e) {
        setError(String(e));
      } finally {
        setBusy(false);
      }
    },
    [workspaceId, source],
  );

  // Poll while waiting. Cancellation flag + interval teardown so this can't
  // outlive the modal being dismissed mid-checkout.
  useEffect(() => {
    if (state !== "waiting" || !workspaceId) return;
    let cancelled = false;
    const startedAt = Date.now();

    const tick = async () => {
      try {
        await refresh();
      } catch {
        // Transient — keep polling; the ceiling below handles a real outage.
      }
      if (cancelled) return;
      const s = useCloudStore.getState().status;
      const ws =
        s.workspaces.find((w) => w.id === workspaceId) ??
        (s.current_workspace?.id === workspaceId ? s.current_workspace : null);
      if (planIsLive(ws)) {
        // Stop watching; the caller's own derivation moves the UI on.
        setState("idle");
        return;
      }
      if (Date.now() - startedAt >= CHECKOUT_TIMEOUT_MS) setState("timeout");
    };

    const timer = window.setInterval(() => void tick(), CHECKOUT_POLL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [state, workspaceId, refresh]);

  return { state, busy, error, start };
}
