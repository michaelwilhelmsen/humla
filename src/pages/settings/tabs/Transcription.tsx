import type { ReactNode } from "react";
import { DiarizeModelManager } from "../components/DiarizeModelManager";
import { Disclosure } from "../components/Disclosure";
import { LocalModelManager } from "../components/LocalModelManager";
import { PerLanguageOverrides } from "../components/PerLanguageOverrides";
import { ProviderConfigForm } from "../components/ProviderConfigForm";
import { Row, Section } from "../components/Section";
import { Select } from "../../../components/ui/Select";
import { ProviderKeyCard, type KeyProvider } from "../../../components/provider/ProviderKeyCard";
import { LANGUAGES, languageOptionLabel } from "../../../lib/languages";
import { inputClass } from "../types";
import type { SettingsHook } from "../useSettings";

// The selected cloud provider's key card renders right under the picker;
// the others sit in Advanced so keys for override-only providers stay
// reachable. Copy comes from the card's per-provider defaults.
const KEY_PROVIDERS: KeyProvider[] = ["openai", "deepgram", "groq"];

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
  sortformer,
  downloadSortformer,
  deleteSortformer,
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
  | "sortformer"
  | "downloadSortformer"
  | "deleteSortformer"
>) {
  const def = transcribeConfig.default;

  return (
    <>
      <Section title="Transcription">
        <ProviderConfigForm
          value={def}
          onChange={setDefaultConfig}
          localModels={local.models}
        />
        {def.provider === "local" && !local.models.some((m) => m.downloaded) && (
          <p className="text-xs text-[var(--color-danger)] py-3">
            No local model is downloaded. Download one under Advanced below
            before recording.
          </p>
        )}
        {def.provider !== "local" && (
          <ProviderKeyCard provider={def.provider as KeyProvider} />
        )}
        <Row
          label="Language"
          description="Default for new notes. Each note has its own language chip in the header that overrides this."
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
            <div className="text-sm">Per-language overrides</div>
            <p className="text-xs text-[var(--color-text-muted)] mt-0.5 mb-3">
              Route specific recording languages to a different provider than
              the default above — e.g. NB Whisper for Norwegian, Deepgram
              Nova-3 for English.
            </p>
            <PerLanguageOverrides
              config={transcribeConfig}
              setLanguageOverride={setLanguageOverride}
              removeLanguageOverride={removeLanguageOverride}
              local={local}
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
          <div>
            <div className="text-sm">Other API keys</div>
            <p className="text-xs text-[var(--color-text-muted)] mt-0.5">
              Keys for providers used only by per-language overrides.
            </p>
            {KEY_PROVIDERS.filter((p) => p !== def.provider).map((p) => (
              <ProviderKeyCard key={p} provider={p} />
            ))}
          </div>
        </Disclosure>
      </Section>

      <Section title="Speaker labels">
        <p className="text-xs text-[var(--color-text-muted)] py-3">
          When downloaded and active, every recording is automatically
          tagged with <code>Speaker 1:</code> / <code>Speaker 2:</code>
          labels after stop. Both engines run locally via CoreML / Apple
          Neural Engine; pick whichever works better for your recordings.
        </p>
        <EngineOption
          label="Community-1 (clustering)"
          description="Pyannote community-1 segmentation + WeSpeaker embeddings + VBx clustering. Strong baseline; auto-detects speaker count; occasionally collapses on rapid back-and-forth in the same channel."
          checked={s.diarize_model === "community1"}
          disabled={!diarize.status?.downloaded}
          onPick={() => update("diarize_model", "community1")}
        >
          <DiarizeModelManager
            state={diarize}
            onDownload={downloadDiarize}
            onDelete={deleteDiarize}
          />
        </EngineOption>
        <EngineOption
          label="Sortformer (end-to-end)"
          description="NVIDIA Sortformer running in batch over the saved WAV. Fixed 4-speaker cap. Handles the rapid speaker changes clustering struggles with — the answer if Community-1 keeps confusing your speakers."
          checked={s.diarize_model === "sortformer"}
          disabled={!sortformer.status?.downloaded}
          onPick={() => update("diarize_model", "sortformer")}
        >
          <DiarizeModelManager
            state={sortformer}
            onDownload={downloadSortformer}
            onDelete={deleteSortformer}
          />
        </EngineOption>
        <Disclosure label="Advanced">
          <ThresholdRow
            label="Community-1 clustering threshold"
            description="Higher = more aggressive separation (more speakers); lower = more merging. Default 0.5. Community-1 only."
            value={s.community1_threshold}
            placeholder="0.5"
            onChange={(v) => update("community1_threshold", v)}
          />
          <ThresholdRow
            label="Sortformer silence threshold"
            description="Sum of speaker probabilities below which a frame is treated as silence. Default 0.5."
            value={s.sortformer_silence_threshold}
            placeholder="0.5"
            onChange={(v) => update("sortformer_silence_threshold", v)}
          />
          <ThresholdRow
            label="Sortformer prediction threshold"
            description="Speech-probability threshold for crediting a speaker. Default 0.25."
            value={s.sortformer_pred_threshold}
            placeholder="0.25"
            onChange={(v) => update("sortformer_pred_threshold", v)}
          />
          <ThresholdRow
            label="Silence RMS threshold"
            description="Chunks quieter than this are skipped before Whisper sees them — prevents hallucinations on near-silence and mic hiss; too high and quiet speech gets cut. Default 0.008 (pure silence ≈ 0.0001, room tone ≈ 0.001, soft speech ≈ 0.01+)."
            value={s.silence_rms_threshold}
            placeholder="0.008"
            onChange={(v) => update("silence_rms_threshold", v)}
          />
          <p className="text-xs text-[var(--color-text-muted)]">
            Tweaks apply on the next recording or re-diarize. Diagnostic
            JSON is dumped per run — open the Note's diagnostics folder
            from its header to inspect where shifts landed.
          </p>
        </Disclosure>
      </Section>

      <Section title="Vocabulary">
        <Row label="Custom terms">
          <textarea
            value={s.custom_vocabulary}
            onChange={(e) => update("custom_vocabulary", e.target.value)}
            rows={3}
            placeholder="Tauri, Humla, ScreenCaptureKit, Granola"
            className={inputClass + " leading-relaxed"}
            style={{ fontFamily: "var(--font-mono)" }}
          />
          <p className="text-xs text-[var(--color-text-muted)] mt-2">
            Comma- or newline-separated. Names, jargon, and uncommon
            spellings — biases the transcriber toward these tokens.
            <code> gpt-4o-transcribe-diarize </code> ignores it.
          </p>
        </Row>
      </Section>
    </>
  );
}
