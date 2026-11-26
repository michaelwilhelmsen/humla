import { Row } from "./Section";
import { Select } from "../../../components/ui/Select";
import { CustomModelField } from "../../../components/provider/CustomEndpoint";
import { useCloudModels } from "../../../components/provider/useCloudModels";
import { useCopilotModels, useCopilotStatus } from "../../../components/provider/useCopilot";
import { useOllamaProbe } from "../../../components/provider/useOllamaProbe";
import { useProviderKey } from "../../../components/provider/useProviderKey";
import { CLEAN_OPENAI_MODEL, cleanModelFor, cleanModelSetting } from "../../../lib/cleanModel";
import { completionModels } from "../../../lib/localModels";

/** Which model cleans transcripts (#217), among the summary provider's own.
 *  Every hook runs, inert unless its provider is the one in use. */
export function CleanModelRow({
  provider,
  setting,
  localBaseUrl,
  onChange,
}: {
  provider: string;
  setting: string;
  localBaseUrl: string;
  onChange: (setting: string) => void;
}) {
  const model = cleanModelFor(setting, provider);
  const choose = (m: string) => onChange(cleanModelSetting(provider, m));
  const isOpenAi = provider === "openai";
  const isAnthropic = provider === "anthropic";
  const isCopilot = provider === "copilot";
  const isLocal = provider === "local" || provider === "ollama";

  const openaiKey = useProviderKey("openai", { enabled: isOpenAi });
  const openai = useCloudModels("openai", openaiKey.hasKey, model, { enabled: isOpenAi });
  const anthropicKey = useProviderKey("anthropic", { enabled: isAnthropic });
  const anthropic = useCloudModels("anthropic", anthropicKey.hasKey, model, { enabled: isAnthropic });
  const copilot = useCopilotStatus({ enabled: isCopilot });
  const copilotModels = useCopilotModels(isCopilot && copilot.ready, model);
  const { installed } = useOllamaProbe(localBaseUrl, { enabled: isLocal, pollMs: 10_000 });

  if (provider === "custom") {
    return (
      <CustomModelField value={model} onChange={choose} description="Empty uses the summary model." />
    );
  }
  const [fallback, models] = isOpenAi
    ? [`Default (${CLEAN_OPENAI_MODEL})`, openai]
    : isAnthropic
      ? ["Same as summaries", anthropic]
      : isCopilot
        ? ["Default (cheapest)", copilotModels]
        : [
            "Same as summaries",
            completionModels(installed).map((m) => ({ value: m, label: m })),
          ];
  const options = [{ value: "", label: fallback }, ...models];
  if (model && !options.some((o) => o.value === model)) options.splice(1, 0, { value: model, label: model });
  return (
    <Row label="Cleaning model" control={<Select value={model} onChange={choose} options={options} />} />
  );
}
