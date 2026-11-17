import { describe, expect, it } from "vitest";
import { NO_POLICY, policyRows, type EffectivePolicy } from "./orgPolicy";

const rule = (kind: EffectivePolicy["rules"][number]["kind"], source = "Northwind") => ({
  kind,
  source,
  reason: "Nothing leaves this Mac.",
});

describe("policyRows", () => {
  it("is empty when nothing is enforced", () => {
    expect(policyRows(NO_POLICY)).toEqual([]);
  });

  it("lists each policy in force with its enforced value and source", () => {
    const policy: EffectivePolicy = {
      ...NO_POLICY,
      org_name: "Northwind",
      local_only: true,
      retention_days: 30,
      telemetry_off: true,
      audio_forbidden: true,
      rules: [rule("telemetry_off", "Contoso"), rule("local_only"), rule("audio_forbidden"), rule("retention_days")],
    };
    expect(policyRows(policy).map((r) => [r.label, r.value, r.source])).toEqual([
      ["AI processing", "On this Mac only", "Northwind"],
      ["Note retention", "Deleted after 30 days", "Northwind"],
      ["Keep recorded audio", "Off", "Northwind"],
      ["Send anonymous usage counters", "Off", "Contoso"],
    ]);
  });

  it("says one day in the singular", () => {
    const policy = { ...NO_POLICY, retention_days: 1, rules: [rule("retention_days")] };
    expect(policyRows(policy)[0].value).toBe("Deleted after 1 day");
  });
});
