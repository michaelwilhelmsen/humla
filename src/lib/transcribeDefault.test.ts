import { describe, it, expect } from "vitest";
import { chosenCloudProvider, isCloudProvider } from "./transcribeDefault";
import { mockTauri } from "../test/tauri";
import type { ProviderConfig } from "./ipc";

// Two readers hang off this rule — computeSetupStatus (is the pipeline
// working, or is this a fresh install?) and the onboarding Transcription
// step's resume (#149) — and neither can observe the other. Pin the rule
// itself so a change to it can't quietly mean two different things.

const LOCAL: ProviderConfig = {
  provider: "local",
  model_id: "large-v3-turbo-q5",
  preset: "quality",
  use_gpu: true,
};

// Records which providers the Keychain was actually asked about, so a test
// can assert that a local default costs no key read at all.
function withKeys(keys: Record<string, string | null>) {
  const asked: string[] = [];
  mockTauri({
    provider_key_get: (args) => {
      const p = (args as { provider: string }).provider;
      asked.push(p);
      return keys[p] ?? null;
    },
  });
  return asked;
}

describe("isCloudProvider", () => {
  it("is every provider with a Keychain slot, and only those", () => {
    expect(isCloudProvider("openai")).toBe(true);
    expect(isCloudProvider("deepgram")).toBe(true);
    expect(isCloudProvider("groq")).toBe(true);
    // The one provider that needs no key — and so can never be resumed by
    // key presence.
    expect(isCloudProvider("local")).toBe(false);
  });
});

describe("chosenCloudProvider", () => {
  it("returns the provider when its key is stored", async () => {
    withKeys({ openai: "sk-stored", deepgram: "dg-stored", groq: "gsk-stored" });

    expect(await chosenCloudProvider({ provider: "openai", model: "whisper-1" })).toBe(
      "openai",
    );
    expect(await chosenCloudProvider({ provider: "deepgram", model: "nova-3" })).toBe(
      "deepgram",
    );
    expect(
      await chosenCloudProvider({ provider: "groq", model: "whisper-large-v3-turbo" }),
    ).toBe("groq");
  });

  // The whole point: `openai` is also the fresh-install fallback, so without a
  // key it is not evidence that anyone chose anything.
  it("returns null for a cloud default with no stored key", async () => {
    withKeys({});
    expect(await chosenCloudProvider({ provider: "openai", model: "whisper-1" })).toBeNull();
    expect(await chosenCloudProvider({ provider: "deepgram", model: "nova-3" })).toBeNull();
  });

  it("asks only about the provider in the config", async () => {
    const asked = withKeys({ openai: "sk-stored", deepgram: "dg-stored" });
    await chosenCloudProvider({ provider: "deepgram", model: "nova-3" });
    expect(asked).toEqual(["deepgram"]);
  });

  it("returns null for a local default without reading any key", async () => {
    const asked = withKeys({ openai: "sk-stored" });
    expect(await chosenCloudProvider(LOCAL)).toBeNull();
    expect(asked).toEqual([]);
  });

  it("returns null for no config at all, without reading any key", async () => {
    const asked = withKeys({ openai: "sk-stored" });
    expect(await chosenCloudProvider(null)).toBeNull();
    expect(await chosenCloudProvider(undefined)).toBeNull();
    expect(asked).toEqual([]);
  });

  // An unreadable Keychain is not evidence of a choice. It must resolve to
  // null rather than reject — callers treat this as "fresh install", and a
  // rejection here would escape into a floated effect (#149).
  it("resolves null when the key read throws", async () => {
    mockTauri({
      provider_key_get: () => {
        throw new Error("keychain locked");
      },
    });
    await expect(
      chosenCloudProvider({ provider: "openai", model: "whisper-1" }),
    ).resolves.toBeNull();
  });

  // The backend returns a sentinel, never the key, but an empty string would
  // be a "present" key under a truthiness bug. Treat it as absent.
  it("treats an empty-string key as no key", async () => {
    withKeys({ openai: "" });
    expect(await chosenCloudProvider({ provider: "openai", model: "whisper-1" })).toBeNull();
  });
});
