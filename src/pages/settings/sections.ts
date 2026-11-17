// The settings dialog's information architecture: five plain-language
// sections replacing the seven flat tabs. `?tab=` in the URL stays the
// source of truth; legacy tab ids from old deep links resolve to their
// new home so none of them 404.

import {
  AudioLines,
  Mic,
  MessageCircle,
  Settings2,
  Sparkles,
  UserRound,
  type LucideIcon,
} from "lucide-react";

export type SectionId =
  | "recording"
  | "transcription"
  | "summaries"
  | "chat"
  | "account"
  | "general";

export const SECTIONS: { id: SectionId; label: string; icon: LucideIcon }[] = [
  { id: "recording", label: "Recording", icon: Mic },
  { id: "transcription", label: "Transcription", icon: AudioLines },
  { id: "summaries", label: "Summaries", icon: Sparkles },
  { id: "chat", label: "Chat", icon: MessageCircle },
  { id: "account", label: "Account", icon: UserRound },
  { id: "general", label: "General", icon: Settings2 },
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
