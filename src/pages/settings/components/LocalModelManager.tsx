import type { ProviderConfig } from "../../../lib/ipc";
import { LANGUAGES } from "../../../lib/languages";
import type { LocalState } from "../types";
import { ModelDownloadCard } from "../../../components/provider/ModelDownloadCard";

function languageLabel(code: string | null): string {
  if (!code) return "Unknown";
  const found = LANGUAGES.find((l) => l.value === code);
  return found?.label ?? code;
}

// The settings model list: one ModelDownloadCard per registry entry.
// Multilingual models carry the default-model radio (they're candidates for
// the default's model_id); language-specific models don't — they're picked
// via per-language overrides. Download/delete route through the parent's
// orchestrating handlers (auto-default promotion + suggest-override flash
// live in useSettings, not here).
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
  // Kept for call-shape compatibility; the list no longer reads it.
  language?: string;
  onDownload: (id: string) => void;
  onDelete: (id: string) => void;
  onSelect: (id: string) => void;
  // Used by the suggest_language_override flash affordance after a
  // language-specific model is downloaded.
  setLanguageOverride: (language: string, cfg: ProviderConfig) => Promise<void>;
}) {
  return (
    <>
      <p className="text-xs text-[var(--color-text-muted)] py-3">
        Pick a multilingual model as the default for transcription. Language-
        specific models (e.g. NB Whisper for Norwegian) sit alongside but are
        picked via per-language overrides. All models run on-device via Metal.
      </p>
      {state.models.map((m) => {
        const selectable = m.kind === "multilingual";
        const isActive = selectable && m.id === activeId;
        return (
          <ModelDownloadCard
            key={m.id}
            model={m}
            tag={
              m.kind === "multilingual"
                ? "Multilingual"
                : languageLabel(m.specificLanguage)
            }
            selected={isActive}
            onSelect={selectable ? () => onSelect(m.id) : undefined}
            onDownload={onDownload}
            onDelete={onDelete}
            warning={
              // The default's model_id can point at a model that isn't on
              // disk (fresh installs; onboarding skips). Recording with the
              // local provider won't start in that state — say so here,
              // where the user is already looking, instead of letting
              // Record fail.
              isActive && !m.downloaded
                ? "Selected as the default but not downloaded — recording won't start until you download it."
                : undefined
            }
          />
        );
      })}
      {state.flash && (
        <div
          className="flex items-center gap-3 px-3 py-2 my-3 rounded bg-[var(--color-pill-hover)] text-xs"
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
        <p className="text-sm text-[var(--color-danger)] break-all py-3">
          {state.error}
        </p>
      )}
    </>
  );
}
