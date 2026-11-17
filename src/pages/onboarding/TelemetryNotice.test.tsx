import { describe, expect, it } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import { mockTauri } from "../../test/tauri";
import { NO_POLICY, type EffectivePolicy } from "../../lib/orgPolicy";
import { TelemetryNotice } from "./TelemetryNotice";

const telemetryOff: EffectivePolicy = {
  ...NO_POLICY,
  org_name: "Northwind",
  telemetry_off: true,
  rules: [{ kind: "telemetry_off", source: "Northwind", reason: "" }],
};

function backend(stored: string | null, orgStatus: () => unknown) {
  const writes: Record<string, string> = {};
  const events: string[] = [];
  mockTauri({
    org_status: orgStatus,
    settings_get: (args) => ((args as { key: string }).key === "telemetry_enabled" ? stored : null),
    settings_set: (args) => {
      const { key, value } = args as { key: string; value: string };
      writes[key] = value;
      return null;
    },
    telemetry_event: (args) => {
      events.push((args as { event: string }).event);
      return null;
    },
  });
  return { writes, events };
}

describe("TelemetryNotice", () => {
  it("opts a first run in when no organisation turns the counters off", async () => {
    const { writes, events } = backend(null, () => ({ policy: NO_POLICY }));
    render(<TelemetryNotice />);
    expect(await screen.findByText(/counters are on/)).toBeTruthy();
    await waitFor(() => expect(events).toEqual(["app_first_run"]));
    expect(writes).toEqual({ telemetry_enabled: "true" });
  });

  it("never opts in under telemetry_off and offers no switch", async () => {
    const { writes, events } = backend(null, () => ({ policy: telemetryOff }));
    render(<TelemetryNotice />);
    expect(await screen.findByText(/counters are off/)).toBeTruthy();
    expect(screen.getByRole("button", { name: /Managed by Northwind/ })).toBeTruthy();
    expect(screen.queryByRole("button", { name: /Turn on/ })).toBeNull();
    expect(writes).toEqual({});
    expect(events).toEqual([]);
  });

  it("never opts in when the policy can't be read", async () => {
    const { writes, events } = backend(null, () => {
      throw new Error("organisation policy: unreadable");
    });
    render(<TelemetryNotice />);
    expect(await screen.findByText(/counters are off/)).toBeTruthy();
    expect(writes).toEqual({});
    expect(events).toEqual([]);
  });
});
