import { afterEach, describe, expect, it } from "vitest";
import { act, render, screen, waitFor } from "@testing-library/react";
import { SummaryTab } from "./tabs/Summary";
import { RecordingSection } from "./tabs/Recording";
import { mockTauri } from "../../test/tauri";
import { useCloudStore } from "../../lib/cloud";
import { NO_POLICY, type EffectivePolicy } from "../../lib/orgPolicy";
import { DEFAULTS, type EditableKey } from "./types";

const NORTHWIND: EffectivePolicy = {
  ...NO_POLICY,
  org_name: "Northwind",
  local_only: true,
  audio_forbidden: true,
  rules: [
    { kind: "local_only", source: "Northwind", reason: "" },
    { kind: "audio_forbidden", source: "Northwind", reason: "" },
  ],
};

const settings = (overrides: Partial<Record<EditableKey, string>> = {}) =>
  ({ ...DEFAULTS, ...overrides }) as Record<EditableKey, string>;

const initial = useCloudStore.getState().status;
afterEach(() => useCloudStore.setState({ status: initial }));

function switchTo(ws: { id: string; name: string } | null) {
  act(() => {
    useCloudStore.setState((s) => ({
      status: {
        ...s.status,
        current_workspace: ws ? { ...ws, role: "member", plan_status: "active" } : null,
        workspaces: ws ? [{ ...ws, role: "member", plan_status: "active" }] : [],
      } as typeof s.status,
    }));
  });
}

function backend() {
  const asked: (string | null)[] = [];
  mockTauri({
    org_status: (args) => {
      const ws = (args as { workspaceId: string | null }).workspaceId;
      asked.push(ws);
      return {
        policy: ws === "w1" ? NORTHWIND : NO_POLICY,
        orgs: [{ id: "o1", name: "Northwind", reason: "", vocabulary: [], workspaces: ["w1"] }],
      };
    },
  });
  return asked;
}

describe("Settings follow the active workspace", () => {
  it("names the workspace and shows managed rows only in a governed department", async () => {
    const asked = backend();
    switchTo({ id: "w1", name: "Case work" });
    render(<SummaryTab s={settings({ summary_provider: "openai" })} update={async () => {}} />);
    expect(await screen.findByRole("button", { name: /Managed by Northwind/ })).toBeInTheDocument();
    expect(screen.getByText("Case work")).toBeInTheDocument();
    expect(asked).toContain("w1");

    switchTo(null);
    await waitFor(() => expect(screen.queryByRole("button", { name: /Managed by/ })).toBeNull());
    expect(screen.getByText("Personal")).toBeInTheDocument();
    expect(screen.getByRole("combobox", { name: /OpenAI/ })).not.toBeDisabled();
  });

  it("names no workspace for someone in no organisation", async () => {
    mockTauri({ org_status: () => ({ policy: NO_POLICY, orgs: [] }) });
    switchTo({ id: "w2", name: "Screenpartner" });
    render(<SummaryTab s={settings({ summary_provider: "openai" })} update={async () => {}} />);
    await screen.findByRole("combobox", { name: /OpenAI/ });
    await new Promise((r) => setTimeout(r, 0));
    expect(screen.queryByText("Screenpartner")).toBeNull();
    expect(screen.getByRole("heading", { name: "Summaries" })).toHaveTextContent(/^Summaries$/);
  });

  it("keeps recorded audio managed only in the governed department", async () => {
    backend();
    switchTo({ id: "w2", name: "Screenpartner" });
    render(<RecordingSection s={settings({ keep_audio: "true" })} update={async () => {}} />);
    expect(await screen.findByText("Screenpartner")).toBeInTheDocument();
    expect(screen.getByRole("switch", { name: "Keep recorded audio" })).toHaveAttribute("aria-checked", "true");

    switchTo({ id: "w1", name: "Case work" });
    expect(await screen.findByRole("button", { name: /Managed by Northwind/ })).toBeInTheDocument();
    expect(screen.getByRole("switch", { name: "Keep recorded audio" })).toHaveAttribute("aria-checked", "false");
  });
});
