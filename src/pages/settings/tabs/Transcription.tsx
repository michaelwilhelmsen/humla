import type { ReactNode } from "react";
import { DiarizeModelManager } from "../components/DiarizeModelManager";
import { Disclosure } from "../components/Disclosure";
import { LocalModelManager } from "../components/LocalModelManager";
import { PerLanguageOverrides } from "../components/PerLanguageOverrides";
import { ProviderConfigForm } from "../components/ProviderConfigForm";
import { Row, Section } from "../components/Section";
import { RenameYouRow } from "../components/RenameYouRow";
import { Select } from "../../../components/ui/Select";
import { ProviderKeyCard, type KeyProvider } from "../../../components/provider/ProviderKeyCard";
import { CustomEndpointCard } from "../../../components/provider/CustomEndpoint";
import { NEMOTRON_MAX_SPEAKERS } from "../../../lib/diarizeEngine";
import { KEY_ONLY_PROVIDERS } from "../../../lib/providers";
import { LANGUAGES, languageOptionLabel } from "../../../lib/languages";
import { enforceLocalOnly, useWorkspaceScope, useLocalOnly } from "../../../lib/orgPolicy";
import type { SettingsHook } from "../useSettings";

// The selected cloud provider's key card renders right under the picker;
// the others sit in Advanced so keys for override-only providers stay
// reachable. Copy comes from the card's per-provider defaults.

// Engine choice row: label + description left, pick-radio right, the
// download/status manager underneath. The radio is presence-gated — an
// engine can't be active until its models are on disk.
function EngineOption({
  label,
  description,
  checked,
  disabled,
  onPick,
  children,
}: {
  label: string;
  description: string;
  checked: boolean;
  disabled: boolean;
  onPick: () => void;
  children: ReactNode;
}) {
  return (
    <div className="py-3.5">
      <div className="flex items-start justify-between gap-6">
        <div className="min-w-0">
          <div className="text-sm">{label}</div>
          <p className="text-xs text-[var(--color-text-muted)] mt-0.5">
            {description}
          </p>
        </div>
        <input
          type="radio"
          name="diarize_model"
          checked={checked}
          disabled={disabled}
          onChange={onPick}
          aria-label={`Use ${label} for new recordings`}
          className="mt-1 shrink-0"
        />
      </div>
      <div className="mt-3">{children}</div>
    </div>
  );
}

// Small right-aligned mono numeric field with the row model's label +
// description — the threshold knobs' shared shape.
function ThresholdRow({
  label,
  description,
  value,
  placeholder,
  onChange,
}: {
  label: string;
  description: string;
  value: string;
  placeholder: string;
  onChange: (v: string) => void;
}) {
  return (
    <Row
      label={label}
      description={description}
      control={
        <input
          type="text"
          value={value}
          onChange={(e) => onChange(e.target.value)}
          placeholder={placeholder}
          aria-label={label}
          className="w-24 text-sm text-right px-2.5 py-1.5 rounded-md border border-[var(--color-line-visible)] bg-[var(--color-surface)] focus:border-[var(--color-text-muted)] transition-colors"
          style={{ fontFamily: "var(--font-mono)" }}
        />
      }
    />
  );
}

