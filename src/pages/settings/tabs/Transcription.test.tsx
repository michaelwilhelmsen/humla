import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { TranscriptionTab } from "./Transcription";
import { mockTauri } from "../../../test/tauri";
import type { ProviderConfig } from "../../../lib/ipc";
import { DEFAULTS, EMPTY_DIARIZE_STATE, EMPTY_LOCAL_STATE, type EditableKey } from "../types";

beforeEach(() => {
  mockTauri();
});

function renderTab(def: ProviderConfig, overrides: Partial<Record<EditableKey, string>> = {}) {
  const update = vi.fn();
  render(
    <TranscriptionTab
      s={{ ...DEFAULTS, ...overrides } as Record<EditableKey, string>}
      update={update}
      transcribeConfig={{ default: def, per_language: {} }}
      setDefaultConfig={vi.fn()}
      setLanguageOverride={vi.fn()}
      removeLanguageOverride={vi.fn()}
      local={EMPTY_LOCAL_STATE}
      downloadModel={vi.fn()}
      deleteModel={vi.fn()}
      diarize={{ nemotron3: EMPTY_DIARIZE_STATE, community1: EMPTY_DIARIZE_STATE }}
      downloadDiarize={vi.fn()}
      deleteDiarize={vi.fn()}
    />,
  );
  return update;
}

describe("TranscriptionTab on a custom endpoint", () => {
  it("shows the shared endpoint in place of a key card", async () => {
    const update = renderTab(
      { provider: "custom", model: "elevenlabs/scribe-v2" },
      { custom_base_url: "https://openrouter.ai/api/v1" },
    );
    expect(screen.getByLabelText("Custom endpoint base URL")).toHaveValue("https://openrouter.ai/api/v1");
    expect(screen.queryByLabelText("Custom API key")).toBeNull();
    await userEvent.type(screen.getByLabelText("Custom endpoint base URL"), "/");
    expect(update).toHaveBeenCalledWith("custom_base_url", "https://openrouter.ai/api/v1/");
  });

  it("keeps the endpoint reachable for overrides when the default is another provider", async () => {
    renderTab({ provider: "openai", model: "whisper-1" });
    await userEvent.click(screen.getAllByRole("button", { name: /Advanced/ })[0]);
    expect(screen.getAllByLabelText("Custom endpoint base URL")).toHaveLength(1);
    expect(screen.queryByLabelText("Custom API key")).toBeNull();
  });
});
