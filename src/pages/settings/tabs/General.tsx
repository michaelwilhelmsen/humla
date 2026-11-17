import { useThemeStore } from "../../../lib/theme";
import { PALETTE_REGISTRY, usePaletteStore } from "../../../lib/palette";
import { Row, Section } from "../components/Section";
import { Segmented } from "../components/Segmented";
import { PALETTES, THEMES } from "../types";

// General owns appearance + the absorbed About content. Defaults moved to
// their sections (#15): language → Transcription, preset → Summaries.
export function GeneralTab() {
  const theme = useThemeStore((t) => t.theme);
  const setThemePref = useThemeStore((t) => t.setTheme);
  const palette = usePaletteStore((p) => p.palette);
  const setPalettePref = usePaletteStore((p) => p.setPalette);

  // The description is the selected design's own, so the row explains what is
  // actually on screen instead of describing the picker.
  const design = PALETTE_REGISTRY.find((p) => p.id === palette);

  return (
    <>
      <Section title="Appearance">
        <Row
          label="Design"
          description={
            design ? design.description : "Typeface, type scale and colours."
          }
          control={
            <Segmented
              label="Design"
              value={palette}
              onChange={setPalettePref}
              options={PALETTES}
            />
          }
        />
        <Row
          label="Theme"
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
