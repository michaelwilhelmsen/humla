import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { ipc, type ProviderConfig } from "./ipc";
import { useCloudStore } from "./cloud";
import { useActiveWorkspaceId } from "./orgContent";
import { allowedUnderLocalOnly } from "./providers";
import type { AuditScope } from "./audit";

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
  /** Opening its notes is recorded. Absent on a summary the client builds for
   *  an organisation it only administers. */
  audit_reads?: boolean;
};

/** Mirrors `commands::org_admin::AdminOrg`. */
export type AdminOrg = { id: string; name: string; require_2fa: boolean };

/** Mirrors `commands::org_admin::OrgTemplateDraft`: `workspaces` are the
 *  user's own departments, `hidden` counts the others it is offered to. */
export type OrgTemplateDraft = { name: string; content: string; workspaces: string[]; hidden: number };

/** Mirrors `commands::org_admin::AdminContent`: an administered organisation
 *  as the server holds it. Edited only, never applied on this Mac. A
 *  template's `workspaces` are every department it is offered in; empty is all. */
export type AdminTemplate = { client_id: string; name: string; content: string; workspaces: string[] };
export type AdminContent = { templates: AdminTemplate[]; vocabulary: string[] };

/** Days of retention per workspace, from `org_policy::retention_scope`. */
export type RetentionScope = Record<string, number>;

/** Mirrors `org_policy::OrgPolicies`: one organisation's own policies and the
 *  departments they govern. */
export type OrgPolicies = { org_id: string; workspaces: string[]; policy: EffectivePolicy };

/** Mirrors `commands::org::OrgStatus`. `policy` is for notes in the workspace
 *  asked about; `device` is what applies app-wide; `managed` is the managed
 *  configuration profile alone. */
export type OrgStatus = {
  policy: EffectivePolicy;
  device?: EffectivePolicy;
  managed?: EffectivePolicy;
  orgs?: OrgSummary[];
  org_policies?: OrgPolicies[];
  /** The organisation whose policies govern notes in each department. */
  governed?: Record<string, string>;
  retention?: RetentionScope;
  audit?: AuditScope;
};

/** Mirrors `org_policy::AudioSweep`: `all` sweeps every note, otherwise the
 *  organisation's departments. */
export type AudioSweep = { org_id: string; org_name: string; all?: boolean };

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

export type PolicyRow = { key: string; label: string; value: string; source: string; reason: string };

export const POLICY_ORDER: PolicyKind[] = ["local_only", "retention_days", "audio_forbidden", "telemetry_off"];

/** The setting each policy fixes, as the Policies list names it. */
export const POLICY_LABEL: Record<PolicyKind, string> = {
  local_only: "AI processing",
  retention_days: "Note retention",
  audio_forbidden: "Keep recorded audio",
  telemetry_off: "Send anonymous usage counters",
};

export const AUDIT_READS_LABEL = "Opening notes";
export const AUDIT_READS_VALUE = "Recorded";

/** One row per policy in force: the setting it fixes and its enforced value.
 *  Then one per organisation in `orgs` that records opening its notes. */
export function policyRows(policy: EffectivePolicy, orgs: OrgSummary[] = []): PolicyRow[] {
  const describe = (kind: PolicyKind): { label: string; value: string } => {
    const label = POLICY_LABEL[kind];
    switch (kind) {
      case "local_only":
        return { label, value: "On this Mac only" };
      case "retention_days":
        return { label, value: deletedAfter(policy.retention_days) };
      case "audio_forbidden":
      case "telemetry_off":
        return { label, value: "Off" };
    }
  };
  const enforced = POLICY_ORDER.flatMap((kind) => {
    const rule = policy.rules.find((r) => r.kind === kind);
    return rule ? [{ key: kind, ...describe(kind), source: rule.source, reason: rule.reason }] : [];
  });
  const audited = orgs
    .filter((o) => o.audit_reads)
    .map((o) => ({
      key: `audit_reads:${o.id}`,
      label: AUDIT_READS_LABEL,
      value: AUDIT_READS_VALUE,
      source: o.name,
      reason: o.reason,
    }));
  return [...enforced, ...audited];
}

/** The policies one source enforces, and the departments they apply to. A
 *  managed profile governs every workspace. */
export type PolicyGroup = { key: string; source: string; workspaces: string[]; everyWorkspace: boolean; rows: PolicyRow[] };

/** Each organisation's rules with its departments, then the managed profile's. */
export function policyGroups(status: Pick<PolicyStatus, "orgPolicies" | "orgs" | "managed">): PolicyGroup[] {
  const groups: PolicyGroup[] = status.orgPolicies.flatMap((op) => {
    const org = status.orgs.find((o) => o.id === op.org_id);
    const rows = policyRows(op.policy, org ? [org] : []);
    const source = org?.name ?? op.policy.org_name ?? "";
    return rows.length > 0 ? [{ key: op.org_id, source, workspaces: op.workspaces, everyWorkspace: false, rows }] : [];
  });
  const managed = policyRows(status.managed);
  if (managed.length > 0)
    groups.push({ key: "managed", source: MANAGED_SOURCE, workspaces: [], everyWorkspace: true, rows: managed });
  return groups;
}

