import { useThemeStore } from "../../../lib/theme";
import { usePaletteStore } from "../../../lib/palette";
import { Permissions } from "../../../components/Permissions";
import { LANGUAGES, languageOptionLabel } from "../../../lib/languages";
import { SUMMARY_PRESETS, presetLabel } from "../../../lib/presets";
import { Row, Section } from "../components/Section";
import { Select } from "../components/Select";
import { PALETTES, THEMES } from "../types";
import type { SettingsHook } from "../useSettings";

export function GeneralTab({ s, update }: Pick<SettingsHook, "s" | "update">) {
  const theme = useThemeStore((t) => t.theme);
  const setThemePref = useThemeStore((t) => t.setTheme);
  const palette = usePaletteStore((p) => p.palette);
  const setPalettePref = usePaletteStore((p) => p.setPalette);

  return (
    <>
      <Section title="Permissions">
        <Permissions />
      </Section>

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
              label: presetLabel(p),
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
        <Row label="Palette">
          <div className="flex gap-1 p-1 rounded-md border border-[var(--color-line-visible)] bg-[var(--color-surface)] w-fit">
            {PALETTES.map((p) => (
              <button
                key={p.value}
                onClick={() => setPalettePref(p.value)}
                className={
                  "px-3 py-1 rounded text-sm " +
                  (palette === p.value
                    ? "bg-[var(--color-pill-hover)] text-[var(--color-text)]"
                    : "text-[var(--color-text-muted)] hover:text-[var(--color-text)]")
                }
              >
                {p.label}
              </button>
            ))}
          </div>
          <p className="text-xs text-[var(--color-text-muted)] mt-2">
            {PALETTES.find((p) => p.value === palette)?.description}
          </p>
        </Row>
        <Row label="Theme">
          <div className="flex gap-1 p-1 rounded-md border border-[var(--color-line-visible)] bg-[var(--color-surface)] w-fit">
            {THEMES.map((t) => (
              <button
                key={t.value}
                onClick={() => setThemePref(t.value)}
                className={
                  "px-3 py-1 rounded text-sm " +
                  (theme === t.value
                    ? "bg-[var(--color-pill-hover)] text-[var(--color-text)]"
                    : "text-[var(--color-text-muted)] hover:text-[var(--color-text)]")
                }
              >
                {t.label}
              </button>
            ))}
          </div>
        </Row>
      </Section>
    </>
  );
}
