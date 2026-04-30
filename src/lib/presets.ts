// Summary presets — labels + the prompt text used by Settings to seed the
// global custom prompt and as a reference for the per-note picker.
//
// Keep these in sync with src-tauri/src/presets.rs. The backend resolves the
// prompt at request time from the note's stored preset value, so this file
// is mostly UI labeling + the "custom" path's seed text.

export type PresetSpec = {
  value: string;
  label_no: string;
  label_en: string;
  prompt_no: string;
  prompt_en: string;
};

// Minimal presets — two constraints only: kind of summary + language.
// Source labels stay because the user message uses them; the parenthetical
// "(brukerens)" / "(automatisk)" implicitly tells the model which to trust
// on conflict, so the explicit rule is dropped. No section lists, no "real
// content only" — the model picks a structure that fits the input.
// Voice memo carries one extra constraint ("Behold brukerens stemme") because
// preserving tone is the *purpose* of that preset. Keep these in sync with
// src-tauri/src/presets.rs.
export const SUMMARY_PRESETS: PresetSpec[] = [
  {
    value: "meeting",
    label_no: "Møte",
    label_en: "Meeting",
    prompt_no: `Du lager møtenotater fra [Notater] (brukerens) og [Transkripsjon] (automatisk). Skriv på norsk i Markdown.`,
    prompt_en: `You produce meeting notes from [Notater] (user-written) and [Transkripsjon] (auto). Reply in {LANGUAGE} using Markdown.`,
  },
  {
    value: "one_on_one",
    label_no: "1:1-samtale",
    label_en: "1:1 conversation",
    prompt_no: `Du lager notater fra en 1:1-samtale fra [Notater] (brukerens) og [Transkripsjon] (automatisk). Skriv på norsk i Markdown.`,
    prompt_en: `You produce 1:1 notes from [Notater] (user-written) and [Transkripsjon] (auto). Reply in {LANGUAGE} using Markdown.`,
  },
  {
    value: "lecture",
    label_no: "Foredrag",
    label_en: "Lecture",
    prompt_no: `Du lager studienotater fra [Notater] (brukerens) og [Transkripsjon] (automatisk). Skriv på norsk i Markdown.`,
    prompt_en: `You produce study notes from [Notater] (user-written) and [Transkripsjon] (auto). Reply in {LANGUAGE} using Markdown.`,
  },
  {
    value: "interview",
    label_no: "Intervju",
    label_en: "Interview",
    prompt_no: `Du lager intervjunotater fra [Notater] (brukerens) og [Transkripsjon] (automatisk). Skriv på norsk i Markdown.`,
    prompt_en: `You produce interview notes from [Notater] (user-written) and [Transkripsjon] (auto). Reply in {LANGUAGE} using Markdown.`,
  },
  {
    value: "brainstorm",
    label_no: "Idémyldring",
    label_en: "Brainstorm",
    prompt_no: `Du oppsummerer en idémyldring fra [Notater] (brukerens) og [Transkripsjon] (automatisk). Skriv på norsk i Markdown.`,
    prompt_en: `You summarize a brainstorm from [Notater] (user-written) and [Transkripsjon] (auto). Reply in {LANGUAGE} using Markdown.`,
  },
  {
    value: "voice_memo",
    label_no: "Stemmenotat",
    label_en: "Voice memo",
    prompt_no: `Du rydder opp i et stemmenotat fra [Notater] (brukerens) og [Transkripsjon] (automatisk). Behold brukerens stemme. Skriv på norsk i Markdown.`,
    prompt_en: `You clean up a voice memo from [Notater] (user-written) and [Transkripsjon] (auto). Preserve the user's voice. Reply in {LANGUAGE} using Markdown.`,
  },
];

const LANGUAGE_LABELS: Record<string, string> = {
  no: "Norwegian",
  en: "English",
  sv: "Swedish",
  da: "Danish",
  auto: "the same language as the input",
};

export function presetPromptForLang(preset: PresetSpec, lang: string): string {
  if (lang === "no") return preset.prompt_no;
  const label = LANGUAGE_LABELS[lang] ?? "English";
  return preset.prompt_en.replace("{LANGUAGE}", label);
}

export function presetLabelForLang(preset: PresetSpec, lang: string): string {
  return lang === "no" ? preset.label_no : preset.label_en;
}

export function presetByValue(value: string): PresetSpec | undefined {
  return SUMMARY_PRESETS.find((p) => p.value === value);
}
