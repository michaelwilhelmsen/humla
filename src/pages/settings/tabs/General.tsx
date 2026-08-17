import { useEffect, useState } from "react";
import { ipc } from "../../../lib/ipc";
import { Toggle } from "../components/Toggle";
import { useThemeStore } from "../../../lib/theme";
import { PALETTE_REGISTRY, usePaletteStore } from "../../../lib/palette";
import { Row, Section } from "../components/Section";
import { Segmented } from "../components/Segmented";
import { PALETTES, THEMES } from "../types";

/** Anonymous usage counters. Owns its own read/write rather than going through the
 *  settings map, because UNSET is a real third state here: it means the install
 *  predates the feature and was never enrolled. Both unset and "false" render as off,
 *  but only an explicit "true" ever sends — so an older install stays silent until
 *  the person reading this row turns it on. */
function UsageCountersRow() {
  const [on, setOn] = useState<boolean | null>(null);

  useEffect(() => {
    let cancelled = false;
    ipc
      .getSetting("telemetry_enabled")
      .then((v) => {
        if (!cancelled) setOn(v === "true");
      })
      .catch(() => {
        if (!cancelled) setOn(false);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  return (
    <Row
      label="Send anonymous usage counters"
      description="Counts first launch and whether you finished setup. No identifier, no account, nothing about your notes or recordings — they are stored as plain daily tallies."
      control={
        <Toggle
          checked={on === true}
          onChange={(next) => {
            setOn(next);
            void ipc.setSetting("telemetry_enabled", next ? "true" : "false");
          }}
          label="Send anonymous usage counters"
        />
      }
    />
  );
}

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
            design
              ? `${design.description} Each design has its own light and dark mode.`
              : "Typeface, type scale and colours. Each design has its own light and dark mode."
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
        <UsageCountersRow />
      </Section>
    </>
  );
}
