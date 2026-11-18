import { describe, expect, it } from "vitest";
import { inviteConsent, inviteFrom } from "./orgInvites";
import { policyRows, NO_POLICY, type PolicyKind } from "./orgPolicy";
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

const EVERY = { local_only: true, retention_days: 30, telemetry_off: true, audio_forbidden: true };

describe("an invite's origin", () => {
  it("names the organisation and who invited", () => {
    expect(inviteFrom(INVITE)).toBe("Bergen kommune · Invited by Kari");
  });

  it("says when it is an admin invite and leaves out a missing inviter", () => {
    expect(inviteFrom({ ...INVITE, role: "admin", invited_by_name: "" })).toBe("Bergen kommune · as admin");
  });
});

describe("what joining means", () => {
  it("lists only the rules that are on, with the reason", () => {
    const consent = inviteConsent({
      ...INVITE,
      policy: { local_only: true, retention_days: 90, telemetry_off: false, audio_forbidden: false },
      policy_reason: "Health data stays here.",
    });
    expect(consent.lead).toBe("Bergen kommune's policies will apply to Humla on this Mac:");
    expect(consent.items).toEqual([
      { label: "AI processing", value: "On this Mac only" },
      { label: "Note retention", value: "Deleted after 90 days" },
    ]);
    expect(consent.reason).toBe("Health data stays here.");
  });

  it("matches the Policies list's rows for the same rules", () => {
    const kinds: PolicyKind[] = ["local_only", "retention_days", "telemetry_off", "audio_forbidden"];
    const listed = policyRows({
      ...NO_POLICY,
      ...EVERY,
      rules: kinds.map((kind) => ({ kind, source: "Org", reason: "" })),
    }).map(({ label, value }) => ({ label, value }));
    expect(inviteConsent({ ...INVITE, policy: EVERY }).items).toEqual(listed);
  });

  it("says when the organisation sets nothing", () => {
    const none = { local_only: false, retention_days: 0, telemetry_off: false, audio_forbidden: false };
    const consent = inviteConsent({ ...INVITE, policy: none, policy_reason: "Unused." });
    expect(consent.lead).toBe("Bergen kommune sets no policies on this Mac.");
    expect(consent.items).toEqual([]);
    expect(consent.reason).toBe("");
  });

  it("states no definite rule without the invite's policy", () => {
    const consent = inviteConsent(INVITE);
    expect(consent.lead).toMatch(/^Bergen kommune's policies will apply to Humla on this Mac\. They can /);
    expect(consent.items).toEqual([]);
    expect(inviteConsent({ ...INVITE, organisation_name: "" }).lead).toMatch(/^The organisation's policies/);
  });

  it("says the organisation's admins will see their name", () => {
    expect(inviteConsent(INVITE).visibility).toBe("Your name will show to Bergen kommune's admins.");
    expect(inviteConsent({ ...INVITE, policy: EVERY }).visibility).toBe("Your name will show to Bergen kommune's admins.");
  });
});
