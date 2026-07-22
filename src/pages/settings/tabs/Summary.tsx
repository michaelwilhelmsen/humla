import { open as openExternal } from "@tauri-apps/plugin-shell";
import { Row, Section } from "../components/Section";
import { Select } from "../components/Select";
import { Toggle } from "../components/Toggle";
import { SummaryPromptsManager } from "../components/SummaryPromptsManager";
import { OllamaConnect } from "../../../components/provider/OllamaConnect";
import { ProviderKeyCard } from "../../../components/provider/ProviderKeyCard";
import { CommandSnippet } from "../../../components/CommandSnippet";
import { RECOMMENDED_OLLAMA_MODEL, RECOMMENDED_OLLAMA_MODEL_16GB } from "../../../lib/localModels";
import { SUMMARY_PRESETS, presetLabel } from "../../../lib/presets";
import { SUMMARY_MODELS, SUMMARY_PROVIDERS } from "../types";
import type { SettingsHook } from "../useSettings";

export function SummaryTab({
  s,
  update,
}: Pick<SettingsHook, "s" | "update">) {
  const isLocal = s.summary_provider === "local";

  return (
    <>
      <Section title="Summaries">
        <Row
          label="Provider"
          description="Local keeps the transcript on your Mac — pick this for confidential meetings. Cloud (OpenAI) is faster and produces better summaries but sends the transcript to OpenAI."
          control={
            <Select
              value={s.summary_provider}
              onChange={(v) => update("summary_provider", v)}
              options={SUMMARY_PROVIDERS}
            />
          }
        />

        {s.summary_provider === "openai" && (
          <>
            <Row
              label="Model"
              description="Reasoning models (gpt-5.x, o-series) are handled automatically."
              control={
                <Select
                  value={s.summary_model}
                  onChange={(v) => update("summary_model", v)}
                  options={SUMMARY_MODELS.map((m) => ({ value: m, label: m }))}
                />
              }
            />
            <ProviderKeyCard provider="openai" />
          </>
        )}

        {isLocal && (
          <>
            <OllamaConnect
              baseUrl={s.local_llm_base_url}
              onBaseUrlChange={(v) => update("local_llm_base_url", v)}
              model={s.local_llm_model}
              onModelChange={(v) => update("local_llm_model", v)}
            />
            <div className="py-3 space-y-2">
              <p className="text-xs text-[var(--color-text-muted)]">
                Works with Ollama, LM Studio (<code>http://localhost:1234/v1</code>),
                <code> llama-server</code>, and vLLM. Don't have one yet?{" "}
                <button
                  type="button"
                  onClick={() => openExternal("https://ollama.com/download")}
                  className="underline hover:text-[var(--color-text)]"
                >
                  Install Ollama
                </button>
                , then pull the recommended model:
              </p>
              <CommandSnippet
                command={`ollama pull ${RECOMMENDED_OLLAMA_MODEL}`}
                ariaLabel="Copy Ollama pull command"
              />
              <p className="text-xs text-[var(--color-text-muted)]">
                On a 16 GB Mac use <code>ollama pull {RECOMMENDED_OLLAMA_MODEL_16GB}</code> instead —{" "}
                {RECOMMENDED_OLLAMA_MODEL} needs ~24 GB+; 9B and up will OOM during summary.
              </p>
            </div>
            <Row
              label="Thinking mode"
              description="Qwen 3+ reasons internally before answering — sometimes higher quality, but can take many minutes on a long meeting. Ollama only; other servers ignore it."
              control={
                <Toggle
                  label="Enable thinking mode"
                  checked={s.local_llm_think === "true"}
                  onChange={(on) =>
                    update("local_llm_think", on ? "true" : "false")
                  }
                />
              }
            />
          </>
        )}

        <Row
          label="Default preset"
          description='Which preset new notes start with. Each note can switch to a different preset (or "Custom") from its own header.'
          control={
            <Select
              value={s.default_summary_preset}
              onChange={(v) => update("default_summary_preset", v)}
              options={SUMMARY_PRESETS.map((p) => ({
                value: p.value,
                label: presetLabel(p),
              }))}
            />
          }
        />
      </Section>

      <Section title="Summary prompts">
        <SummaryPromptsManager language={s.language} />
      </Section>
    </>
  );
}
