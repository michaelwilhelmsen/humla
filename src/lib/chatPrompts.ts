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

import type { ChatTarget } from "./chatTarget";

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

// Library-scoped prompts, for the `/chat` surface (issue #95). Deliberately
// cross-note: that is what a library-wide destination is FOR, and none of these
// can be answered from a single note's grounding — so they double as a statement
// of what this surface does that the Note's Chat tab cannot.
//
// "Client status" earns its place twice: it is the only hint that narrowing by
// client works at all, which is otherwise undiscoverable now that `/chat` has no
// scope picker (a dropdown whose only option is "All notes" would be noise, so
// narrowing stays a tool argument the model chooses — the tools-not-stuffing
// decision from #81). Its wording drops #82's original "…for a given client?":
// that carried a placeholder the user had to edit before sending, unlike the
// other three, which are one-click.
export const LIBRARY_PROMPTS: ChatPrompt[] = [
  {
    // Deliberately "what needs attention" rather than "list the open actions".
    // An enumerating prompt gets an enumeration back — every item, evenly
    // weighted; asking what needs attention makes the model filter, which is the
    // only useful behaviour once a library has months in it. The citation clause
    // rides on the prompt rather than the system message on purpose: terse system
    // prompts beat constraint-heavy ones on the small local models, which turn a
    // list of rules into a checklist they re-litigate while thinking.
    label: "Needs my attention",
    description: "Unresolved, blocked, or waiting on you",
    prompt:
      "Review my meetings from the last 30 days and tell me what needs my attention now — not everything that happened. Focus on unresolved commitments, blocked work, decisions waiting for me, deadlines, contradictions, and issues that keep coming back. Cite the meeting for each item.",
  },
  {
    label: "Weekly recap",
    description: "This week across your meetings",
    prompt: "Recap this week across my meetings.",
  },
  {
    label: "Client status",
    description: "Latest per client you've met",
    prompt: "What's the latest for each client I've met recently?",
  },
  {
    label: "Decisions log",
    description: "What was decided, and where",
    prompt: "What decisions were made recently, and where?",
  },
];

/** The prompt set a pane offers: note-scoped for a Note's Chat tab, library-wide
 *  for `/chat`. One switch on the target, so neither call site can pick the set
 *  that doesn't match what it can actually answer. */
export function promptsFor(target: ChatTarget): ChatPrompt[] {
  return target.kind === "global" ? LIBRARY_PROMPTS : NOTE_PROMPTS;
}

/**
 * Whether typing this key in the composer should open the prompt picker.
 *
 * Only on an empty input: a slash typed mid-sentence ("and/or", a file path) is
 * a literal character and must never be hijacked.
 */
export function opensPromptPicker(key: string, input: string): boolean {
  return key === "/" && input.length === 0;
}
