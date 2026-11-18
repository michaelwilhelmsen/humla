import type { OrgInvite } from "./cloud";
import { deletedAfter, POLICY_LABEL } from "./orgPolicy";

export type ConsentItem = { label: string; value: string };

export type InviteConsent = {
  lead: string;
  /** The rules that are on, in the Policies list's words. */
  items: ConsentItem[];
  reason: string;
  visibility: string;
};

/** Where an invite comes from: "Bergen kommune · Invited by Kari". */
export function inviteFrom(invite: OrgInvite): string {
  const parts = [invite.organisation_name, invite.invited_by_name && `Invited by ${invite.invited_by_name}`];
  if (invite.role === "admin") parts.push("as admin");
  return parts.filter(Boolean).join(" · ");
}

/** What joining means, shown before accepting. Without the invite's policy
 *  (an older server) it says what an organisation can set, never which. */
export function inviteConsent(invite: OrgInvite): InviteConsent {
  const name = invite.organisation_name.trim();
  const org = name || "The organisation";
  const orgs = name ? `${name}'s` : "The organisation's";
  const visibility = `Your name will show to ${name ? `${name}'s` : "the organisation's"} admins.`;
  const policy = invite.policy;
  if (!policy) {
    return {
      lead: `${orgs} policies will apply to Humla on this Mac. They can keep AI processing on this Mac, turn off keeping recorded audio, and delete notes after a set time.`,
      items: [],
      reason: "",
      visibility,
    };
  }
  const items: ConsentItem[] = [];
  if (policy.local_only) items.push({ label: POLICY_LABEL.local_only, value: "On this Mac only" });
  if (policy.retention_days > 0) {
    items.push({ label: POLICY_LABEL.retention_days, value: deletedAfter(policy.retention_days) });
  }
  if (policy.audio_forbidden) items.push({ label: POLICY_LABEL.audio_forbidden, value: "Off" });
  if (policy.telemetry_off) items.push({ label: POLICY_LABEL.telemetry_off, value: "Off" });
  return {
    lead: items.length
      ? `${orgs} policies will apply to Humla on this Mac:`
      : `${org} sets no policies on this Mac.`,
    items,
    reason: items.length ? (invite.policy_reason ?? "").trim() : "",
    visibility,
  };
}
