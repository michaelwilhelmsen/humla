import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { ipc, type ProviderConfig } from "./ipc";
import { useCloudStore } from "./cloud";
import { allowedUnderLocalOnly } from "./providers";

export { isLoopbackUrl } from "./loopback";

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

/** Mirrors `org_policy::OrgSummary`. */
export type OrgSummary = {
  id: string;
  name: string;
  reason: string;
  vocabulary: string[];
  /** The organisation's departments the user is in. */
  workspaces: string[];
};

export type OrgStatus = { policy: EffectivePolicy; orgs?: OrgSummary[] };

/** Who set `local_only`, or null when it is off. */
export function localOnlySource(policy: EffectivePolicy): PolicyRule | null {
  return policy.local_only ? (policy.rules.find((r) => r.kind === "local_only") ?? null) : null;
}

/** The one transcriber `local_only` always allows. Mirrors the fallback in
 *  `stt::from_legacy_settings(Some("local"), …)`. */
export const ON_DEVICE_WHISPER: ProviderConfig = {
  provider: "local",
  model_id: "large-v3-turbo-q5",
  preset: "quality",
  use_gpu: true,
};

/** Where a transcription config sends audio: null for on-device Whisper. */
export function transcribeEndpoint(cfg: ProviderConfig, customBaseUrl: string): string | null {
  switch (cfg.provider) {
    case "custom":
      return customBaseUrl.trim() || null;
    case "openai":
    case "deepgram":
      return cfg.base_url ?? null;
    case "local":
    case "groq":
      return null;
  }
}

/** `cfg` itself when `local_only` allows it, else on-device Whisper. Mirrors
 *  `transcription_config::enforce` for one slot. */
export function enforceLocalOnly(cfg: ProviderConfig, customBaseUrl: string): ProviderConfig {
  return allowedUnderLocalOnly(cfg.provider, transcribeEndpoint(cfg, customBaseUrl)) ? cfg : ON_DEVICE_WHISPER;
}

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
  return useOrgPolicyStatus().policy;
}

/** Who sets `local_only` (null when off), and whether the policy has loaded.
 *  Until it has, a settings tab touches no cloud provider's key or models. */
export function useLocalOnly(): { managed: PolicyRule | null; known: boolean } {
  const { policy, known } = useOrgPolicyStatus();
  return { managed: localOnlySource(policy), known };
}

function useOrgPolicyStatus(): { policy: EffectivePolicy; known: boolean } {
  const [state, setState] = useState({ policy: NO_POLICY, known: false });
  const cloudStatus = useCloudStore((s) => s.status);

  useEffect(() => {
    let cancelled = false;
    const load = () =>
      ipc
        .orgStatus()
        .then((s) => !cancelled && setState({ policy: s?.policy ?? NO_POLICY, known: true }))
        .catch(() => !cancelled && setState({ policy: NO_POLICY, known: true }));
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

  return state;
}
