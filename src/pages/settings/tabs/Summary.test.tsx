import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { SummaryTab } from "./Summary";
import { mockTauri } from "../../../test/tauri";
import { DEFAULTS, type EditableKey } from "../types";

function settings(overrides: Partial<Record<EditableKey, string>> = {}) {
  return { ...DEFAULTS, ...overrides } as Record<EditableKey, string>;
}

beforeEach(() => {
  mockTauri();
});

describe("SummaryTab provider setting", () => {
  it("offers every summary-capable provider", async () => {
    render(<SummaryTab s={settings()} update={async () => {}} />);
    await userEvent.click(screen.getByRole("combobox", { name: /Cloud \(OpenAI\)/ }));
    expect(screen.getAllByRole("option").map((o) => o.textContent)).toEqual([
      "Cloud (OpenAI)",
      "Cloud (Anthropic)",
      "Local (any OpenAI-compatible server)",
    ]);
  });

  // Opening Settings must not open a Keychain slot the chosen provider never
  // uses — on a fresh machine that read is a macOS prompt.
  it("touches nothing Anthropic while OpenAI is selected", async () => {
    const keyReads: unknown[] = [];
    const listed = vi.fn(() => []);
    mockTauri({
      provider_key_get: (args) => {
        keyReads.push((args as { provider: string }).provider);
        return null;
      },
      anthropic_list_models: listed,
    });
    render(<SummaryTab s={settings({ summary_provider: "openai" })} update={async () => {}} />);
    await screen.findByLabelText("OpenAI API key");
    expect(keyReads).not.toContain("anthropic");
    expect(listed).not.toHaveBeenCalled();
  });

  it("Anthropic shows its key card and model row", async () => {
    render(<SummaryTab s={settings({ summary_provider: "anthropic" })} update={async () => {}} />);
    expect(await screen.findByLabelText("Anthropic API key")).toBeInTheDocument();
    expect(screen.getByRole("combobox", { name: "claude-sonnet-5" })).toBeInTheDocument();
  });
});
