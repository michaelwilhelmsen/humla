import type { OrgInvite } from "./cloud";
import { AUDIT_READS_LABEL, AUDIT_READS_VALUE, POLICY_LABEL } from "./orgPolicy";

export type ConsentItem = { label: string; value: string };

/** Where an invite comes from: "Bergen kommune · Invited by Kari". */
export function inviteFrom(invite: OrgInvite): string {
  const parts = [invite.organisation_name, invite.invited_by_name && `Invited by ${invite.invited_by_name}`];
  if (invite.role === "admin") parts.push("as admin");
  return parts.filter(Boolean).join(" · ");
}

/** What joining means, shown before accepting. The invite doesn't carry the
 *  organisation's policies, so this names what they can set, in the Policies
 *  list's words. */
export function inviteConsent(invite: OrgInvite): { lead: string; items: ConsentItem[] } {
  const org = invite.organisation_name.trim();
  return {
    lead: `${org ? `${org}'s` : "The organisation's"} policies will apply to Humla on this Mac, such as:`,
    items: [
      { label: POLICY_LABEL.local_only, value: "On this Mac only" },
      { label: POLICY_LABEL.audio_forbidden, value: "Off, and stored audio deleted" },
      { label: POLICY_LABEL.retention_days, value: "Deleted after a set number of days" },
      { label: POLICY_LABEL.telemetry_off, value: "Off" },
      { label: AUDIT_READS_LABEL, value: AUDIT_READS_VALUE },
    ],
  };
}
