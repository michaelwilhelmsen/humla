import { StrictMode } from "react";
import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { mockTauri } from "../../../test/tauri";
import { SummaryStep } from "./Summary";
import type { StepContext } from "../types";
import {
  EMBEDDING_OLLAMA_MODEL,
  RECOMMENDED_OLLAMA_MODEL,
} from "../../../lib/localModels";

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

/** A `settings_set` handler that records what the step persisted, so tests can
 *  assert on the settings rather than on which handler was called. */
function captureWrites() {
  const writes: Record<string, string> = {};
  return {
    writes,
    settings_set: (args: unknown) => {
      const { key, value } = args as { key: string; value: string };
      writes[key] = value;
      return null;
    },
  };
}

describe("onboarding SummaryStep — OpenAI path", () => {
  it("save + passing test commits OpenAI as the summary provider", async () => {
    const { writes, settings_set } = captureWrites();
    renderStep({
      provider_key_get: () => null, // no key yet → inline key UI
      settings_set,
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
    // The copyable pull command for the recommended model, plus the 16 GB fallback.
    expect(screen.getByText(/ollama pull gemma4:12b-mlx/i)).toBeInTheDocument();
    expect(screen.getByText(/ollama pull qwen3\.5:4b/i)).toBeInTheDocument();
  });

  it("reachable server: picking a model configures the local summary path", async () => {
    const { writes, settings_set } = captureWrites();
    renderStep({
      local_llm_list_models: () => ["qwen3.5:4b", "llama3.2:3b"],
      settings_set,
    });

    await userEvent.click(
      await screen.findByRole("button", { name: /local \(ollama\)/i }),
    );

    expect(await screen.findByText(/ollama is running/i)).toBeInTheDocument();

    // Gemma (the headline recommendation) isn't installed here, so the picker
    // preselects qwen3.5:4b — the 16 GB fallback — over the other installed model.
    // The picker is the shared Select now (#114) — a listbox, not a native
    // <select>, so its current value reads off the trigger.
    const select = screen.getByRole("combobox", { name: "Model" });
    expect(select).toHaveTextContent("qwen3.5:4b");

    await userEvent.click(select);
    await userEvent.click(screen.getByRole("option", { name: "llama3.2:3b" }));

    expect(writes.summary_provider).toBe("local");
    expect(writes.local_llm_model).toBe("llama3.2:3b");
    expect(writes.local_llm_think).toBe("false");
  });

  // #147 — accepting the auto-preselected model must commit exactly like an
  // explicit pick. Preselection used to only *display* a model: no settings
  // were written, so Continue stayed disabled and the only way forward was to
  // touch the dropdown. Same shape as #9 (a displayed default nobody saved).
  it("commits the auto-preselected recommended model with no dropdown interaction", async () => {
    const { writes, settings_set } = captureWrites();
    renderStep({
      local_llm_list_models: () => [RECOMMENDED_OLLAMA_MODEL],
      settings_set,
    });

    await userEvent.click(
      await screen.findByRole("button", { name: /local \(ollama\)/i }),
    );
    expect(await screen.findByText(/ollama is running/i)).toBeInTheDocument();

    // No interaction with the picker at all — the preselect is the pick.
    expect(await screen.findByText(/^using/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^continue$/i })).toBeEnabled();
    expect(writes.summary_provider).toBe("local");
    expect(writes.local_llm_base_url).toBe("http://localhost:11434/v1");
    expect(writes.local_llm_model).toBe(RECOMMENDED_OLLAMA_MODEL);
    expect(writes.local_llm_think).toBe("false");
    // Chat is seeded off the same choice so it works without a second setup.
    expect(writes.chat_provider).toBe("ollama");
    expect(writes.chat_model).toBe(RECOMMENDED_OLLAMA_MODEL);
  });

  // The other half of #147: auto-commit must not fire when there's nothing
  // legitimate to commit. An embedding-only server has models installed but
  // none that can chat, so it must stay on the pull-command path.
  it("commits nothing and blocks Continue when only an embedding model is installed", async () => {
    const { writes, settings_set } = captureWrites();
    renderStep({
      local_llm_list_models: () => [EMBEDDING_OLLAMA_MODEL],
      settings_set,
    });

    await userEvent.click(
      await screen.findByRole("button", { name: /local \(ollama\)/i }),
    );

    expect(
      await screen.findByText(/none of your installed models can write summaries/i),
    ).toBeInTheDocument();
    expect(screen.getByText(/waiting for the model/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^continue$/i })).toBeDisabled();
    expect(writes.summary_provider).toBeUndefined();
    expect(writes.local_llm_model).toBeUndefined();
  });

  // Acceptance criterion: the "first usable installed model" fallback commits
  // on the same path as the recommended one, and the now-unreachable "pull the
  // recommended model" hint stays visible as an upgrade prompt.
  it("commits the first usable model when neither recommendation is installed", async () => {
    const { writes, settings_set } = captureWrites();
    renderStep({
      local_llm_list_models: () => [EMBEDDING_OLLAMA_MODEL, "llama3.2:3b"],
      settings_set,
    });

    await userEvent.click(
      await screen.findByRole("button", { name: /local \(ollama\)/i }),
    );

    expect(await screen.findByText(/^using/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^continue$/i })).toBeEnabled();
    // The embedding model is never a candidate (#48).
    expect(writes.local_llm_model).toBe("llama3.2:3b");
    // Still nudged toward the recommended model rather than left in silence.
    expect(
      screen.getByText(/recommended model isn't installed/i),
    ).toBeInTheDocument();
  });

  // The probe re-lists every ~2s with a fresh array identity. The commit is
  // keyed on the resolved model name and guarded, so polling must not rewrite
  // the settings over and over.
  it("commits once across repeated probe polls", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      let providerWrites = 0;
      let probes = 0;
      renderStep({
        local_llm_list_models: () => {
          probes++;
          return [RECOMMENDED_OLLAMA_MODEL];
        },
        settings_set: (args) => {
          if ((args as { key: string }).key === "summary_provider") providerWrites++;
          return null;
        },
      });

      await userEvent.click(
        await screen.findByRole("button", { name: /local \(ollama\)/i }),
      );
      expect(await screen.findByText(/^using/i)).toBeInTheDocument();
      expect(providerWrites).toBe(1);

      // Several polls' worth of re-listing.
      const probesBefore = probes;
      await vi.advanceTimersByTimeAsync(7000);
      // Guard against a vacuous assertion: the poll must really have re-listed.
      expect(probes).toBeGreaterThan(probesBefore);
      expect(providerWrites).toBe(1);
    } finally {
      vi.useRealTimers();
    }
  });

  // A transient settings_set failure must not be a dead end. The commit is
  // retried on the next probe re-list; before that, the only escape was the
  // "— pick a model —" placeholder, and with one model installed there was no
  // other value to pick.
  it("retries a failed commit on the next probe poll", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      let attempts = 0;
      renderStep({
        local_llm_list_models: () => [RECOMMENDED_OLLAMA_MODEL],
        settings_set: (args) => {
          const { key } = args as { key: string };
          if (key === "summary_provider") {
            attempts++;
            if (attempts === 1) throw new Error("database is locked");
          }
          return null;
        },
      });

      await userEvent.click(
        await screen.findByRole("button", { name: /local \(ollama\)/i }),
      );
      await screen.findByRole("combobox", { name: "Model" });

      // First attempt failed → not configured, Continue still disabled.
      expect(screen.getByRole("button", { name: /^continue$/i })).toBeDisabled();
      expect(attempts).toBe(1);

      // The next re-list retries it, with no user interaction. Assert the
      // observable outcome first: the retry lands a render after the poll, so
      // reading the counter straight after advancing the clock races it.
      await vi.advanceTimersByTimeAsync(3000);
      expect(await screen.findByText(/^using/i)).toBeInTheDocument();
      expect(screen.getByRole("button", { name: /^continue$/i })).toBeEnabled();
      expect(attempts).toBeGreaterThan(1);
    } finally {
      vi.useRealTimers();
    }
  });

  // The app renders under StrictMode (src/main.tsx), which double-invokes
  // effects on mount in dev. The commit must still land exactly once.
  it("commits once under StrictMode's double-invoked effects", async () => {
    let providerWrites = 0;
    mockTauri({
      local_llm_list_models: () => [RECOMMENDED_OLLAMA_MODEL],
      settings_set: (args) => {
        if ((args as { key: string }).key === "summary_provider") providerWrites++;
        return null;
      },
    });
    render(
      <StrictMode>
        <SummaryStep ctx={ctx()} />
      </StrictMode>,
    );

    await userEvent.click(
      await screen.findByRole("button", { name: /local \(ollama\)/i }),
    );
    expect(await screen.findByText(/^using/i)).toBeInTheDocument();
    expect(providerWrites).toBe(1);
  });
});
