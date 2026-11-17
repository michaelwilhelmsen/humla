import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { ipc } from "./ipc";
import { useCloudStore } from "./cloud";

// Mirrors `org_policy::EffectivePolicy` and `commands::org::OrgStatus`.
export type PolicyKind = "local_only" | "retention_days" | "telemetry_off" | "audio_forbidden";

export type PolicyRule = { kind: PolicyKind; source: string; reason: string };

export type EffectivePolicy = {
  org_name: string | null;
  reason: string | null;
  local_only: boolean;
  /** 0 is off. */
  retention_days: number;
  telemetry_off: boolean;
  audio_forbidden: boolean;
  rules: PolicyRule[];
};

export type OrgStatus = { policy: EffectivePolicy };

export const NO_POLICY: EffectivePolicy = {
  org_name: null,
  reason: null,
  local_only: false,
  retention_days: 0,
  telemetry_off: false,
  audio_forbidden: false,
  rules: [],
};

export type PolicyRow = { kind: PolicyKind; label: string; value: string; source: string; reason: string };

const ORDER: PolicyKind[] = ["local_only", "retention_days", "audio_forbidden", "telemetry_off"];

/** One row per policy in force: the setting it fixes and its enforced value. */
export function policyRows(policy: EffectivePolicy): PolicyRow[] {
  const describe = (kind: PolicyKind): { label: string; value: string } => {
    switch (kind) {
      case "local_only":
        return { label: "AI processing", value: "On this Mac only" };
      case "retention_days": {
        const n = policy.retention_days;
        return { label: "Note retention", value: `Deleted after ${n} ${n === 1 ? "day" : "days"}` };
      }
      case "audio_forbidden":
        return { label: "Keep recorded audio", value: "Off" };
      case "telemetry_off":
        return { label: "Send anonymous usage counters", value: "Off" };
    }
  };
  return ORDER.flatMap((kind) => {
    const rule = policy.rules.find((r) => r.kind === kind);
    return rule ? [{ kind, ...describe(kind), source: rule.source, reason: rule.reason }] : [];
  });
}

/**
 * The policies in force, for display. Reloads when cloud status changes (which
 * refetches the organisation config) and when the backend reports a change.
 * A failed read shows nothing; nothing enforces from this value.
 */
export function useOrgPolicy(): EffectivePolicy {
  const [policy, setPolicy] = useState<EffectivePolicy>(NO_POLICY);
  const cloudStatus = useCloudStore((s) => s.status);

  useEffect(() => {
    let cancelled = false;
    const load = () =>
      ipc
        .orgStatus()
        .then((s) => !cancelled && setPolicy(s?.policy ?? NO_POLICY))
        .catch(() => !cancelled && setPolicy(NO_POLICY));
    void load();
    let unlisten: (() => void) | undefined;
    void listen("org_policy_changed", () => void load()).then((u) => {
      if (cancelled) u();
      else unlisten = u;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [cloudStatus]);

  return policy;
}
