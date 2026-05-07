import type { ProviderConfig, LocalWhisperModelStatus } from "../../../lib/ipc";
import { LANGUAGES } from "../../../lib/languages";
import type { LocalState } from "../types";
import { Btn } from "./Btn";
import { formatBytes } from "./format";

function languageLabel(code: string | null): string {
  if (!code) return "Unknown";
  const found = LANGUAGES.find((l) => l.value === code);
  return found?.label ?? code;
}

export function LocalModelManager({
  state,
  activeId,
  onDownload,
  onDelete,
  onSelect,
  setLanguageOverride,
}: {
  state: LocalState;
  activeId: string;
  // `language` is no longer consumed by the model list (Phase 4 dropped
  // the auto-route addon mechanism). The prop signature stays the same
  // so the parent doesn't need to change call shape; we just don't read
  // it. Future re-introduction of language-aware UI hints can pick it
  // back up.
  language?: string;
  onDownload: (id: string) => void;
  onDelete: (id: string) => void;
  onSelect: (id: string) => void;
  // Used by the suggest_language_override flash affordance after a
  // language-specific model is downloaded.
  setLanguageOverride: (language: string, cfg: ProviderConfig) => Promise<void>;
}) {
  // One flat list — both kinds rendered together with their language tag.
  // Multilingual models get the radio button (they're candidates for the
  // default's model_id); language-specific models don't (they're picked
  // via per-language overrides, not via this radio).
  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-col gap-3">
        <p className="text-xs text-[var(--color-text-muted)]">
          Pick a multilingual model as the default for transcription. Language-
          specific models (e.g. NB Whisper for Norwegian) sit alongside but
          are picked via per-language overrides above. All models run on-
          device via Metal.
        </p>
        {state.models.map((m) => (
          <ModelRow
            key={m.id}
            model={m}
            progress={state.downloading[m.id]}
            isActive={m.kind === "multilingual" && m.id === activeId}
            showRadio={m.kind === "multilingual"}
            onDownload={onDownload}
            onDelete={onDelete}
            onSelect={onSelect}
          />
        ))}
      </div>
      {state.flash && (
        <div
          className="flex items-center gap-3 px-3 py-2 rounded bg-[var(--color-pill-hover)] text-xs"
          role="status"
        >
          <span className="break-all">{state.flash.message}</span>
          {state.flash.kind === "suggest_language_override" && (
            <button
              type="button"
              onClick={() => {
                if (state.flash?.kind !== "suggest_language_override") return;
                setLanguageOverride(state.flash.language, {
                  provider: "local",
                  model_id: state.flash.modelId,
                  preset: "quality",
                  use_gpu: true,
                });
              }}
              className="ml-auto text-xs px-2 py-1 rounded border border-[var(--color-line)] hover:bg-[var(--color-canvas)] whitespace-nowrap"
            >
              Add as {languageLabel(state.flash.language)} override
            </button>
          )}
        </div>
      )}
      {state.error && (
        <p className="text-sm text-red-600 dark:text-red-400 break-all">
          {state.error}
        </p>
      )}
    </div>
  );
}

function ModelRow({
  model,
  progress,
  isActive,
  showRadio,
  onDownload,
  onDelete,
  onSelect,
}: {
  model: LocalWhisperModelStatus;
  progress: { received: number; total: number | null } | undefined;
  isActive: boolean;
  showRadio: boolean;
  onDownload: (id: string) => void;
  onDelete: (id: string) => void;
  onSelect: (id: string) => void;
}) {
  const tagLabel =
    model.kind === "multilingual"
      ? "Multilingual"
      : languageLabel(model.specificLanguage);
  return (
    <div className="flex flex-col gap-2 px-3 py-2 rounded-md border border-[var(--color-line)]">
      <div className="flex items-start gap-2">
        {showRadio ? (
          <input
            type="radio"
            name="local_whisper_model"
            checked={isActive}
            disabled={!model.downloaded}
            onChange={() => onSelect(model.id)}
            className="mt-1"
            aria-label={`Use ${model.label}`}
          />
        ) : (
          <span className="mt-1 w-3.5" aria-hidden />
        )}
        <div className="flex-1 flex flex-col gap-0.5">
          <div className="flex items-center gap-2 text-sm">
            <span className="font-medium">{model.label}</span>
            <span className="text-xs px-1.5 py-0.5 rounded bg-[var(--color-pill-hover)] text-[var(--color-text-muted)]">
              {tagLabel}
            </span>
            {isActive && model.downloaded && (
              <span className="text-xs text-[var(--color-text-muted)]">
                · active
              </span>
            )}
          </div>
          <p className="text-xs text-[var(--color-text-muted)]">
            {model.description}
          </p>
          <p className="text-xs text-[var(--color-text-muted)]">
            {model.downloaded
              ? `Downloaded${
                  model.sizeBytes ? ` · ${formatBytes(model.sizeBytes)}` : ""
                }`
              : `Not downloaded · ~${formatBytes(model.sizeBytesHint)}`}
          </p>
        </div>
        <div className="flex gap-2">
          {!model.downloaded && !progress && (
            <Btn onClick={() => onDownload(model.id)}>Download</Btn>
          )}
          {model.downloaded && !progress && (
            <Btn onClick={() => onDelete(model.id)}>Delete</Btn>
          )}
        </div>
      </div>
      {progress && (
        <div className="flex flex-col gap-1 mt-1">
          <div className="text-xs text-[var(--color-text-muted)]">
            Downloading
            {progress.total
              ? ` ${formatBytes(progress.received)} / ${formatBytes(progress.total)}`
              : ` ${formatBytes(progress.received)}`}
            …
          </div>
          <div className="h-1 rounded bg-[var(--color-pill-hover)] overflow-hidden">
            <div
              className="h-full bg-[var(--color-text-muted)] transition-[width] duration-150"
              style={{
                width:
                  progress.total === null
                    ? "30%"
                    : `${Math.min(100, (progress.received / progress.total) * 100)}%`,
              }}
            />
          </div>
        </div>
      )}
    </div>
  );
}
