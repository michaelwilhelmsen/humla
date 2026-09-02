import { describe, it, expect } from "vitest";
import { localChatHint } from "./useChatReadiness";

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
