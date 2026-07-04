// Step 3 — Meeting language. Writes the existing global `language` setting
// (write-through on change, no final commit). Its real job is upstream: the
// answer drives the step-4 transcription-model recommendation (a later work
// package). Mirrors the option list + setting key used by Settings → General.
//
// This is the *meeting* language, never the UI language — see
// design/ONBOARDING.md § Copy.
import { useEffect, useState } from "react";
import { Languages } from "lucide-react";
import { ipc } from "../../../lib/ipc";
import { LANGUAGES, languageOptionLabel } from "../../../lib/languages";
import type { StepContext } from "../types";
import { StepShell } from "../StepShell";

// Same default as Settings → General (`DEFAULTS.language`). Kept inline rather
// than imported from the Settings types module to avoid coupling onboarding to
// the Settings page's internal shape.
const DEFAULT_LANGUAGE = "no";

export function LanguageStep({ ctx }: { ctx: StepContext }) {
  const [value, setValue] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    ipc
      .getSetting("language")
      .then((v) => {
        if (!cancelled) setValue(v ?? DEFAULT_LANGUAGE);
      })
      .catch(() => {
        if (!cancelled) setValue(DEFAULT_LANGUAGE);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  async function change(next: string) {
    setValue(next); // optimistic
    try {
      await ipc.setSetting("language", next);
    } catch (e) {
      console.warn("[onboarding] failed to save language:", e);
    }
  }

  return (
    <StepShell
      icon={<Languages size={26} strokeWidth={1.6} />}
      title="What language are your meetings mostly in?"
      subtitle="This helps Humla pick the right transcription model. You can override it per note later."
    >
      <label className="w-full max-w-xs">
        <span className="sr-only">Meeting language</span>
        <select
          value={value ?? DEFAULT_LANGUAGE}
          disabled={value === null}
          onChange={(e) => change(e.target.value)}
          className="w-full px-3 py-2.5 rounded-md text-sm bg-[var(--color-input-bg)] border border-[var(--color-line)] focus:border-[var(--color-text-muted)]"
        >
          {LANGUAGES.map((l) => (
            <option key={l.value} value={l.value}>
              {languageOptionLabel(l)}
            </option>
          ))}
        </select>
      </label>

      <div className="mt-8">
        <button
          type="button"
          className="nd-btn nd-btn-primary"
          disabled={value === null}
          onClick={ctx.goNext}
        >
          Continue
        </button>
      </div>
    </StepShell>
  );
}
