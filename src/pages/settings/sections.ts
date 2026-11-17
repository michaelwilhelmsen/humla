// The settings dialog's information architecture: sections in three groups.
// `?tab=` in the URL stays the source of truth; legacy tab ids from old deep
// links resolve to their new home so none of them 404.

import {
  AudioLines,
  BookA,
  LayoutTemplate,
  Mic,
  MessageCircle,
  Settings2,
  Sparkles,
  UserRound,
  type LucideIcon,
} from "lucide-react";

export type SectionId =
  | "templates"
  | "vocabulary"
  | "recording"
  | "transcription"
  | "summaries"
  | "chat"
  | "account"
  | "general";

export type SectionSpec = { id: SectionId; label: string; icon: LucideIcon };

export const SECTION_GROUPS: { label: string; sections: SectionSpec[] }[] = [
  {
    label: "Content",
    sections: [
      { id: "templates", label: "Templates", icon: LayoutTemplate },
      { id: "vocabulary", label: "Vocabulary", icon: BookA },
    ],
  },
  {
    label: "Setup",
    sections: [
      { id: "recording", label: "Recording", icon: Mic },
      { id: "transcription", label: "Transcription", icon: AudioLines },
      { id: "summaries", label: "Summaries", icon: Sparkles },
      { id: "chat", label: "Chat", icon: MessageCircle },
    ],
  },
  {
    label: "Account",
    sections: [
      { id: "account", label: "Account", icon: UserRound },
      { id: "general", label: "General", icon: Settings2 },
    ],
  },
];

export const SECTIONS: SectionSpec[] = SECTION_GROUPS.flatMap((g) => g.sections);

const LEGACY_TAB_MAP: Record<string, SectionId> = {
  about: "general",
  organization: "account",
  keys: "transcription",
  summary: "summaries",
  prompts: "templates",
  "summary-prompts": "templates",
  "custom-vocabulary": "vocabulary",
};

export function resolveSectionId(tab: string | null): SectionId {
  if (tab && SECTIONS.some((s) => s.id === tab)) return tab as SectionId;
  if (tab && tab in LEGACY_TAB_MAP) return LEGACY_TAB_MAP[tab];
  return SECTIONS[0].id;
}
