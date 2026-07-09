import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { mockTauri } from "../../../test/tauri";
import { SummaryStep } from "./Summary";
import type { StepContext } from "../types";

// Characterization tests pinned BEFORE the #22 hook extraction: the wizard's
// staged local-LLM flow must survive the refactor byte-for-byte in behavior.

function ctx(): StepContext {
  return {
    stepId: "summary",
    index: 4,
    total: 6,
    goNext: vi.fn(),
    goBack: vi.fn(),
    goTo: vi.fn(),
    canGoBack: true,
    complete: vi.fn(),
  } as unknown as StepContext;
}

function renderStep(handlers: Parameters<typeof mockTauri>[0] = {}) {
  mockTauri(handlers);
  return render(<SummaryStep ctx={ctx()} />);
}

describe("onboarding SummaryStep — OpenAI path", () => {
  it("save + passing test commits OpenAI as the summary provider", async () => {
    const writes: Record<string, string> = {};
    renderStep({
      provider_key_get: () => null, // no key yet → inline key UI
      settings_set: (args) => {
        const { key, value } = args as { key: string; value: string };
        writes[key] = value;
        return null;
      },
      provider_key_test: () => ({ ok: true, status: 200, error: null }),
    });

    await userEvent.click(
      await screen.findByRole("button", { name: /^openai/i }),
    );
    await userEvent.type(
      await screen.findByPlaceholderText("sk-…"),
      "sk-test",
    );
    await userEvent.click(screen.getByRole("button", { name: /save/i }));
    await userEvent.click(screen.getByRole("button", { name: /^test/i }));

    expect(await screen.findByText(/^connected$/i)).toBeInTheDocument();
    expect(writes.summary_provider).toBe("openai");
  });
});

describe("onboarding SummaryStep — local path", () => {
  it("waits for an unreachable Ollama with install guidance", async () => {
    renderStep({
      local_llm_list_models: () => {
        throw new Error("connection refused");
      },
    });

    await userEvent.click(
      await screen.findByRole("button", { name: /local \(ollama\)/i }),
    );

    expect(
      await screen.findByText(/ollama isn't running/i),
    ).toBeInTheDocument();
    expect(screen.getByText(/waiting for ollama/i)).toBeInTheDocument();
    // The copyable pull command for the recommended model.
    expect(screen.getByText(/ollama pull qwen3\.5:4b/i)).toBeInTheDocument();
  });

  it("reachable server: picking a model configures the local summary path", async () => {
    const writes: Record<string, string> = {};
    renderStep({
      local_llm_list_models: () => ["qwen3.5:4b", "llama3.2:3b"],
      settings_set: (args) => {
        const { key, value } = args as { key: string; value: string };
        writes[key] = value;
        return null;
      },
    });

    await userEvent.click(
      await screen.findByRole("button", { name: /local \(ollama\)/i }),
    );

    expect(await screen.findByText(/ollama is running/i)).toBeInTheDocument();

    // The recommended model is preselected in the picker.
    const select = screen.getByLabelText("Model");
    expect(select).toHaveValue("qwen3.5:4b");

    await userEvent.selectOptions(select, "llama3.2:3b");

    expect(writes.summary_provider).toBe("local");
    expect(writes.local_llm_model).toBe("llama3.2:3b");
    expect(writes.local_llm_think).toBe("false");
  });
});
