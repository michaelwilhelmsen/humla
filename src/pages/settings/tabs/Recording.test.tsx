import { describe, expect, it } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import { RecordingSection } from "./Recording";
import { AboutTab } from "./About";
import { MemoryRouter } from "react-router-dom";
import { mockTauri } from "../../../test/tauri";
import { NO_POLICY, type EffectivePolicy } from "../../../lib/orgPolicy";
import { DEFAULTS, type EditableKey } from "../types";

function settings(overrides: Partial<Record<EditableKey, string>> = {}) {
  return { ...DEFAULTS, ...overrides } as Record<EditableKey, string>;
}

const enforcing: EffectivePolicy = {
  ...NO_POLICY,
  org_name: "Northwind",
  audio_forbidden: true,
  telemetry_off: true,
  rules: [
    { kind: "audio_forbidden", source: "Northwind", reason: "" },
    { kind: "telemetry_off", source: "Northwind", reason: "" },
  ],
};

describe("Keep recorded audio", () => {
  it("is the user's switch without an organisation", async () => {
    mockTauri();
    render(<RecordingSection s={settings({ keep_audio: "true" })} update={async () => {}} />);
    const toggle = screen.getByRole("switch", { name: "Keep recorded audio" });
    expect(toggle).toHaveAttribute("aria-checked", "true");
    expect(toggle).not.toBeDisabled();
    expect(screen.getByRole("switch", { name: "Transcribe manually" })).toBeTruthy();
    expect(screen.queryByText(/Managed by/)).toBeNull();
  });

  it("is managed and off under audio_forbidden, and hides Transcribe manually", async () => {
    mockTauri({ org_status: () => ({ policy: enforcing }) });
    render(<RecordingSection s={settings({ keep_audio: "true" })} update={async () => {}} />);
    expect(await screen.findByRole("button", { name: /Managed by Northwind/ })).toBeTruthy();
    const toggle = screen.getByRole("switch", { name: "Keep recorded audio" });
    expect(toggle).toHaveAttribute("aria-checked", "false");
    expect(toggle).toBeDisabled();
    expect(screen.queryByRole("switch", { name: "Transcribe manually" })).toBeNull();
  });
});

describe("Send anonymous usage counters", () => {
  it("is the user's switch without an organisation", async () => {
    mockTauri();
    render(<MemoryRouter><AboutTab /></MemoryRouter>);
    const toggle = await screen.findByRole("switch", { name: "Send anonymous usage counters" });
    expect(toggle).not.toBeDisabled();
    expect(screen.queryByText(/Managed by/)).toBeNull();
  });

  it("is managed and off under telemetry_off", async () => {
    mockTauri({
      org_status: () => ({ policy: NO_POLICY, device: enforcing }),
      settings_get: (args) => ((args as { key: string }).key === "telemetry_enabled" ? "true" : null),
    });
    render(<MemoryRouter><AboutTab /></MemoryRouter>);
    expect(await screen.findByRole("button", { name: /Managed by Northwind/ })).toBeTruthy();
    await waitFor(() =>
      expect(screen.getByRole("switch", { name: "Send anonymous usage counters" })).toHaveAttribute("aria-checked", "false"),
    );
    expect(screen.getByRole("switch", { name: "Send anonymous usage counters" })).toBeDisabled();
  });
});
