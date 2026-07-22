// The settings dialog's information architecture: five plain-language
// sections replacing the seven flat tabs. `?tab=` in the URL stays the
// source of truth; legacy tab ids from old deep links resolve to their
// new home so none of them 404.

export type SectionId =
  | "recording"
  | "transcription"
  | "summaries"
  | "chat"
  | "account"
  | "general";

export const SECTIONS: { id: SectionId; label: string }[] = [
  { id: "recording", label: "Recording" },
  { id: "transcription", label: "Transcription" },
  { id: "summaries", label: "Summaries" },
  { id: "chat", label: "Chat" },
  { id: "account", label: "Account" },
  { id: "general", label: "General" },
];

const LEGACY_TAB_MAP: Record<string, SectionId> = {
  about: "general",
  organization: "account",
  keys: "transcription",
  summary: "summaries",
};

export function resolveSectionId(tab: string | null): SectionId {
  if (tab && SECTIONS.some((s) => s.id === tab)) return tab as SectionId;
  if (tab && tab in LEGACY_TAB_MAP) return LEGACY_TAB_MAP[tab];
  return SECTIONS[0].id;
}
