import { open as openExternal } from "@tauri-apps/plugin-shell";
import { Row, Section } from "../components/Section";
import { Select } from "../../../components/ui/Select";
import { Toggle } from "../components/Toggle";
import { SummaryPromptsManager } from "../components/SummaryPromptsManager";
import { OllamaConnect } from "../../../components/provider/OllamaConnect";
import { ProviderKeyCard } from "../../../components/provider/ProviderKeyCard";
import { CommandSnippet } from "../../../components/CommandSnippet";
import { RECOMMENDED_OLLAMA_MODEL, RECOMMENDED_OLLAMA_MODEL_16GB } from "../../../lib/localModels";
import { SUMMARY_PRESETS, presetLabel } from "../../../lib/presets";
import { useCloudModels } from "../../../components/provider/useCloudModels";
import { useProviderKey } from "../../../components/provider/useProviderKey";
import { CopilotCard } from "../../../components/provider/CopilotCard";
import { CustomEndpointCard, CustomModelField } from "../../../components/provider/CustomEndpoint";
import { useCopilotStatus } from "../../../components/provider/useCopilot";
import { SUMMARY_PROVIDERS } from "../types";
import type { SettingsHook } from "../useSettings";

export function SummaryTab({
  s,
  update,
}: Pick<SettingsHook, "s" | "update">) {
  const isLocal = s.summary_provider === "local";
  const isAnthropic = s.summary_provider === "anthropic";
  // Called unconditionally (rules of hooks) and inert until that provider is
  // the chosen one — neither the Keychain slot nor the model listing is
  // touched otherwise.
  const isOpenAi = s.summary_provider === "openai";
  const openaiKey = useProviderKey("openai", { enabled: isOpenAi });
  const openaiModels = useCloudModels("openai", openaiKey.hasKey, s.summary_model, {
    enabled: isOpenAi,
  });
  const anthropicKey = useProviderKey("anthropic", { enabled: isAnthropic });
  const anthropicModels = useCloudModels("anthropic", anthropicKey.hasKey, s.anthropic_model, {
    enabled: isAnthropic,
  });
  const isCopilot = s.summary_provider === "copilot";
  const copilot = useCopilotStatus({ enabled: isCopilot });

  return (
    <>
      <Section title="Summaries">
        <Row
          label="Provider"
          description="Local keeps the transcript on your Mac. The others send it to the provider."
          control={
            <Select
              value={s.summary_provider}
              onChange={(v) => update("summary_provider", v)}
              options={SUMMARY_PROVIDERS}
            />
          }
        />

        {isOpenAi && (
          <>
            <Row
              label="Model"
              control={
                <Select
                  value={s.summary_model}
                  onChange={(v) => update("summary_model", v)}
                  options={openaiModels}
                />
              }
            />
            <ProviderKeyCard provider="openai" />
          </>
        )}

        {isAnthropic && (
          <>
            <Row
              label="Model"
              control={
                <Select
                  value={s.anthropic_model}
                  onChange={(v) => update("anthropic_model", v)}
                  options={anthropicModels}
                />
              }
            />
            <ProviderKeyCard provider="anthropic" />
          </>
        )}

        {isCopilot && (
          <CopilotCard
            copilot={copilot}
            model={s.copilot_model}
            onModelChange={(v) => update("copilot_model", v)}
            modelDescription="Also used for titles."
          />
        )}

        {s.summary_provider === "custom" && (
          <>
            <CustomEndpointCard
              baseUrl={s.custom_base_url}
              onBaseUrlChange={(v) => update("custom_base_url", v)}
              testModel={s.custom_model}
            />
            <CustomModelField
              value={s.custom_model}
              onChange={(v) => update("custom_model", v)}
              description="Also used for titles. The endpoint's own defaults apply."
            />
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
                Don't have a server?{" "}
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
                On a 16 GB Mac use <code>ollama pull {RECOMMENDED_OLLAMA_MODEL_16GB}</code> instead.{" "}
                {RECOMMENDED_OLLAMA_MODEL} needs 24 GB.
              </p>
            </div>
            <Row
              label="Thinking mode"
              description="Qwen 3+ on Ollama. Can take many minutes on a long meeting."
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
          description="For new notes."
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
