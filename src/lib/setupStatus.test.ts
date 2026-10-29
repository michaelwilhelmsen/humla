import { describe, it, expect } from "vitest";
import { computeSetupStatus } from "./setupStatus";
import { mockTauri } from "../test/tauri";

function withSummaryProvider(provider: string, keys: Record<string, string>) {
  const asked: string[] = [];
  mockTauri({
    settings_get: (args) => ((args as { key: string }).key === "summary_provider" ? provider : null),
    provider_key_get: (args) => {
      const p = (args as { provider: string }).provider;
      asked.push(p);
      return keys[p] ?? null;
    },
  });
  return asked;
}

describe("computeSetupStatus: summary configured", () => {
  it("reads the key of the summary provider in force, not OpenAI's", async () => {
    const asked = withSummaryProvider("anthropic", { anthropic: "stored" });
    expect((await computeSetupStatus()).summaryConfigured).toBe(true);
    expect(asked).not.toContain("openai");
  });

  it("is not configured when only another provider has a key", async () => {
    withSummaryProvider("anthropic", { openai: "stored" });
    expect((await computeSetupStatus()).summaryConfigured).toBe(false);
  });

  it("reads OpenAI's key when no provider is chosen", async () => {
    withSummaryProvider("", { openai: "stored" });
    expect((await computeSetupStatus()).summaryConfigured).toBe(true);
  });
});

// A custom endpoint's key is optional, so what makes it configured is its URL
// and the surface's model — never a key.
describe("computeSetupStatus on a custom endpoint", () => {
  function custom(settings: Record<string, string>) {
    const asked: string[] = [];
    mockTauri({
      settings_get: (args) => settings[(args as { key: string }).key] ?? null,
      get_transcribe_config: () => ({ default: { provider: "custom", model: settings.stt_model ?? "" }, per_language: {} }),
      provider_key_get: (args) => {
        asked.push((args as { provider: string }).provider);
        return null;
      },
    });
    return asked;
  }

  it("transcribes and summarises with a URL and models, and no key", async () => {
    const asked = custom({
      summary_provider: "custom",
      custom_base_url: "https://gw.example/v1",
      custom_model: "deepseek-chat",
      stt_model: "elevenlabs/scribe-v2",
    });
    const status = await computeSetupStatus();
    expect(status.stt).toMatchObject({ working: true, kind: "cloud", provider: "custom", model: "elevenlabs/scribe-v2" });
    expect(status.pipelineReady).toBe(true);
    expect(status.summaryConfigured).toBe(true);
    expect(asked).toEqual([]);
  });

  it("is not working without a base URL, or without a model", async () => {
    custom({ summary_provider: "custom", custom_model: "m", stt_model: "m" });
    let status = await computeSetupStatus();
    expect(status.stt.working).toBe(false);
    expect(status.summaryConfigured).toBe(false);
    custom({ summary_provider: "custom", custom_base_url: "https://gw.example/v1" });
    status = await computeSetupStatus();
    expect(status.stt.working).toBe(false);
    expect(status.summaryConfigured).toBe(false);
  });
});
