import { SettingsLayout } from "./settings/SettingsLayout";
import { GeneralTab } from "./settings/tabs/General";
import { TranscriptionTab } from "./settings/tabs/Transcription";
import { SummaryTab } from "./settings/tabs/Summary";
import { ApiKeysTab } from "./settings/tabs/ApiKeys";
import { AboutTab } from "./settings/tabs/About";
import { AccountTab } from "./settings/tabs/Account";
import { OrganizationTab } from "./settings/tabs/Organization";
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
          id: "account",
          label: "Account",
          content: <AccountTab />,
        },
        {
          id: "organization",
          label: "Organization",
          content: <OrganizationTab />,
        },
        {
          id: "transcription",
          label: "Transcription",
          content: (
            <TranscriptionTab
              s={settings.s}
              update={settings.update}
              transcribeConfig={settings.transcribeConfig}
              setDefaultConfig={settings.setDefaultConfig}
              setLanguageOverride={settings.setLanguageOverride}
              removeLanguageOverride={settings.removeLanguageOverride}
              local={settings.local}
              downloadModel={settings.downloadModel}
              deleteModel={settings.deleteModel}
              diarize={settings.diarize}
              downloadDiarize={settings.downloadDiarize}
              deleteDiarize={settings.deleteDiarize}
              sortformer={settings.sortformer}
              downloadSortformer={settings.downloadSortformer}
              deleteSortformer={settings.deleteSortformer}
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
              deepgramKey={settings.deepgramKey}
              setDeepgramKey={settings.setDeepgramKey}
              groqKey={settings.groqKey}
              setGroqKey={settings.setGroqKey}
              saveProviderKey={settings.saveProviderKey}
              testProviderKey={settings.testProviderKey}
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
