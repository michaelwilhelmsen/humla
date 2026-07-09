import { useThemeStore } from "../../../lib/theme";
import { usePaletteStore } from "../../../lib/palette";
import { LANGUAGES, languageOptionLabel } from "../../../lib/languages";
import { SUMMARY_PRESETS, presetLabel } from "../../../lib/presets";
import { Row, Section } from "../components/Section";
import { Segmented } from "../components/Segmented";
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
      <Section title="Defaults">
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
        <Row
          label="Summary preset"
          description='Which preset new notes start with. Each note can switch to a different preset (or "Custom") from its own header.'
          control={
            <Select
              value={s.default_summary_preset}
              onChange={(v) => update("default_summary_preset", v)}
              options={SUMMARY_PRESETS.map((p) => ({
                value: p.value,
                label: presetLabel(p),
              }))}
            />
          }
        />
      </Section>

      <Section title="Appearance">
        <Row
          label="Palette"
          description="Humla ships a single refined palette; the preference is kept for a future return of variants."
          control={
            <Segmented
              label="Palette"
              value={palette}
              onChange={setPalettePref}
              options={PALETTES}
            />
          }
        />
        <Row
          label="Theme"
          description="System follows macOS appearance."
          control={
            <Segmented
              label="Theme"
              value={theme}
              onChange={setThemePref}
              options={THEMES}
            />
          }
        />
      </Section>
    </>
  );
}
