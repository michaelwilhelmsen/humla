import { describe, expect, it } from "vitest";
import { inviteConsent, inviteFrom } from "./orgInvites";
import { policyRows, POLICY_LABEL, AUDIT_READS_LABEL, NO_POLICY } from "./orgPolicy";
import type { OrgInvite } from "./cloud";

const INVITE: OrgInvite = {
  id: "i1",
  workspace_id: "ws1",
  workspace_name: "Child welfare",
  organisation_name: "Bergen kommune",
  role: "member",
  invited_by_name: "Kari",
  created: "2026-11-18 10:00:00.000Z",
};

describe("an invite's origin", () => {
  it("names the organisation and who invited", () => {
    expect(inviteFrom(INVITE)).toBe("Bergen kommune · Invited by Kari");
  });

  it("says when it is an admin invite and leaves out a missing inviter", () => {
    expect(inviteFrom({ ...INVITE, role: "admin", invited_by_name: "" })).toBe("Bergen kommune · as admin");
  });
});

describe("what joining means", () => {
  it("says the organisation's policies apply to this Mac", () => {
    expect(inviteConsent(INVITE).lead).toBe("Bergen kommune's policies will apply to Humla on this Mac, such as:");
    expect(inviteConsent({ ...INVITE, organisation_name: "" }).lead).toMatch(/^The organisation's policies/);
  });

  it("names local-only processing, stored audio and retention", () => {
    const items = inviteConsent(INVITE).items;
    expect(items).toContainEqual({ label: "AI processing", value: "On this Mac only" });
    expect(items).toContainEqual({ label: "Keep recorded audio", value: "Off, and stored audio deleted" });
    expect(items).toContainEqual({ label: "Note retention", value: "Deleted after a set number of days" });
  });

  it("uses the Policies list's labels", () => {
    const every = {
      ...NO_POLICY,
      local_only: true,
      retention_days: 30,
      telemetry_off: true,
      audio_forbidden: true,
      rules: (["local_only", "retention_days", "telemetry_off", "audio_forbidden"] as const).map((kind) => ({
        kind,
        source: "Org",
        reason: "",
      })),
    };
    const listed = policyRows(every, [
      { id: "o1", name: "Org", reason: "", vocabulary: [], workspaces: [], audit_reads: true },
    ]).map((r) => r.label);
    for (const item of inviteConsent(INVITE).items) expect(listed).toContain(item.label);
    expect(listed).toContain(POLICY_LABEL.local_only);
    expect(listed).toContain(AUDIT_READS_LABEL);
  });
});
