import { SettingsLayout } from "./settings/SettingsLayout";
import { GeneralTab } from "./settings/tabs/General";
import { TranscriptionTab } from "./settings/tabs/Transcription";
import { SummaryTab } from "./settings/tabs/Summary";
import { ApiKeysTab } from "./settings/tabs/ApiKeys";
import { AboutTab } from "./settings/tabs/About";
import { useSettings } from "./settings/useSettings";

export function Settings() {
  const settings = useSettings();
  return (
    <SettingsLayout
      defaultTabId="general"
      tabs={[
        {
          id: "general",
          label: "General",
          content: <GeneralTab s={settings.s} update={settings.update} />,
        },
        {
          id: "transcription",
          label: "Transcription",
          content: (
            <TranscriptionTab
              s={settings.s}
              update={settings.update}
              local={settings.local}
              downloadModel={settings.downloadModel}
              deleteModel={settings.deleteModel}
              diarize={settings.diarize}
              downloadDiarize={settings.downloadDiarize}
              deleteDiarize={settings.deleteDiarize}
            />
          ),
        },
        {
          id: "summary",
          label: "AI Summary",
          content: (
            <SummaryTab
              s={settings.s}
              update={settings.update}
              llmModels={settings.llmModels}
              refreshLlmModels={settings.refreshLlmModels}
            />
          ),
        },
        {
          id: "keys",
          label: "API keys",
          content: (
            <ApiKeysTab
              openaiKey={settings.openaiKey}
              setOpenaiKey={settings.setOpenaiKey}
              saveKey={settings.saveKey}
              testKey={settings.testKey}
            />
          ),
        },
        {
          id: "about",
          label: "About",
          content: <AboutTab />,
        },
      ]}
    />
  );
}
