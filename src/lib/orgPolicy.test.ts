import { describe, expect, it } from "vitest";
import sttConfig from "../../src-tauri/src/stt/config.rs?raw";
import policySrc from "../../src-tauri/src/org_policy.rs?raw";
import vectors from "./loopbackVectors.json";
import redate from "./redateVectors.json";
import {
  MANAGED_SOURCE,
  NO_POLICY,
  ON_DEVICE_WHISPER,
  enforceLocalOnly,
  isLoopbackUrl,
  localOnlySource,
  policySource,
  policyRows,
  redateBounds,
  retentionDaysFor,
  type EffectivePolicy,
} from "./orgPolicy";

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

  it("adds a row for each organisation that records opening its notes", () => {
    const org = (id: string, name: string, audit_reads?: boolean) => ({
      id,
      name,
      reason: `${name} says so`,
      vocabulary: [],
      workspaces: [],
      audit_reads,
    });
    const rows = policyRows(NO_POLICY, [org("o1", "Northwind", true), org("o2", "Contoso", false), org("o3", "Fabrikam")]);
    expect(rows.map((r) => [r.label, r.value, r.source, r.reason])).toEqual([
      ["Opening notes", "Recorded", "Northwind", "Northwind says so"],
    ]);
  });

  it("says one day in the singular", () => {
    const policy = { ...NO_POLICY, retention_days: 1, rules: [rule("retention_days")] };
    expect(policyRows(policy)[0].value).toBe("Deleted after 1 day");
  });
});

describe("retentionDaysFor", () => {
  const scope = { w1: 30, w2: 7 };

  it("reads the workspace's days from the backend's map", () => {
    expect(retentionDaysFor(scope, "w1")).toBe(30);
    expect(retentionDaysFor(scope, "w2")).toBe(7);
  });

  it("is 0 for Personal, an unknown workspace and no organisation", () => {
    expect(retentionDaysFor(scope, "")).toBe(0);
    expect(retentionDaysFor(scope, " ")).toBe(0);
    expect(retentionDaysFor(scope, null)).toBe(0);
    expect(retentionDaysFor(scope, undefined)).toBe(0);
    expect(retentionDaysFor(scope, "w3")).toBe(0);
    expect(retentionDaysFor(scope, "constructor")).toBe(0);
    expect(retentionDaysFor({}, "w1")).toBe(0);
  });
});

describe("redateBounds mirrors org_policy::redate_bounds", () => {
  it.each(redate.cases)("$days days, created $created_at", (c) => {
    expect(redateBounds(c.days, c.created_at, redate.now)).toEqual({ min: c.min, max: c.max });
  });
});

describe("isLoopbackUrl mirrors org_policy::is_loopback_url", () => {
  it.each(vectors.loopback)("%s is loopback", (url) => {
    expect(isLoopbackUrl(url)).toBe(true);
  });
  it.each(vectors.elsewhere)("%s is not", (url) => {
    expect(isLoopbackUrl(url)).toBe(false);
  });
});

describe("enforceLocalOnly", () => {
  it("keeps what local_only allows and moves the rest to on-device Whisper", () => {
    const whisper = { provider: "local", model_id: "nb-whisper-large-q5", preset: "fast", use_gpu: false } as const;
    expect(enforceLocalOnly(whisper, "")).toBe(whisper);
    const custom = { provider: "custom", model: "m" } as const;
    expect(enforceLocalOnly(custom, "http://localhost:9000/v1")).toBe(custom);
    expect(enforceLocalOnly(custom, "https://api.example.com/v1")).toEqual(ON_DEVICE_WHISPER);
    expect(enforceLocalOnly({ provider: "openai", model: "whisper-1", base_url: "http://localhost:1/v1" }, "")).toEqual(
      ON_DEVICE_WHISPER,
    );
    expect(enforceLocalOnly({ provider: "deepgram", model: "nova-3" }, "")).toEqual(ON_DEVICE_WHISPER);
  });

  it("uses the same on-device default as stt::from_legacy_settings", () => {
    const model = /whisper_model_id\.unwrap_or\("([^"]+)"\)/.exec(sttConfig)![1];
    const preset = /whisper_preset\.unwrap_or\("([^"]+)"\)/.exec(sttConfig)![1];
    expect(ON_DEVICE_WHISPER).toEqual({ provider: "local", model_id: model, preset, use_gpu: true });
  });
});

describe("localOnlySource", () => {
  it("names who set local_only, and nothing when it is off", () => {
    expect(localOnlySource(NO_POLICY)).toBeNull();
    const policy = { ...NO_POLICY, local_only: true, rules: [rule("telemetry_off", "Contoso"), rule("local_only")] };
    expect(localOnlySource(policy)?.source).toBe("Northwind");
  });
});

describe("policySource", () => {
  it("names who sets each policy, and nobody without a rule", () => {
    const policy: EffectivePolicy = {
      ...NO_POLICY,
      audio_forbidden: true,
      rules: [rule("audio_forbidden", "Contoso")],
    };
    expect(policySource(policy, "audio_forbidden")?.source).toBe("Contoso");
    expect(policySource(policy, "telemetry_off")).toBeNull();
    expect(policySource(NO_POLICY, "audio_forbidden")).toBeNull();
  });
});

describe("MANAGED_SOURCE", () => {
  it("mirrors org_policy::MANAGED_SOURCE", () => {
    expect(policySrc).toContain(`pub(crate) const MANAGED_SOURCE: &str = "${MANAGED_SOURCE}";`);
  });
});
