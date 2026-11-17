import src from "../../src-tauri/src/providers.rs?raw";
import policySrc from "../../src-tauri/src/org_policy.rs?raw";
import { describe, expect, it } from "vitest";
import {
  KEY_PROVIDERS,
  LOCAL_ONLY_RULE,
  PROVIDERS,
  TRANSCRIBE_KEY_PROVIDERS,
  allowedUnderLocalOnly,
  isKeyProvider,
} from "./providers";

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

// `org_policy::local_only_rule` as { id_str: snake_case rule }.
function rustLocalOnlyRules() {
  const variantToId = Object.fromEntries(
    [...src.matchAll(/id: ProviderId::(\w+),\s*id_str: "([a-z0-9-]+)"/g)].map((m) => [m[1], m[2]]),
  );
  const start = policySrc.indexOf("fn local_only_rule");
  const body = policySrc.slice(start, policySrc.indexOf("\n}", start));
  const snake = (v: string) => v.replace(/[A-Z]/g, (c, i) => (i ? "_" : "") + c.toLowerCase());
  return Object.fromEntries(
    [...body.matchAll(/ProviderId::(\w+) => LocalOnly::(\w+)/g)].map((m) => [variantToId[m[1]], snake(m[2])]),
  );
}

describe("allowedUnderLocalOnly mirrors org_policy::allows_endpoint", () => {
  it("has the same rule per provider as local_only_rule", () => {
    const rust = rustLocalOnlyRules();
    expect(Object.keys(rust)).toHaveLength(PROVIDERS.length);
    expect(LOCAL_ONLY_RULE).toEqual(rust);
  });

  it("allows on-device Whisper and loopback servers, refuses the rest", () => {
    const local = "http://localhost:11434/v1";
    const lan = "http://192.168.1.20:11434/v1";
    expect(allowedUnderLocalOnly("local", null)).toBe(true);
    expect(allowedUnderLocalOnly("local", local)).toBe(true);
    expect(allowedUnderLocalOnly("local", lan)).toBe(false);
    expect(allowedUnderLocalOnly("custom", local)).toBe(true);
    expect(allowedUnderLocalOnly("custom", lan)).toBe(false);
    expect(allowedUnderLocalOnly("custom", null)).toBe(false);
    for (const p of ["openai", "anthropic", "copilot", "deepgram", "groq"] as const) {
      expect(allowedUnderLocalOnly(p, null)).toBe(false);
      expect(allowedUnderLocalOnly(p, local)).toBe(false);
    }
  });
});
