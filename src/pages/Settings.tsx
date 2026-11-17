import type { ReactNode } from "react";
import { useSearchParams } from "react-router-dom";
import { SettingsLayout } from "./settings/SettingsLayout";
import { SECTIONS, type SectionId } from "./settings/sections";
import { GeneralTab } from "./settings/tabs/General";
import { IntegrationsSection } from "./settings/tabs/Integrations";
import { RecordingSection } from "./settings/tabs/Recording";
import { TranscriptionTab } from "./settings/tabs/Transcription";
import { SummaryTab } from "./settings/tabs/Summary";
import { TemplatesSection } from "./settings/tabs/Templates";
import { VocabularySection } from "./settings/tabs/Vocabulary";
import { ChatTab } from "./settings/tabs/Chat";
import { AboutTab } from "./settings/tabs/About";
import { AccountTab, PoliciesSection } from "./settings/tabs/Account";
import { OrganizationTab } from "./settings/tabs/Organization";
import { useSettings } from "./settings/useSettings";

// Section registry: maps the grouped IA (see sections.ts) to content.
export function Settings() {
  const settings = useSettings();
  const [, setParams] = useSearchParams();

  const content: Record<SectionId, ReactNode> = {
    templates: <TemplatesSection language={settings.s.language} />,
    vocabulary: <VocabularySection s={settings.s} update={settings.update} />,
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
        />
      </>
    ),
    summaries: (
      <SummaryTab
        s={settings.s}
        update={settings.update}
        onOpenTemplates={() => setParams({ tab: "templates" }, { replace: true })}
      />
    ),
    chat: <ChatTab s={settings.s} update={settings.update} />,
    account: (
      <>
        <AccountTab s={settings.s} update={settings.update} />
        <PoliciesSection />
        <OrganizationTab />
      </>
    ),
    general: (
      <>
        <GeneralTab />
        {/* The integrations (MCP, the webhook) live here rather than in their
            own section: each is a switch plus what it needs once on. */}
        <IntegrationsSection s={settings.s} update={settings.update} />
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
