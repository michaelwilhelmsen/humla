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

  it("offers the live OpenAI listing when there is one", async () => {
    mockTauri({ provider_key_get: () => "stored", openai_list_models: () => ["gpt-9-turbo"] });
    render(<SummaryTab s={settings()} update={async () => {}} />);
    await userEvent.click(await screen.findByRole("combobox", { name: "gpt-5.4-mini" }));
    // The stored value rides along, so a model the listing omits isn't lost.
    expect(screen.getAllByRole("option").map((o) => o.textContent)).toEqual([
      "gpt-5.4-mini",
      "gpt-9-turbo",
    ]);
  });

  it("falls back to the shipped list when the listing fails", async () => {
    mockTauri({
      provider_key_get: () => "stored",
      openai_list_models: () => {
        throw new Error("network");
      },
    });
    render(<SummaryTab s={settings()} update={async () => {}} />);
    await userEvent.click(await screen.findByRole("combobox", { name: "gpt-5.4-mini" }));
    expect(screen.getAllByRole("option").map((o) => o.textContent)).toContain("gpt-5.4");
  });

  it("doesn't list OpenAI models while Anthropic is selected", async () => {
    const listed = vi.fn(() => []);
    mockTauri({ provider_key_get: () => "stored", openai_list_models: listed });
    render(<SummaryTab s={settings({ summary_provider: "anthropic" })} update={async () => {}} />);
    await screen.findByLabelText("Anthropic API key");
    expect(listed).not.toHaveBeenCalled();
  });

  it("Anthropic shows its key card and model row", async () => {
    render(<SummaryTab s={settings({ summary_provider: "anthropic" })} update={async () => {}} />);
    expect(await screen.findByLabelText("Anthropic API key")).toBeInTheDocument();
    expect(screen.getByRole("combobox", { name: "claude-sonnet-5" })).toBeInTheDocument();
  });
});
