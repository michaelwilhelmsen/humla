import src from "../../src-tauri/src/providers.rs?raw";
import { describe, expect, it } from "vitest";
import { KEY_PROVIDERS, PROVIDERS, TRANSCRIBE_KEY_PROVIDERS, isKeyProvider } from "./providers";

// Pull each REGISTRY row out of the Rust source.
function rustRegistry() {
  const body = src.slice(src.indexOf("pub const REGISTRY"), src.indexOf("];", src.indexOf("pub const REGISTRY")));
  const rows = [...body.matchAll(/ProviderSpec \{([\s\S]*?)\n {4}\},/g)].map((m) => m[1]);
  return rows.map((row) => ({
    id: /id_str: "([a-z0-9-]+)"/.exec(row)![1],
    label: /label: "([^"]+)"/.exec(row)![1],
    keychain: /keychain_account: Some\(/.test(row),
    summarize: /summarize: true/.test(row),
    chat: /chat: true/.test(row),
    transcribe: /transcribe: true/.test(row),
  }));
}

describe("providers.ts mirrors providers.rs", () => {
  it("has the same rows in the same order with the same flags", () => {
    const rust = rustRegistry();
    expect(rust.length).toBeGreaterThan(0);
    expect(PROVIDERS.map((p) => ({ ...p }))).toEqual(rust);
  });

  it("derives the key providers from the keychain flag", () => {
    expect(KEY_PROVIDERS).toEqual(["openai", "anthropic", "deepgram", "groq", "custom"]);
    expect(isKeyProvider("local")).toBe(false);
    expect(isKeyProvider("groq")).toBe(true);
  });

  // A custom endpoint needs its URL as well as (optionally) a key, so a key
  // alone never makes it a transcription choice.
  it("keeps custom out of the key-only transcription providers", () => {
    expect(TRANSCRIBE_KEY_PROVIDERS).toEqual(["openai", "deepgram", "groq"]);
  });
});
