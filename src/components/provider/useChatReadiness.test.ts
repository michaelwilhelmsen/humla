import { describe, it, expect, vi } from "vitest";
import { renderHook, waitFor } from "@testing-library/react";
import { localChatHint, useChatReadiness } from "./useChatReadiness";
import { mockTauri } from "../../test/tauri";

// The local chat provider is not necessarily Ollama (#179), so the advice has
// to name the server the user actually runs. One ladder, two readers: the
// Settings tab has the controls in front of it, the Note pane does not.
const OLLAMA = "http://localhost:11434/v1";
const COMPAT = "http://127.0.0.1:8000/v1";

describe("localChatHint", () => {
  it("is empty — meaning ready — once the server has the chosen model", () => {
    expect(
      localChatHint({
        reachable: true,
        installed: ["qwen3.5:4b"],
        model: "qwen3.5:4b",
        baseUrl: OLLAMA,
        where: "above",
      }),
    ).toBe("");
  });

  it("names Ollama for an unreachable Ollama, and the URL for anything else", () => {
    const down = (baseUrl: string) =>
      localChatHint({ reachable: false, installed: null, model: "m", baseUrl, where: "above" });
    expect(down(OLLAMA)).toMatch(/install Ollama/);
    expect(down(COMPAT)).toContain(COMPAT);
    expect(down(COMPAT)).not.toMatch(/Ollama/);
  });

  it("offers ollama pull only on Ollama's own port", () => {
    const missing = (baseUrl: string) =>
      localChatHint({
        reachable: true,
        installed: ["something-else"],
        model: "qwen3.5:4b",
        baseUrl,
        where: "above",
      });
    expect(missing(OLLAMA)).toMatch(/ollama pull qwen3\.5:4b/);
    expect(missing(COMPAT)).not.toMatch(/ollama pull/);
    expect(missing(COMPAT)).toMatch(/isn't one of the models the local server lists/);
  });

  it("points the reader at the controls they can actually see", () => {
    const noModel = (where: "above" | "settings") =>
      localChatHint({ reachable: true, installed: [], model: "", baseUrl: OLLAMA, where });
    expect(noModel("above")).toBe("Choose a chat model above.");
    expect(noModel("settings")).toBe("Choose a chat model in Settings → Chat.");
  });

  it("says it is still checking rather than claiming ready on the first probe", () => {
    expect(
      localChatHint({
        reachable: null,
        installed: null,
        model: "qwen3.5:4b",
        baseUrl: OLLAMA,
        where: "settings",
      }),
    ).toMatch(/Checking/);
  });

  it("rejects an embedding model set as the chat model", () => {
    expect(
      localChatHint({
        reachable: true,
        installed: ["embeddinggemma"],
        model: "embeddinggemma",
        baseUrl: OLLAMA,
        where: "above",
      }),
    ).toMatch(/is an embedding model/);
  });
});

describe("useChatReadiness on GitHub Copilot", () => {
  const settingsGet = (provider: string) => (args: unknown) =>
    (args as { key: string }).key === "chat_provider" ? provider : null;

  it("is ready once Copilot is signed in and the CLI is present", async () => {
    mockTauri({
      settings_get: settingsGet("copilot"),
      copilot_status: () => ({ login: "octocat", cliPath: "/bin/copilot", cliVersion: "1", cliError: null, downloadVersion: "1" }),
    });
    const { result } = renderHook(() => useChatReadiness());
    await waitFor(() => expect(result.current.ready).toBe(true));
  });

  // `chat_model` is shared by OpenAI, Anthropic and local; Copilot answers on
  // its own setting, so a model left there by another provider isn't it.
  it("names Copilot's own chat model, not the shared one", async () => {
    const settings: Record<string, string> = {
      chat_provider: "copilot",
      chat_model: "gpt-5.6-luna",
      copilot_chat_model: "claude-sonnet-5.5",
    };
    mockTauri({
      settings_get: (args) => settings[(args as { key: string }).key] ?? null,
      copilot_status: () => ({ login: "octocat", cliPath: "/bin/copilot", cliVersion: "1", cliError: null, downloadVersion: "1" }),
    });
    const { result } = renderHook(() => useChatReadiness());
    await waitFor(() => expect(result.current.model).toBe("claude-sonnet-5.5"));
  });

  it("names auto when Copilot has no chat model chosen", async () => {
    mockTauri({
      settings_get: (args) =>
        ({ chat_provider: "copilot", chat_model: "gpt-5.6-luna" })[(args as { key: string }).key] ?? null,
      copilot_status: () => ({ login: "octocat", cliPath: "/bin/copilot", cliVersion: "1", cliError: null, downloadVersion: "1" }),
    });
    const { result } = renderHook(() => useChatReadiness());
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.model).toBe("auto");
  });

  it("names the missing CLI", async () => {
    mockTauri({
      settings_get: settingsGet("copilot"),
      copilot_status: () => ({ login: "octocat", cliPath: null, cliVersion: null, cliError: null, downloadVersion: "1" }),
    });
    const { result } = renderHook(() => useChatReadiness());
    await waitFor(() =>
      expect(result.current.hint).toBe("Download the Copilot CLI in Settings → Chat to use GitHub Copilot."),
    );
  });

  it("never asks for Copilot's status on another provider", async () => {
    const status = vi.fn(() => null);
    mockTauri({ settings_get: settingsGet("openai"), copilot_status: status, provider_key_get: () => "stored" });
    const { result } = renderHook(() => useChatReadiness());
    await waitFor(() => expect(result.current.ready).toBe(true));
    expect(status).not.toHaveBeenCalled();
  });
});

describe("useChatReadiness on a custom endpoint", () => {
  it("is ready with a base URL and its own chat model, without any key", async () => {
    const keyReads: unknown[] = [];
    mockTauri({
      settings_get: (args) =>
        ({
          chat_provider: "custom",
          chat_model: "gpt-5.5",
          custom_base_url: "https://gw.example/v1",
          custom_chat_model: "deepseek-chat",
        })[(args as { key: string }).key] ?? null,
      provider_key_get: (args) => {
        keyReads.push((args as { provider: string }).provider);
        return null;
      },
    });
    const { result } = renderHook(() => useChatReadiness());
    await waitFor(() => expect(result.current.ready).toBe(true));
    expect(result.current.model).toBe("deepseek-chat");
    // The custom key is optional and never read here.
    expect(keyReads).not.toContain("custom");
  });

  it("asks for the base URL first", async () => {
    mockTauri({
      settings_get: (args) => ((args as { key: string }).key === "chat_provider" ? "custom" : null),
    });
    const { result } = renderHook(() => useChatReadiness());
    await waitFor(() =>
      expect(result.current.hint).toBe("Add the custom endpoint's base URL in Settings → Chat."),
    );
  });
});
