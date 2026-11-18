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

/** Who a managed configuration profile's rules name. Mirrors
 *  `org_policy::MANAGED_SOURCE`. */
export const MANAGED_SOURCE = "This Mac's configuration";

/** Mirrors `org_policy::OrgSummary`. */
export type OrgSummary = {
  id: string;
  name: string;
  reason: string;
  vocabulary: string[];
  /** The organisation's departments the user is in. */
  workspaces: string[];
};

/** Mirrors `commands::org_admin::AdminOrg`. */
export type AdminOrg = { id: string; name: string; require_2fa: boolean };

/** Mirrors `commands::org_admin::OrgTemplateDraft`: `workspaces` are the
 *  user's own departments, `hidden` counts the others it is offered to. */
export type OrgTemplateDraft = { name: string; content: string; workspaces: string[]; hidden: number };

/** Mirrors `commands::org_admin::AdminContent`: an organisation the user
 *  administers without being in any of its departments. Edited only, never
 *  applied on this Mac. `departments` counts where a template is offered; 0 is
 *  every department. */
export type AdminTemplate = { client_id: string; name: string; content: string; departments: number };
export type AdminContent = { templates: AdminTemplate[]; vocabulary: string[] };

/** Days of retention per workspace, from `org_policy::retention_scope`. */
export type RetentionScope = Record<string, number>;

export type OrgStatus = { policy: EffectivePolicy; orgs?: OrgSummary[]; retention?: RetentionScope };

/** Mirrors `org_policy::AudioSweep`. */
export type AudioSweep = { org_id: string; org_name: string };

/** Who set `kind`, or null when no rule enforces it. */
export function policySource(policy: EffectivePolicy, kind: PolicyKind): PolicyRule | null {
  return policy.rules.find((r) => r.kind === kind) ?? null;
}

/** Who set `local_only`, or null when it is off. */
export function localOnlySource(policy: EffectivePolicy): PolicyRule | null {
  return policy.local_only ? policySource(policy, "local_only") : null;
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

const DAY_MS = 86_400_000;

/** The smallest `created_at` read as a real time. Mirrors `org_policy::MIN_EPOCH_MS`. */
export const MIN_EPOCH_MS = 1_000_000_000_000;

/** Under `days` of retention, the dates a note created at `createdAt` may be
 *  moved to at `now`, inclusive: never later than it is, never past the cutoff.
 *  Mirrors `org_policy::redate_bounds`; `redateVectors.json` pins both. */
export function redateBounds(days: number, createdAt: number, now: number): { min: number; max: number } {
  const min = Math.max(now - days * DAY_MS, MIN_EPOCH_MS);
  const max = createdAt >= MIN_EPOCH_MS ? createdAt : now;
  return { min, max };
}

export function deletedAfter(days: number): string {
  return `Deleted after ${days} ${days === 1 ? "day" : "days"}`;
}

/** Days before a note in `workspaceId` is deleted; 0 when none applies. A
 *  Personal note (blank workspace) never has one. */
export function retentionDaysFor(scope: RetentionScope, workspaceId: string | null | undefined): number {
  if (!workspaceId?.trim()) return 0;
  return Object.prototype.hasOwnProperty.call(scope, workspaceId) ? scope[workspaceId] : 0;
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
      case "retention_days":
        return { label: "Note retention", value: deletedAfter(policy.retention_days) };
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

/** Days before a note in `workspaceId` is deleted under its organisation's
 *  retention; 0 when none applies. */
export function useRetentionDays(workspaceId: string | null | undefined): number {
  return retentionDaysFor(useOrgPolicyStatus().retention, workspaceId);
}

/** Who sets `local_only` (null when off), and whether the policy has loaded.
 *  Until it has, a settings tab touches no cloud provider's key or models. */
export function useLocalOnly(): { managed: PolicyRule | null; known: boolean } {
  const { policy, known } = useOrgPolicyStatus();
  return { managed: localOnlySource(policy), known };
}

type PolicyStatus = { policy: EffectivePolicy; retention: RetentionScope; known: boolean };

function useOrgPolicyStatus(): PolicyStatus {
  const [state, setState] = useState<PolicyStatus>({ policy: NO_POLICY, retention: {}, known: false });
  const cloudStatus = useCloudStore((s) => s.status);

  useEffect(() => {
    let cancelled = false;
    const load = () =>
      ipc
        .orgStatus()
        .then(
          (s) =>
            !cancelled && setState({ policy: s?.policy ?? NO_POLICY, retention: s?.retention ?? {}, known: true }),
        )
        .catch(() => !cancelled && setState({ policy: NO_POLICY, retention: {}, known: true }));
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
