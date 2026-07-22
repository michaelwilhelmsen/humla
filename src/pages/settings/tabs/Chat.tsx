import { open as openExternal } from "@tauri-apps/plugin-shell";
import { Row, Section } from "../components/Section";
import { Select } from "../components/Select";
import { OllamaConnect } from "../../../components/provider/OllamaConnect";
import { ProviderKeyCard } from "../../../components/provider/ProviderKeyCard";
import { CommandSnippet } from "../../../components/CommandSnippet";
import { useOllamaProbe } from "../../../components/provider/useOllamaProbe";
import { useProviderKey } from "../../../components/provider/useProviderKey";
import { CHAT_PROVIDERS, SUMMARY_MODELS } from "../types";
import type { SettingsHook } from "../useSettings";

// AI Chat provider setting (issue #44). A dedicated provider choice, separate
// from transcription/summary: OpenAI (cloud, shared key) or Ollama (local).
// No chat behaviour yet — just the choice, its persistence, and a readiness
// prompt saying exactly what's still missing. The embedding model is
// auto-derived (text-embedding-3-small / embeddinggemma) and not surfaced here;
// its setup lands with semantic retrieval.
export function ChatTab({ s, update }: Pick<SettingsHook, "s" | "update">) {
  const isOllama = s.chat_provider === "ollama";
  // Both hooks run unconditionally (rules of hooks); the probe parks itself
  // when chat isn't on Ollama.
  const key = useProviderKey("openai");
  const { reachable, installed } = useOllamaProbe(s.local_llm_base_url, { enabled: isOllama });

  // Readiness — reflect exactly what's missing before chat can run.
  let ready = false;
  let hint = "";
  if (isOllama) {
    if (reachable === false) hint = "Start or install Ollama — it's detected automatically.";
    else if (!s.chat_model) hint = "Choose a chat model above.";
    else if (installed && !installed.includes(s.chat_model))
      hint = `“${s.chat_model}” isn't installed on the server — run ollama pull ${s.chat_model}.`;
    else ready = true;
  } else {
    if (!key.hasKey) hint = "Add your OpenAI key above to use chat.";
    else if (!s.chat_model) hint = "Choose a chat model above.";
    else ready = true;
  }

  // Show the stored value even when it isn't a known option (e.g. an empty
  // choice, or a model list the app doesn't hard-code) so nothing looks blank.
  const openaiModelOptions = [
    ...(s.chat_model === "" ? [{ value: "", label: "Choose a model…" }] : []),
    ...SUMMARY_MODELS.map((m) => ({ value: m, label: m })),
  ];

  return (
    <Section title="AI Chat">
      <Row
        label="Provider"
        description="Ask questions grounded in your notes. Cloud (OpenAI) uses your key; Local (Ollama) runs fully offline. Independent of your transcription and summary providers."
        control={
          <Select
            value={s.chat_provider}
            onChange={(v) => update("chat_provider", v)}
            options={CHAT_PROVIDERS}
          />
        }
      />

      {!isOllama && (
        <>
          <Row
            label="Model"
            description="A GPT-5-class model. Reasoning and tool-calling are handled automatically."
            control={
              <Select
                value={s.chat_model}
                onChange={(v) => update("chat_model", v)}
                options={openaiModelOptions}
              />
            }
          />
          <ProviderKeyCard
            provider="openai"
            description="Reused across cloud transcription, summaries, and chat — one key."
          />
        </>
      )}

      {isOllama && (
        <>
          <OllamaConnect
            baseUrl={s.local_llm_base_url}
            onBaseUrlChange={(v) => update("local_llm_base_url", v)}
            model={s.chat_model}
            onModelChange={(v) => update("chat_model", v)}
          />
          <div className="py-3 space-y-2">
            <p className="text-xs text-[var(--color-text-muted)]">
              Runs fully offline. Don't have Ollama yet?{" "}
              <button
                type="button"
                onClick={() => openExternal("https://ollama.com/download")}
                className="underline hover:text-[var(--color-text)]"
              >
                Install Ollama
              </button>
              , then pull a tool-calling-capable model:
            </p>
            <CommandSnippet
              command={`ollama pull ${s.chat_model || "qwen3.5:4b"}`}
              ariaLabel="Copy Ollama pull command"
            />
          </div>
        </>
      )}

      <Row label="Status">
        <span className={ready ? "text-xs text-[var(--color-success)]" : "text-xs text-[var(--color-warning)]"}>
          {ready ? "Ready ✓" : "Setup needed"}
        </span>
        {!ready && hint && (
          <p className="text-xs text-[var(--color-text-muted)] mt-1">{hint}</p>
        )}
      </Row>
    </Section>
  );
}
