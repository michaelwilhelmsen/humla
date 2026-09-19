import src from "../../src-tauri/src/providers.rs?raw";
import { describe, expect, it } from "vitest";
import { KEY_PROVIDERS, PROVIDERS, isKeyProvider } from "./providers";

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
  }));
}

describe("providers.ts mirrors providers.rs", () => {
  it("has the same rows in the same order with the same flags", () => {
    const rust = rustRegistry();
    expect(rust.length).toBeGreaterThan(0);
    expect(PROVIDERS.map((p) => ({ ...p }))).toEqual(rust);
  });

  it("derives the key providers from the keychain flag", () => {
    expect(KEY_PROVIDERS).toEqual(["openai", "deepgram", "groq"]);
    expect(isKeyProvider("local")).toBe(false);
    expect(isKeyProvider("groq")).toBe(true);
  });
});
