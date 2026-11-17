import { useState } from "react";
import { LANGUAGES, languageOptionLabel } from "../../../lib/languages";
import type { ProviderConfig, TranscribeConfig } from "../../../lib/ipc";
import { ProviderConfigForm } from "./ProviderConfigForm";
import { Select } from "../../../components/ui/Select";
import type { LocalState } from "../types";
import { ON_DEVICE_WHISPER, enforceLocalOnly, type PolicyRule } from "../../../lib/orgPolicy";
import { ManagedBadge } from "../ManagedRow";

// Section that lists existing per-language overrides and offers a + Add
// form to create new ones. Each entry is rendered as a card with a
// one-line summary plus a [×] delete button. No inline edit — to change
// an existing override, the user deletes and re-adds. Keeps the UI
// simple; revisit if friction surfaces.
export function PerLanguageOverrides({
  config,
  setLanguageOverride,
  removeLanguageOverride,
  local,
  managed = null,
  customBaseUrl = "",
}: {
  config: TranscribeConfig;
  setLanguageOverride: (language: string, cfg: ProviderConfig) => Promise<void>;
  removeLanguageOverride: (language: string) => Promise<void>;
  local: LocalState;
  /** Who fixes the provider (`local_only`): each override shows its enforced value. */
  managed?: PolicyRule | null;
  customBaseUrl?: string;
}) {
  const [showAddForm, setShowAddForm] = useState(false);
  const entries = Object.entries(config.per_language).sort(([a], [b]) =>
    a.localeCompare(b),
  );

  return (
    <div className="flex flex-col gap-3">
      {entries.length > 0 ? (
        <div className="flex flex-col gap-2">
          {entries.map(([language, cfg]) => (
            <OverrideRow
              key={language}
              language={language}
              cfg={managed ? enforceLocalOnly(cfg, customBaseUrl) : cfg}
              managed={managed}
              onDelete={() => removeLanguageOverride(language)}
            />
          ))}
        </div>
      ) : (
        <p className="text-xs text-[var(--color-text-muted)] italic">
          No overrides.
        </p>
      )}

      {!showAddForm && (
        <button
          type="button"
          onClick={() => setShowAddForm(true)}
          className="self-start text-sm px-3 py-1.5 rounded-md border border-[var(--color-line)] hover:bg-[var(--color-pill-hover)]"
        >
          + Add language override
        </button>
      )}
      {showAddForm && (
        <AddOverrideForm
          existingLanguages={Object.keys(config.per_language)}
          local={local}
          managed={managed}
          onCancel={() => setShowAddForm(false)}
          onAdd={async (language, cfg) => {
            await setLanguageOverride(language, cfg);
            setShowAddForm(false);
          }}
        />
      )}
    </div>
  );
}

function OverrideRow({
  language,
  cfg,
  managed,
  onDelete,
}: {
  language: string;
  cfg: ProviderConfig;
  managed: PolicyRule | null;
  onDelete: () => void;
}) {
  const lang = LANGUAGES.find((l) => l.value === language);
  return (
    <div className="flex items-start gap-3 px-3 py-2 rounded-md border border-[var(--color-line)]">
      <div className="flex-1 flex flex-col gap-0.5">
        <div className="flex flex-wrap items-center gap-x-2 text-sm font-medium">
          {lang ? languageOptionLabel(lang) : language}
          {managed && <ManagedBadge source={managed.source} reason={managed.reason} />}
        </div>
        <div className="text-xs text-[var(--color-text-muted)]">
          {summariseProvider(cfg)}
        </div>
      </div>
      <button
        type="button"
        onClick={onDelete}
        aria-label={`Remove ${language} override`}
        className="text-sm px-2 py-1 rounded hover:bg-[var(--color-pill-hover)]"
      >
        ×
      </button>
    </div>
  );
}

function summariseProvider(cfg: ProviderConfig): string {
  switch (cfg.provider) {
    case "openai":
      return `OpenAI · ${cfg.model}`;
    case "deepgram":
      return `Deepgram · ${cfg.model}`;
    case "groq":
      return `Groq · ${cfg.model}`;
    case "custom":
      return `Custom · ${cfg.model}`;
    case "local":
      return `Local · ${cfg.model_id} · ${cfg.preset} preset · GPU ${cfg.use_gpu ? "on" : "off"}`;
  }
}

function AddOverrideForm({
  existingLanguages,
  local,
  managed,
  onCancel,
  onAdd,
}: {
  existingLanguages: string[];
  local: LocalState;
  managed: PolicyRule | null;
  onCancel: () => void;
  onAdd: (language: string, cfg: ProviderConfig) => Promise<void>;
}) {
  // Filter the language picker to languages NOT already overridden.
  // Drop "auto" — overrides apply to specific recording languages, not
  // the auto-detect sentinel.
  const available = LANGUAGES.filter(
    (l) => l.value !== "auto" && !existingLanguages.includes(l.value),
  );
  const initialLanguage = available[0]?.value ?? "no";
  const [language, setLanguage] = useState(initialLanguage);
  const [cfg, setCfg] = useState<ProviderConfig>(
    managed ? ON_DEVICE_WHISPER : { provider: "openai", model: "whisper-1" },
  );

  return (
    <div className="flex flex-col gap-3 px-3 py-3 rounded-md border border-[var(--color-line)] bg-[var(--color-canvas)]">
      <div className="text-xs font-medium text-[var(--color-text-muted)]">
        New override
      </div>
      <Select
        value={language}
        onChange={setLanguage}
        options={available.map((l) => ({
          value: l.value,
          label: languageOptionLabel(l),
        }))}
      />
      <ProviderConfigForm
        value={cfg}
        onChange={setCfg}
        localModels={local.models}
        filterLocalToLanguage={language}
        managed={managed}
      />
      <div className="flex items-center gap-2">
        <button
          type="button"
          onClick={onCancel}
          className="text-sm px-3 py-1.5 rounded-md border border-[var(--color-line)] hover:bg-[var(--color-pill-hover)]"
        >
          Cancel
        </button>
        <button
          type="button"
          onClick={() => onAdd(language, cfg)}
          className="text-sm px-3 py-1.5 rounded-md bg-[var(--color-text)] text-[var(--color-canvas)]"
        >
          Add
        </button>
      </div>
    </div>
  );
}
