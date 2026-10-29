import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { ProviderConfigForm } from "./ProviderConfigForm";
import { mockTauri } from "../../../test/tauri";

beforeEach(() => {
  mockTauri();
});

describe("ProviderConfigForm on a custom endpoint", () => {
  it("offers Custom, which starts with no model rather than a guessed one", async () => {
    const onChange = vi.fn();
    render(
      <ProviderConfigForm value={{ provider: "openai", model: "whisper-1" }} onChange={onChange} localModels={[]} />,
    );
    await userEvent.click(screen.getByRole("combobox", { name: "OpenAI" }));
    await userEvent.click(screen.getByRole("option", { name: "Custom (OpenAI-compatible API)" }));
    expect(onChange).toHaveBeenCalledWith({ provider: "custom", model: "" });
  });

  it("takes any typed model", async () => {
    const onChange = vi.fn();
    render(
      <ProviderConfigForm
        value={{ provider: "custom", model: "elevenlabs/scribe-v2" }}
        onChange={onChange}
        localModels={[]}
      />,
    );
    await userEvent.click(screen.getByRole("button", { name: "elevenlabs/scribe-v2" }));
    const input = screen.getByRole("combobox", { name: "Custom endpoint model" });
    await userEvent.clear(input);
    await userEvent.type(input, "openai/whisper-1{Enter}");
    expect(onChange).toHaveBeenCalledWith({ provider: "custom", model: "openai/whisper-1" });
  });
});