export function TranscriptionTab({
  s,
  update,
  transcribeConfig,
  setDefaultConfig,
  setLanguageOverride,
  removeLanguageOverride,
  local,
  downloadModel,
  deleteModel,
  diarize,
  downloadDiarize,
  deleteDiarize,
}: Pick<
  SettingsHook,
  | "s"
  | "update"
  | "transcribeConfig"
  | "setDefaultConfig"
  | "setLanguageOverride"
  | "removeLanguageOverride"
  | "local"
  | "downloadModel"
  | "deleteModel"
  | "diarize"
  | "downloadDiarize"
  | "deleteDiarize"
>) {
  const { managed, known } = useLocalOnly();
  const workspaceScope = useWorkspaceScope();
  const def = managed
    ? enforceLocalOnly(transcribeConfig.default, s.custom_base_url)
    : transcribeConfig.default;

  return (
    <>
      <Section title="Transcription" scope={workspaceScope}>
        <ProviderConfigForm
          value={def}
          onChange={setDefaultConfig}
          localModels={local.models}
          managed={managed}
        />
        {def.provider === "local" && !local.models.some((m) => m.downloaded) && (
          <p className="text-xs text-[var(--color-danger)] py-3">
            No local model downloaded. Download one under Advanced.
          </p>
        )}
        {def.provider === "custom" && (
          <CustomEndpointCard
            baseUrl={s.custom_base_url}
            onBaseUrlChange={(v) => update("custom_base_url", v)}
          />
        )}
        {known && def.provider !== "local" && def.provider !== "custom" && (
          <ProviderKeyCard provider={def.provider as KeyProvider} />
        )}
        <Row
          label="Language"
          description="Default for new notes."
          control={
            <Select
              value={s.language}
              onChange={(v) => update("language", v)}
              options={LANGUAGES.map((l) => ({
                value: l.value,
                label: languageOptionLabel(l),
              }))}
            />
          }
        />
        <Disclosure label="Advanced">
          <div>
            <div className="text-sm mb-2">Per-language overrides</div>
            <PerLanguageOverrides
              config={transcribeConfig}
              setLanguageOverride={setLanguageOverride}
              removeLanguageOverride={removeLanguageOverride}
              local={local}
              managed={managed}
              customBaseUrl={s.custom_base_url}
            />
          </div>
          <div>
            <div className="text-sm">Local models</div>
            <LocalModelManager
              state={local}
              activeId={def.provider === "local" ? def.model_id : ""}
              language={s.language}
              onDownload={downloadModel}
              onDelete={deleteModel}
              setLanguageOverride={setLanguageOverride}
              onSelect={(id) => {
                // Selecting a local model from the manager pins it as the
                // default's model_id. If currently on a non-local default,
                // switch them to Local with this model — matches the v0.23
                // implicit behaviour of the radio button.
                if (def.provider === "local") {
                  setDefaultConfig({ ...def, model_id: id });
                } else {
                  setDefaultConfig({
                    provider: "local",
                    model_id: id,
                    preset: "quality",
                    use_gpu: true,
                  });
                }
              }}
            />
          </div>
          {known && !managed && (
            <div>
              <div className="text-sm">Other API keys</div>
              <p className="text-xs text-[var(--color-text-muted)] mt-0.5">
                For per-language overrides.
              </p>
              {KEY_ONLY_PROVIDERS.filter((p) => p !== def.provider).map((p) => (
                <ProviderKeyCard key={p} provider={p} />
              ))}
            </div>
          )}
          {def.provider !== "custom" && (
            <div>
              <div className="text-sm">Custom endpoint</div>
              <p className="text-xs text-[var(--color-text-muted)] mt-0.5">
                For a per-language override.
              </p>
              <CustomEndpointCard
                baseUrl={s.custom_base_url}
                onBaseUrlChange={(v) => update("custom_base_url", v)}
              />
            </div>
          )}
        </Disclosure>
      </Section>

      <Section title="Speaker labels">
        <EngineOption
          label="Nemotron 3 (end-to-end)"
          description={`Limit: ${NEMOTRON_MAX_SPEAKERS} speakers.`}
          checked={s.diarize_model === "nemotron3"}
          disabled={!diarize.nemotron3.status?.downloaded}
          onPick={() => update("diarize_model", "nemotron3")}
        >
          <DiarizeModelManager
            state={diarize.nemotron3}
            cost="193 MB. Setup takes about 2 minutes, once."
            onDownload={() => downloadDiarize("nemotron3")}
            onDelete={() => deleteDiarize("nemotron3")}
          />
          {s.diarize_model === "nemotron3" &&
            diarize.nemotron3.status?.downloaded === false &&
            diarize.community1.status?.downloaded && (
              <p className="text-xs text-[var(--color-text-muted)] mt-2">
                Until it’s ready, recordings are labelled with Community-1.
              </p>
            )}
        </EngineOption>
        <EngineOption
          label="Community-1 (clustering)"
          description={`Uses the note’s speaker count. On Auto it can merge a meeting one person dominates into one speaker. Keep it downloaded: it labels notes set above ${NEMOTRON_MAX_SPEAKERS} speakers.`}
          checked={s.diarize_model === "community1"}
          disabled={!diarize.community1.status?.downloaded}
          onPick={() => update("diarize_model", "community1")}
        >
          <DiarizeModelManager
            state={diarize.community1}
            cost="22 MB. Setup takes 20–30 s."
            onDownload={() => downloadDiarize("community1")}
            onDelete={() => deleteDiarize("community1")}
          />
        </EngineOption>
        <Disclosure label="Advanced">
          <ThresholdRow
            label="Community-1 clustering threshold"
            description="Higher means more speakers. Default 0.5."
            value={s.community1_threshold}
            placeholder="0.5"
            onChange={(v) => update("community1_threshold", v)}
          />
          <ThresholdRow
            label="Silence RMS threshold"
            description="Quieter audio is skipped. Too high cuts quiet speech. Default 0.008."
            value={s.silence_rms_threshold}
            placeholder="0.008"
            onChange={(v) => update("silence_rms_threshold", v)}
          />
          <p className="text-xs text-[var(--color-text-muted)]">
            Applies from the next recording or re-diarize.
          </p>
        </Disclosure>

        {/* Count-gated, so it is absent for a new install and gone for good once
            used (#116 part 2). In Transcription rather than Chat because the row
            is about what transcripts say. */}
        <RenameYouRow />
      </Section>
    </>
  );
}
