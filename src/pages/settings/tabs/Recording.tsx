import { Permissions } from "../../../components/Permissions";
import { Row, Section } from "../components/Section";
import { Toggle } from "../components/Toggle";
import type { SettingsHook } from "../useSettings";

// Recording section: capture permissions + audio retention. Menu-bar mode
// and the global hotkey land here when that feature ships.
export function RecordingSection({
  s,
  update,
}: Pick<SettingsHook, "s" | "update">) {
  return (
    <>
      <Section title="Permissions">
        <div className="py-3.5">
          <Permissions />
        </div>
      </Section>

      <Section title="Audio retention">
        <Row
          label="Keep recorded audio"
          description="Save the mic and system WAV files after a recording stops, so you can listen back or re-run speaker detection. Roughly 1 MB per minute per channel; off keeps nothing after post-processing."
          control={
            <Toggle
              label="Keep recorded audio"
              checked={s.keep_audio === "true"}
              onChange={(on) => update("keep_audio", on ? "true" : "false")}
            />
          }
        />
      </Section>
    </>
  );
}
