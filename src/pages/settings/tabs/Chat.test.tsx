import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
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
  it("offers exactly OpenAI and Ollama — never Groq/Deepgram", async () => {
    render(<ChatTab s={settings({ chat_provider: "openai" })} update={async () => {}} />);
    await userEvent.click(screen.getByRole("combobox", { name: /Cloud \(OpenAI\)/ }));
    const options = screen.getAllByRole("option").map((o) => o.textContent);
    expect(options).toEqual(["Cloud (OpenAI)", "Local (any OpenAI-compatible server)"]);
    expect(screen.queryByRole("option", { name: /Groq/i })).toBeNull();
    expect(screen.queryByRole("option", { name: /Deepgram/i })).toBeNull();
  });

  it("persists the provider choice via update", async () => {
    const update = vi.fn();
    render(<ChatTab s={settings({ chat_provider: "openai" })} update={update} />);
    await userEvent.click(screen.getByRole("combobox", { name: /Cloud \(OpenAI\)/ }));
    await userEvent.click(screen.getByRole("option", { name: "Local (any OpenAI-compatible server)" }));
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

  // Embedding model (#48, #179) — a soft, non-blocking recommendation, and a
  // real probe rather than a guess off the chat server's model listing: a
  // server can list a model and serve no /v1/embeddings route at all.
  it("Ollama: a failing embedder says so and offers the pull, without blocking readiness", async () => {
    mockTauri({ local_llm_list_models: () => ["qwen3.5:4b"] });
    render(<ChatTab s={settings({ chat_provider: "ollama", chat_model: "qwen3.5:4b" })} update={async () => {}} />);
    // Still Ready — semantic is optional and degrades to keyword-only.
    await waitFor(() => expect(screen.getByText("Ready ✓")).toBeInTheDocument());
    await waitFor(() => expect(screen.getByText(/keyword only/)).toBeInTheDocument());
    expect(screen.getByRole("button", { name: /Copy embedding-model pull command/ })).toBeInTheDocument();
  });

  it("Ollama: shows semantic search ready once the embedder answers", async () => {
    mockTauri({
      local_llm_list_models: () => ["qwen3.5:4b"],
      local_llm_embed_probe: () => 768,
    });
    render(<ChatTab s={settings({ chat_provider: "ollama", chat_model: "qwen3.5:4b" })} update={async () => {}} />);
    await waitFor(() => expect(screen.getByText(/Semantic search ready/)).toBeInTheDocument());
    expect(screen.getByText(/768 dimensions/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Copy embedding-model pull command/ })).toBeNull();
  });

  // The shape #179 exists for: the chat server can't embed, so the embedder is
  // pointed at a second one. The probe must follow the override, not the chat URL.
  it("the embedder is probed at its own URL and model", async () => {
    const probed: Array<{ baseUrl: string; model: string }> = [];
    mockTauri({
      local_llm_list_models: () => ["mlx-community/Qwen3-8B"],
      local_llm_embed_probe: (args) => {
        probed.push(args as { baseUrl: string; model: string });
        return 768;
      },
    });
    render(
      <ChatTab
        s={settings({
          chat_provider: "ollama",
          chat_model: "mlx-community/Qwen3-8B",
          local_llm_base_url: "http://127.0.0.1:8000/v1",
          embed_base_url: "http://localhost:11434/v1",
          embed_model: "embeddinggemma:latest",
        })}
        update={async () => {}}
      />,
    );
    await waitFor(() => expect(probed.length).toBeGreaterThan(0));
    expect(probed[0]).toEqual({
      baseUrl: "http://localhost:11434/v1",
      model: "embeddinggemma:latest",
    });
  });

  it("blank embedder fields fall back to the chat server and embeddinggemma", async () => {
    const probed: Array<{ baseUrl: string; model: string }> = [];
    mockTauri({
      local_llm_list_models: () => ["mlx-community/Qwen3-8B"],
      local_llm_embed_probe: (args) => {
        probed.push(args as { baseUrl: string; model: string });
        return 768;
      },
    });
    render(
      <ChatTab
        s={settings({
          chat_provider: "ollama",
          chat_model: "mlx-community/Qwen3-8B",
          local_llm_base_url: "http://127.0.0.1:8000/v1",
        })}
        update={async () => {}}
      />,
    );
    await waitFor(() => expect(probed.length).toBeGreaterThan(0));
    expect(probed[0]).toEqual({
      baseUrl: "http://127.0.0.1:8000/v1",
      model: "embeddinggemma",
    });
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

  // #179: chat works against any OpenAI-compatible server, so the advice must
  // stop naming Ollama once the URL is off its port — `ollama pull` is not a
  // command an mlx / LM Studio / vLLM user has.
  it("a non-Ollama local server gets no ollama pull command", async () => {
    mockTauri({ local_llm_list_models: () => ["mlx-community/Qwen3-8B"] });
    render(
      <ChatTab
        s={settings({
          chat_provider: "ollama",
          chat_model: "mlx-community/Qwen3-8B",
          local_llm_base_url: "http://127.0.0.1:8000/v1",
        })}
        update={async () => {}}
      />,
    );
    await waitFor(() => expect(screen.getByText("Ready ✓")).toBeInTheDocument());
    expect(screen.queryByRole("button", { name: /Copy Ollama pull command/ })).toBeNull();
    // The embedder failed (no server in tests) and its URL is not Ollama's, so
    // the pull command is withheld — it is not a command this user has.
    await waitFor(() => expect(screen.getByText(/keyword only/)).toBeInTheDocument());
    expect(screen.queryByRole("button", { name: /Copy embedding-model pull command/ })).toBeNull();
  });

  it("a non-Ollama local server that is down names the URL, not Ollama", async () => {
    mockTauri({
      local_llm_list_models: () => {
        throw new Error("connection refused");
      },
    });
    render(
      <ChatTab
        s={settings({
          chat_provider: "ollama",
          chat_model: "mlx-community/Qwen3-8B",
          local_llm_base_url: "http://127.0.0.1:8000/v1",
        })}
        update={async () => {}}
      />,
    );
    await waitFor(() =>
      expect(screen.getByText(/Couldn't reach the local server at http:\/\/127\.0\.0\.1:8000\/v1/)).toBeInTheDocument(),
    );
    expect(screen.queryByText(/install Ollama/)).toBeNull();
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
    expect(await screen.findByText(/7 recordings were indexed before the latest improvements/)).toBeTruthy();
    expect(screen.getByRole("button", { name: /Rebuild now/i })).toBeTruthy();
    // The cost is disclosed rather than buried.
    expect(screen.getByText(/uses your configured key/)).toBeTruthy();
    // And it says what it does NOT touch, since "rebuild" sounds destructive.
    expect(screen.getByText(/your notes aren't changed/)).toBeTruthy();
  });

  it("reports how many notes it rebuilt", async () => {
    // The rebuild walks the WHOLE library, so its number is larger than the stale
    // count it was offered for. The copy must not pair them as though they should match.
    mockTauri({ chat_stale_note_count: () => 3, chat_rebuild_index: () => 41 });
    render(<ChatTab s={settings()} update={async () => {}} />);
    fireEvent.click(await screen.findByRole("button", { name: /Rebuild now/i }));
    await waitFor(() => expect(screen.getByText(/Index rebuilt across 41 notes/)).toBeTruthy());
  });

  it("says it couldn't check rather than claiming either state", async () => {
    // This test previously asserted "Up to date ✓" on a failed count — which is a
    // STRONGER unverified claim than the "0 notes are stale" it was trying to avoid.
    // Not knowing and knowing-it's-fine are different facts, and the row now says which.
    mockTauri({
      chat_stale_note_count: () => {
        throw new Error("nope");
      },
    });
    render(<ChatTab s={settings()} update={async () => {}} />);
    await waitFor(() => expect(screen.getByText(/Couldn't check the index/)).toBeTruthy());
    expect(screen.queryByText(/Up to date/)).toBeNull();
    // And no action is offered on unknown state.
    expect(screen.queryByRole("button", { name: /Rebuild/i })).toBeNull();
  });

  it("never calls a typed-notes-only library stale", async () => {
    // #104 moved transcript boundaries only, so a note with no recording can't be
    // stale on account of it. Asserted here as well as in db.rs because this row is
    // where a user would see the wrong claim.
    mockTauri({ chat_stale_note_count: () => 0 });
    render(<ChatTab s={settings()} update={async () => {}} />);
    expect(await screen.findByText(/Up to date/)).toBeTruthy();
    expect(screen.queryByText(/indexed before the latest improvements/)).toBeNull();
  });
});
