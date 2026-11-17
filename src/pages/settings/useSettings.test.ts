import { describe, it, expect } from "vitest";
import { act, renderHook, waitFor } from "@testing-library/react";
import { emit } from "@tauri-apps/api/event";
import { mockTauri } from "../../test/tauri";
import type { TranscribeConfig } from "../../lib/ipc";
import { useSettings } from "./useSettings";

// A backend that stores settings and the transcription config, so a test can
// rewrite them behind the hook's back the way `apply_local_only` does.
function backend(stored: Record<string, string>, config: { current: TranscribeConfig }) {
  const writes: { key: string; value: string }[] = [];
  const configWrites: TranscribeConfig[] = [];
  mockTauri(
    {
      settings_get: (args) => stored[(args as { key: string }).key] ?? null,
      settings_set: (args) => {
        const { key, value } = args as { key: string; value: string };
        stored[key] = value;
        writes.push({ key, value });
        return null;
      },
      get_transcribe_config: () => config.current,
      set_transcribe_config: (args) => {
        config.current = (args as { config: TranscribeConfig }).config;
        configWrites.push(config.current);
        return null;
      },
    },
    { events: true },
  );
  return { writes, configWrites };
}

describe("useSettings after an organisation policy rewrites settings", () => {
  it("reloads, so a later edit doesn't bring the refused value back", async () => {
    const stored: Record<string, string> = { summary_provider: "anthropic", chat_provider: "openai" };
    const config = {
      current: {
        default: { provider: "deepgram", model: "nova-3" },
        per_language: {},
      } as TranscribeConfig,
    };
    const { configWrites } = backend(stored, config);
    const { result } = renderHook(() => useSettings());
    await waitFor(() => expect(result.current.s.summary_provider).toBe("anthropic"));
    await waitFor(() => expect(result.current.transcribeConfig.default.provider).toBe("deepgram"));

    // What `apply_local_only` writes when local_only arrives.
    stored.summary_provider = "local";
    stored.chat_provider = "ollama";
    config.current = {
      default: { provider: "local", model_id: "large-v3-turbo-q5", preset: "quality", use_gpu: true },
      per_language: {},
    };
    await act(() => emit("org_settings_rewritten"));
    await waitFor(() => expect(result.current.s.summary_provider).toBe("local"));
    await waitFor(() => expect(result.current.transcribeConfig.default.provider).toBe("local"));

    await act(() =>
      result.current.setLanguageOverride("sv", {
        provider: "local",
        model_id: "large-v3-turbo-q5",
        preset: "balanced",
        use_gpu: true,
      }),
    );
    expect(configWrites.at(-1)?.default.provider).toBe("local");
    expect(result.current.s.chat_provider).toBe("ollama");
  });

  it("doesn't reload on a policy change that rewrote nothing", async () => {
    const stored: Record<string, string> = { summary_provider: "local" };
    const config = {
      current: { default: { provider: "deepgram", model: "nova-3" }, per_language: {} } as TranscribeConfig,
    };
    backend(stored, config);
    const { result } = renderHook(() => useSettings());
    await waitFor(() => expect(result.current.s.summary_provider).toBe("local"));
    // Behind the hook's back; a reload would pick it up.
    stored.summary_provider = "custom";
    config.current = { default: { provider: "groq", model: "whisper-large-v3-turbo" }, per_language: {} };
    await act(() => emit("org_policy_changed"));
    await new Promise((r) => setTimeout(r, 50));
    expect(result.current.s.summary_provider).toBe("local");
    expect(result.current.transcribeConfig.default.provider).toBe("deepgram");
  });
});
