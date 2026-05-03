import { ApiKeyField } from "../components/ApiKeyField";
import { Row, Section } from "../components/Section";
import type { SettingsHook } from "../useSettings";

export function ApiKeysTab({
  openaiKey,
  setOpenaiKey,
  saveKey,
  testKey,
}: Pick<SettingsHook, "openaiKey" | "setOpenaiKey" | "saveKey" | "testKey">) {
  return (
    <Section title="OpenAI">
      <Row label="API key">
        <ApiKeyField
          state={openaiKey}
          setState={setOpenaiKey}
          placeholder="sk-…"
          onSave={saveKey}
          onTest={testKey}
        />
        <p className="text-xs text-[var(--color-text-muted)] mt-2">
          Used for cloud transcription and cloud summarization when those
          providers are selected. Stored locally in the app's database; not
          sent anywhere except OpenAI.
        </p>
      </Row>
    </Section>
  );
}