/**
 * The policies for notes in the active workspace, for display. Reloads when
 * cloud status changes (which refetches the organisation config), when the
 * workspace switches, and when the backend reports a change. A failed read
 * shows nothing; nothing enforces from this value.
 */
export function useOrgPolicy(): EffectivePolicy {
  return useOrgPolicyStatus(useActiveWorkspaceId()).policy;
}

/** What applies app-wide: the managed profile and every organisation's
 *  `telemetry_off`. */
export function useDevicePolicy(): EffectivePolicy {
  return useOrgPolicyStatus("").device;
}

/** The managed configuration profile alone, which governs the user's own
 *  settings everywhere (the MCP switch, the webhook receiver). */
export function useManagedPolicy(): EffectivePolicy {
  return useOrgPolicyStatus("").managed;
}

/** [`policyGroups`] for the organisations and profile in force. */
export function usePolicyGroups(): PolicyGroup[] {
  return policyGroups(useOrgPolicyStatus(""));
}

/** What the organisations ask of a note in `workspaceId`: its policies, the
 *  organisation that governs it (null when none), days before it is deleted (0
 *  when none), and the audit scope its opens and copies are reported against. */
export function useNoteOrgPolicy(workspaceId: string | null | undefined): {
  policy: EffectivePolicy;
  managedBy: string | null;
  retentionDays: number;
  audit: AuditScope;
} {
  const ws = workspaceId?.trim() ?? "";
  const { policy, governed, retention, audit } = useOrgPolicyStatus(ws);
  const named = ws && Object.prototype.hasOwnProperty.call(governed, ws) ? governed[ws] : null;
  return {
    policy,
    managedBy: named ?? (ws ? organisationRuleSource(policy) : null),
    retentionDays: retentionDaysFor(retention, ws),
    audit,
  };
}

/** The organisation departments the user is in. */
export function useOrgWorkspaces(): string[] {
  return useOrgPolicyStatus("").orgs.flatMap((o) => o.workspaces);
}

/** Who sets `local_only` for notes in the active workspace (null when off),
 *  and whether the policy has loaded. Until it has, a settings tab touches no
 *  cloud provider's key or models. */
export function useLocalOnly(): { managed: PolicyRule | null; known: boolean } {
  const { policy, known } = useOrgPolicyStatus(useActiveWorkspaceId());
  return { managed: localOnlySource(policy), known };
}

/** Who among the organisations governs notes under `policy`, or null: the
 *  source of its first note rule that isn't the managed profile. */
export function organisationRuleSource(policy: EffectivePolicy): string | null {
  const kinds: PolicyKind[] = ["local_only", "audio_forbidden", "retention_days"];
  return policy.rules.find((r) => kinds.includes(r.kind) && r.source !== MANAGED_SOURCE)?.source ?? null;
}

/** The workspace name Settings shows on a section a policy can govern, or
 *  undefined when no organisation is cached, so Settings stay as they were. */
export function useWorkspaceScope(): string | undefined {
  const { orgs } = useOrgPolicyStatus("");
  const name = useCloudStore(
    (s) => s.status.current_workspace?.name ?? s.status.locked_current_workspace?.name ?? "Personal",
  );
  return orgs.length > 0 ? name : undefined;
}

type PolicyStatus = {
  policy: EffectivePolicy;
  device: EffectivePolicy;
  managed: EffectivePolicy;
  orgPolicies: OrgPolicies[];
  governed: Record<string, string>;
  retention: RetentionScope;
  audit: AuditScope;
  orgs: OrgSummary[];
  known: boolean;
};

const UNKNOWN: PolicyStatus = {
  policy: NO_POLICY,
  device: NO_POLICY,
  managed: NO_POLICY,
  orgPolicies: [],
  governed: {},
  retention: {},
  audit: {},
  orgs: [],
  known: false,
};

function useOrgPolicyStatus(workspaceId: string): PolicyStatus {
  const [state, setState] = useState<PolicyStatus>(UNKNOWN);
  const cloudStatus = useCloudStore((s) => s.status);
  const loadedFor = useRef(workspaceId);

  useEffect(() => {
    let cancelled = false;
    // The previous workspace's policy would show its rows until this one
    // lands; what isn't per workspace stays.
    if (loadedFor.current !== workspaceId) {
      loadedFor.current = workspaceId;
      setState((s) => ({ ...s, policy: NO_POLICY, known: false }));
    }
    const load = () =>
      ipc
        .orgStatus(workspaceId)
        .then(
          (s) =>
            !cancelled &&
            setState({
              policy: s?.policy ?? NO_POLICY,
              device: s?.device ?? NO_POLICY,
              managed: s?.managed ?? NO_POLICY,
              orgPolicies: s?.org_policies ?? [],
              governed: s?.governed ?? {},
              retention: s?.retention ?? {},
              audit: s?.audit ?? {},
              orgs: s?.orgs ?? [],
              known: true,
            }),
        )
        .catch(() => !cancelled && setState({ ...UNKNOWN, known: true }));
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
  }, [cloudStatus, workspaceId]);

  return state;
}
