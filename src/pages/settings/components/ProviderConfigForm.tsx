import type { ProviderConfig } from "../../../lib/ipc";
import {
  DEEPGRAM_MODELS,
  GROQ_MODELS,
  LOCAL_PROVIDER,
  PROVIDERS_BASE,
  TRANSCRIBE_MODELS,
  WHISPER_PRESETS,
  type Provider,
} from "../types";
import { Row } from "./Section";
import { Select } from "./Select";
import { Toggle } from "./Toggle";

type LocalModelSummary = {
  id: string;
  label: string;
  kind: "multilingual" | "language_specific";
  specificLanguage: string | null;
  downloaded: boolean;
};

// Model-row description per provider: each picker explains what it selects
// and how it relates to the provider chosen above (design-review amendment).
const MODEL_DESCRIPTIONS: Record<Provider, string> = {
  openai:
    "whisper-1 is the safe default; gpt-4o-transcribe-diarize labels speakers but treats language as a hint and ignores vocabulary biasing.",
  deepgram:
    "nova-3 is the current best for English and falls back gracefully to other languages.",
  groq: "Whisper Large v3 Turbo at OpenAI-compatible endpoints — same Whisper quality, ~10× cheaper and faster than OpenAI's hosted version.",
  local:
    "Only downloaded models are offered — manage them under Local models below.",
};

// Reusable provider+model picker rendered as self-describing rows. Used by
// the Default provider section and the per-language override editor. Keeps
// the four provider variants' divergent fields (OpenAI: model, Local:
// model_id+preset+gpu, Deepgram: model, Groq: model) in one place so the
// two callers stay in lockstep.
//
// Returns a fragment of Rows so the parent Section's divide-y hairlines
// apply between them.
//
// `localModels` is the live list of downloaded local models (from
// `useSettings.local.models`). Used to:
//   1. Hide the Local option when nothing is downloaded.
//   2. Pre-select the first downloaded multilingual model when
//      switching to Local.
//   3. Filter the model_id picker to actually-downloaded files.
//
// `filterLocalToLanguage` (when set) restricts the local model picker to
// `LanguageSpecific` models matching the language PLUS multilingual
// fallbacks. Used by the per-language override form so "Norwegian → Local
// → ?" only offers NB Whisper or multilingual options.
export function ProviderConfigForm({
  value,
  onChange,
  localModels,
  filterLocalToLanguage,
  hideLocal = false,
}: {
  value: ProviderConfig;
  onChange: (next: ProviderConfig) => void;
  localModels: LocalModelSummary[];
  filterLocalToLanguage?: string;
  hideLocal?: boolean;
}) {
  const provider = value.provider;
  const localAvailable = !hideLocal && localModels.some((m) => m.downloaded);

  const localModelOptions = localModels
    .filter((m) => m.downloaded)
    .filter((m) => {
      if (!filterLocalToLanguage) return true;
      // Multilingual models always usable; language-specific must match.
      return (
        m.kind === "multilingual" || m.specificLanguage === filterLocalToLanguage
      );
    })
    .map((m) => ({ value: m.id, label: m.label }));

  const modelSelect =
    value.provider === "openai" ? (
      <Select
        value={value.model}
        onChange={(v) => onChange({ provider: "openai", model: v })}
        options={TRANSCRIBE_MODELS.map((m) => ({ value: m, label: m }))}
      />
    ) : value.provider === "deepgram" ? (
      <Select
        value={value.model}
        onChange={(v) => onChange({ provider: "deepgram", model: v })}
        options={DEEPGRAM_MODELS.map((m) => ({ value: m, label: m }))}
      />
    ) : value.provider === "groq" ? (
      <Select
        value={value.model}
        onChange={(v) => onChange({ provider: "groq", model: v })}
        options={GROQ_MODELS.map((m) => ({ value: m, label: m }))}
      />
    ) : (
      <Select
        value={value.model_id}
        onChange={(v) => onChange({ ...value, model_id: v })}
        options={
          localModelOptions.length > 0
            ? localModelOptions
            : [
                {
                  value: value.model_id,
                  label: `${value.model_id} (not downloaded)`,
                },
              ]
        }
      />
    );

  return (
    <>
      <Row
        label="Provider"
        description="Where audio is transcribed — on this Mac or a cloud API."
        control={
          <Select
            value={provider}
            onChange={(v) => {
              const p = v as Provider;
              if (p === "openai") {
                onChange({ provider: "openai", model: "whisper-1" });
              } else if (p === "local") {
                const first = localModels.find(
                  (m) =>
                    m.downloaded &&
                    (filterLocalToLanguage
                      ? m.kind === "multilingual" ||
                        m.specificLanguage === filterLocalToLanguage
                      : m.kind === "multilingual"),
                );
                onChange({
                  provider: "local",
                  model_id: first?.id ?? "large-v3-turbo-q5",
                  preset: "quality",
                  use_gpu: true,
                });
              } else if (p === "deepgram") {
                onChange({ provider: "deepgram", model: "nova-3" });
              } else if (p === "groq") {
                onChange({ provider: "groq", model: "whisper-large-v3-turbo" });
              }
            }}
            options={
              localAvailable ? [...PROVIDERS_BASE, LOCAL_PROVIDER] : PROVIDERS_BASE
            }
          />
        }
      />

      <Row
        label="Model"
        description={MODEL_DESCRIPTIONS[provider]}
        control={modelSelect}
      />

      {value.provider === "local" && (
        <>
          <Row
            label="Quality"
            description="Speed vs accuracy for on-device Whisper — Quality uses beam search and catches more borderline words."
            control={
              <Select
                value={value.preset}
                onChange={(v) => onChange({ ...value, preset: v })}
                options={WHISPER_PRESETS}
              />
            }
          />
          <Row
            label="Use Metal (Apple GPU)"
            description="Runs Whisper inference on the GPU — much faster on Apple Silicon."
            control={
              <Toggle
                label="Use Metal (Apple GPU) for Whisper inference"
                checked={value.use_gpu}
                onChange={(on) => onChange({ ...value, use_gpu: on })}
              />
            }
          />
        </>
      )}
    </>
  );
}
