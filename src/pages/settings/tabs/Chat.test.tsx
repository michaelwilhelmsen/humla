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

// Issue #122: the rebuild action came out from behind developer mode, which is only
// safe because it stays QUIET when there is nothing to repair. A permanently visible
// "rebuild" button trains people to ignore it or to press it pointlessly — and
// pressing it pointlessly spends their embedding key.
describe("ChatTab rebuild-index row", () => {
  it("is visible without developer mode, and silent on a current library", async () => {
    mockTauri({ chat_stale_note_count: () => 0 });
    render(<ChatTab s={settings()} update={async () => {}} />);

    // Present at all — the #122 fix is that this no longer requires developer mode.
    expect(await screen.findByText(/Up to date/)).toBeTruthy();
    // …and offers no action, because there is nothing to do.
    expect(screen.queryByRole("button", { name: /Rebuild/i })).toBeNull();
  });

  it("says how many notes are stale and offers the fix when some are", async () => {
    mockTauri({ chat_stale_note_count: () => 7 });
    render(<ChatTab s={settings()} update={async () => {}} />);

    // The count is the point: a bare button gives no way to judge whether the slow,
    // key-spending action is worth taking.
    expect(await screen.findByText(/7 notes indexed before the latest improvements/)).toBeTruthy();
    expect(screen.getByRole("button", { name: /Rebuild now/i })).toBeTruthy();
    // The cost is disclosed rather than buried.
    expect(screen.getByText(/uses your configured key/)).toBeTruthy();
    // And it says what it does NOT touch, since "rebuild" sounds destructive.
    expect(screen.getByText(/your notes aren't changed/)).toBeTruthy();
  });

  it("reports how many notes it rebuilt", async () => {
    mockTauri({ chat_stale_note_count: () => 3, chat_rebuild_index: () => 3 });
    render(<ChatTab s={settings()} update={async () => {}} />);
    fireEvent.click(await screen.findByRole("button", { name: /Rebuild now/i }));
    await waitFor(() => expect(screen.getByText(/Rebuilt 3 notes/)).toBeTruthy());
  });

  it("stays quiet rather than guessing when the count can't be read", async () => {
    // A failed count must not render "0 notes are stale" (a claim) or a rebuild
    // prompt (an action on unknown state).
    mockTauri({
      chat_stale_note_count: () => {
        throw new Error("nope");
      },
    });
    render(<ChatTab s={settings()} update={async () => {}} />);
    await waitFor(() => expect(screen.getByText(/Up to date/)).toBeTruthy());
    expect(screen.queryByRole("button", { name: /Rebuild/i })).toBeNull();
  });
});
