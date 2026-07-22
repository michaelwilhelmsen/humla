import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { ChatTab } from "./Chat";
import { mockTauri } from "../../../test/tauri";
import { DEFAULTS, type EditableKey } from "../types";

function settings(overrides: Partial<Record<EditableKey, string>> = {}) {
  return { ...DEFAULTS, ...overrides } as Record<EditableKey, string>;
}

beforeEach(() => {
  mockTauri();
});

describe("ChatTab provider setting", () => {
  it("offers exactly OpenAI and Ollama — never Groq/Deepgram", () => {
    render(<ChatTab s={settings({ chat_provider: "openai" })} update={async () => {}} />);
    fireEvent.click(screen.getByRole("button", { name: /Cloud \(OpenAI\)/ }));
    const options = screen.getAllByRole("option").map((o) => o.textContent);
    expect(options).toEqual(["Cloud (OpenAI)", "Local (Ollama)"]);
    expect(screen.queryByRole("option", { name: /Groq/i })).toBeNull();
    expect(screen.queryByRole("option", { name: /Deepgram/i })).toBeNull();
  });

  it("persists the provider choice via update", () => {
    const update = vi.fn();
    render(<ChatTab s={settings({ chat_provider: "openai" })} update={update} />);
    fireEvent.click(screen.getByRole("button", { name: /Cloud \(OpenAI\)/ }));
    fireEvent.click(screen.getByRole("option", { name: "Local (Ollama)" }));
    expect(update).toHaveBeenCalledWith("chat_provider", "ollama");
  });
});

describe("ChatTab readiness", () => {
  it("OpenAI: flags a missing key", async () => {
    mockTauri({ provider_key_get: () => null }); // no key stored
    render(<ChatTab s={settings({ chat_provider: "openai", chat_model: "gpt-5.4" })} update={async () => {}} />);
    expect(screen.getByText("Setup needed")).toBeInTheDocument();
    expect(screen.getByText(/Add your OpenAI key/)).toBeInTheDocument();
  });

  it("OpenAI: ready with a stored key and a chosen model", async () => {
    mockTauri({ provider_key_get: () => "stored" });
    render(<ChatTab s={settings({ chat_provider: "openai", chat_model: "gpt-5.4" })} update={async () => {}} />);
    await waitFor(() => expect(screen.getByText("Ready ✓")).toBeInTheDocument());
  });

  it("Ollama: flags an unreachable server", async () => {
    mockTauri({
      local_llm_list_models: () => {
        throw new Error("connection refused");
      },
    });
    render(<ChatTab s={settings({ chat_provider: "ollama", chat_model: "qwen3.5:4b" })} update={async () => {}} />);
    await waitFor(() => expect(screen.getByText(/Start or install Ollama/)).toBeInTheDocument());
  });

  it("Ollama: flags a model that isn't installed on the server", async () => {
    mockTauri({ local_llm_list_models: () => ["some-other-model"] });
    render(<ChatTab s={settings({ chat_provider: "ollama", chat_model: "qwen3.5:4b" })} update={async () => {}} />);
    await waitFor(() =>
      expect(screen.getByText(/isn't installed on the server/)).toBeInTheDocument(),
    );
  });

  it("Ollama: ready when the server has the chosen model", async () => {
    mockTauri({ local_llm_list_models: () => ["qwen3.5:4b"] });
    render(<ChatTab s={settings({ chat_provider: "ollama", chat_model: "qwen3.5:4b" })} update={async () => {}} />);
    await waitFor(() => expect(screen.getByText("Ready ✓")).toBeInTheDocument());
  });

  it("Ollama: the pull command is copyable", () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.assign(navigator, { clipboard: { writeText } });
    mockTauri({ local_llm_list_models: () => ["some-other-model"] });
    render(<ChatTab s={settings({ chat_provider: "ollama", chat_model: "qwen3.5:4b" })} update={async () => {}} />);
    fireEvent.click(screen.getByRole("button", { name: /Copy Ollama pull command/ }));
    expect(writeText).toHaveBeenCalledWith("ollama pull qwen3.5:4b");
  });

  // Embedding model (issue #48) — a soft, non-blocking recommendation.
  it("Ollama: prompts to pull embeddinggemma when it's missing, without blocking readiness", async () => {
    mockTauri({ local_llm_list_models: () => ["qwen3.5:4b"] }); // chat model present, no embedder
    render(<ChatTab s={settings({ chat_provider: "ollama", chat_model: "qwen3.5:4b" })} update={async () => {}} />);
    // Still Ready — semantic is optional and degrades to keyword-only.
    await waitFor(() => expect(screen.getByText("Ready ✓")).toBeInTheDocument());
    expect(screen.getByText(/For semantic search/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Copy embedding-model pull command/ })).toBeInTheDocument();
  });

  it("Ollama: shows semantic search ready once embeddinggemma is installed", async () => {
    mockTauri({ local_llm_list_models: () => ["qwen3.5:4b", "embeddinggemma:latest"] });
    render(<ChatTab s={settings({ chat_provider: "ollama", chat_model: "qwen3.5:4b" })} update={async () => {}} />);
    await waitFor(() => expect(screen.getByText(/Semantic search ready/)).toBeInTheDocument());
    // Tag-insensitive match: "embeddinggemma:latest" satisfies "embeddinggemma".
    expect(screen.queryByRole("button", { name: /Copy embedding-model pull command/ })).toBeNull();
  });

  // Regression: an embedding model must never be usable as the chat model
  // (Ollama 400s "does not support chat"). Surfaced by real pnpm tauri dev.
  it("Ollama: flags an embedding model wrongly set as the chat model", async () => {
    mockTauri({ local_llm_list_models: () => ["embeddinggemma:latest", "gemma4:12b-mlx"] });
    render(
      <ChatTab
        s={settings({ chat_provider: "ollama", chat_model: "embeddinggemma:latest" })}
        update={async () => {}}
      />,
    );
    await waitFor(() => expect(screen.getByText("Setup needed")).toBeInTheDocument());
    expect(screen.getByText(/is an embedding model/)).toBeInTheDocument();
  });

  it("Ollama: auto-selects a chat model, never the embedding model, when none is set", async () => {
    const update = vi.fn();
    // Embedding model listed FIRST (as Ollama does) — the old installed[0]
    // auto-select would have picked it.
    mockTauri({ local_llm_list_models: () => ["embeddinggemma:latest", "gemma4:12b-mlx"] });
    render(<ChatTab s={settings({ chat_provider: "ollama", chat_model: "" })} update={update} />);
    await waitFor(() => expect(update).toHaveBeenCalledWith("chat_model", "gemma4:12b-mlx"));
    expect(update).not.toHaveBeenCalledWith("chat_model", "embeddinggemma:latest");
  });
});
