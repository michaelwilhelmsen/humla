import type { ReactNode } from "react";
import { SettingsLayout } from "./settings/SettingsLayout";
import { SECTIONS, type SectionId } from "./settings/sections";
import { GeneralTab } from "./settings/tabs/General";
import { RecordingSection } from "./settings/tabs/Recording";
import { TranscriptionTab } from "./settings/tabs/Transcription";
import { SummaryTab } from "./settings/tabs/Summary";
import { ChatTab } from "./settings/tabs/Chat";
import { AboutTab } from "./settings/tabs/About";
import { AccountTab } from "./settings/tabs/Account";
import { OrganizationTab } from "./settings/tabs/Organization";
import { useSettings } from "./settings/useSettings";

// Section registry: maps the 5-section IA (see sections.ts) to content.
// Transcription and Summaries render their pre-refactor tab bodies (plus
// the old API-keys fields under Transcription) until PRD 2/2 rebuilds
// them; the keys live inline under providers from then on.
export function Settings() {
  const settings = useSettings();

  const content: Record<SectionId, ReactNode> = {
    recording: <RecordingSection s={settings.s} update={settings.update} />,
    transcription: (
      <>
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
      </>
    ),
    summaries: <SummaryTab s={settings.s} update={settings.update} />,
    chat: <ChatTab s={settings.s} update={settings.update} />,
    account: (
      <>
        <AccountTab s={settings.s} update={settings.update} />
        <OrganizationTab />
      </>
    ),
    general: (
      <>
        <GeneralTab />
        <AboutTab />
      </>
    ),
  };

  return (
    <SettingsLayout
      sections={SECTIONS.map((s) => ({ ...s, content: content[s.id] }))}
    />
  );
}
