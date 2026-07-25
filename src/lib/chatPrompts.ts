// Canned chat prompts, surfaced from the composer by typing "/" (issue #80).
//
// Their job is discovery, not power: a blank input tells a first-time user
// nothing about what chat can do, and these four show it in one glance. Static
// on purpose — editable prompts would need a settings section, CRUD and sync for
// a feature whose whole value is the hint, and anyone wanting something bespoke
// just types it.
//
// Deliberately called "Prompts": "Skills" oversells a canned string, and
// "Presets" already means summary styles (`summary_preset` — Meeting / 1:1 /
// Lecture), so reusing it would be ambiguous in Settings and in conversation.

export type ChatPrompt = {
  /** Shown in the popover row. */
  label: string;
  /** One-line explanation under the label. */
  description: string;
  /** What actually gets sent. */
  prompt: string;
};

// Note-scoped prompts, for the Chat tab of a Note. Answerable from the anchor
// note's own grounding, so they work without any retrieval luck.
export const NOTE_PROMPTS: ChatPrompt[] = [
  {
    label: "Key decisions",
    description: "What this meeting actually settled",
    prompt: "What was decided in this meeting?",
  },
  {
    label: "Action items",
    description: "Who agreed to do what",
    prompt: "What action items came out of this, and who owns them?",
  },
  {
    label: "What I missed",
    description: "Discussed out loud but not in your notes",
    prompt: "Summarise anything discussed that isn't in my typed notes.",
  },
  {
    label: "Open questions",
    description: "Raised but left unresolved",
    prompt: "What was raised but left unresolved?",
  },
];

/**
 * Whether typing this key in the composer should open the prompt picker.
 *
 * Only on an empty input: a slash typed mid-sentence ("and/or", a file path) is
 * a literal character and must never be hijacked.
 */
export function opensPromptPicker(key: string, input: string): boolean {
  return key === "/" && input.length === 0;
}
