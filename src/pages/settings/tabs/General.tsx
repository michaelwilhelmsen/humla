import { useThemeStore } from "../../../lib/theme";
import { Permissions } from "../../../components/Permissions";
import { LANGUAGES, languageOptionLabel } from "../../../lib/languages";
import { SUMMARY_PRESETS, presetLabelForLang } from "../../../lib/presets";
import { Row, Section } from "../components/Section";
import { Select } from "../components/Select";
import { THEMES } from "../types";
import type { SettingsHook } from "../useSettings";

export function GeneralTab({ s, update }: Pick<SettingsHook, "s" | "update">) {
  const theme = useThemeStore((t) => t.theme);
  const setThemePref = useThemeStore((t) => t.setTheme);

  return (
    <>
      <Section title="Defaults">
        <Row label="Language">
          <Select
            value={s.language}
            onChange={(v) => update("language", v)}
            options={LANGUAGES.map((l) => ({ value: l.value, label: languageOptionLabel(l) }))}
          />
          <p className="text-xs text-[var(--color-text-muted)] mt-2">
            Default for new notes. Each note has its own language chip in
            the header that overrides this.
          </p>
        </Row>
        <Row label="Summary preset">
          <Select
            value={s.default_summary_preset}
            onChange={(v) => update("default_summary_preset", v)}
            options={SUMMARY_PRESETS.map((p) => ({
              value: p.value,
              label: presetLabelForLang(p, s.language),
            }))}
          />
          <p className="text-xs text-[var(--color-text-muted)] mt-2">
            Which preset new notes start with. Each note can switch to a
            different preset (or "Custom") from its own header. Existing
            notes are unaffected.
          </p>
        </Row>
      </Section>

      <Section title="Appearance">
        <Row label="Theme">
          <div className="flex gap-1 p-1 rounded-md border border-[var(--color-line-visible)] bg-[var(--color-surface)] w-fit">
            {THEMES.map((t) => (
              <button
                key={t.value}
                onClick={() => setThemePref(t.value)}
                className={
                  "px-3 py-1 rounded text-sm " +
                  (theme === t.value
                    ? "bg-[var(--color-surface)] shadow-sm"
                    : "text-[var(--color-text-muted)] hover:text-[var(--color-text)]")
                }
              >
                {t.label}
              </button>
            ))}
          </div>
        </Row>
      </Section>

      <Section title="Permissions">
        <Permissions />
      </Section>
    </>
  );
}
