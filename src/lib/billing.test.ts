import { describe, it, expect } from "vitest";
import { hasLiveSubscription } from "./billing";
import type { CloudWorkspace, PlanStatus } from "./cloud";

const ws = (plan_status: PlanStatus): CloudWorkspace => ({
  id: "w1",
  name: "Acme",
  role: "owner",
  plan_status,
});

describe("hasLiveSubscription", () => {
  // The Personal Billing list and the seat rows both read this: a state left
  // out here is an owner who can't reach the portal to cancel.
  it.each<[PlanStatus, boolean]>([
    ["trialing", true],
    ["active", true],
    ["past_due", true],
    ["canceled", false],
    ["none", false],
  ])("%s → %s", (plan, live) => {
    expect(hasLiveSubscription(ws(plan))).toBe(live);
  });
});
