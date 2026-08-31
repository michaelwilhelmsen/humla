import { type Note } from "./ipc";
import { stripSpeakerLabels } from "./speakers";

// Shared helpers for the note-list views (All notes, Folder).

// A note's date as the card shows it: weekday and date, plus the clock time.
// The card carries the whole date because the grid has no date headings to
// lean on — it is the only temporal anchor a note gets.
export function formatNoteDate(ts: number): string {
  const d = new Date(ts);
  const sameYear = d.getFullYear() === new Date().getFullYear();
  const date = d.toLocaleDateString(undefined, {
    weekday: "short",
    day: "numeric",
    month: "short",
    ...(sameYear ? {} : { year: "numeric" }),
  });
  const time = d.toLocaleTimeString(undefined, {
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  });
  return `${date} · ${time}`;
}

// Plain text behind Tiptap's HTML, via a detached element (textContent only —
// never inserted live, so no script runs). The backend does this properly with
// `html_text::html_to_text` when it builds a prompt; this is the client's
// version, for display and for the body text chat is grounded on.
//
// Block ends become a space first: textContent alone butts one paragraph
// against the next ("dyrere.Kanskje"), which a multi-line excerpt shows off
// and which reaches the chat model as a run-on word.
export function htmlToText(html: string | null | undefined): string {
  const trimmed = html?.trim();
  if (!trimmed) return "";
  const el = document.createElement("div");
  el.innerHTML = trimmed
    .replace(/<br\s*\/?>/gi, " ")
    .replace(/<\/(p|div|li|h[1-6]|blockquote|tr)>/gi, "$& ");
  return el.textContent || "";
}

// Floor under an excerpt candidate: anything shorter is a fragment
// ("— Begge") that tells the card nothing.
const EXCERPT_MIN_CHARS = 20;

// Letters or digits. A line without them (---, ***, lone punctuation) is
// structure, not content.
const SUBSTANCE = /[\p{L}\p{N}]/u;

// Model throat-clearing that introduces the summary instead of opening it.
// The colon test is language-neutral; the phrase list covers what the models
// emit in this library's languages (no + en).
function isPreamble(text: string): boolean {
  return text.endsWith(":") || /^(her (er|følger)|here('s| is))\b/i.test(text);
}

// The summary's opening: its first substantive paragraph, or its first bullet
// when the whole summary is a list (which the terse presets often produce).
// Horizontal rules, preamble lines and fragments are stepped over; the first
// of them is kept as a last resort, since a weak summary line still beats
// showing a summarized note's typed body or raw transcript. Markdown emphasis
// is stripped — the excerpt renders as plain text.
function summaryOpening(summary: string): string {
  let firstBullet = "";
  let weak = "";
  for (const raw of summary.split("\n")) {
    const line = raw.trim();
    if (!line || line.startsWith("#")) continue;
    const bullet = line.match(/^(?:[-*•—–]|\d+\.)\s+(.*)$/);
    const text = clean(bullet ? bullet[1] : line);
    if (!SUBSTANCE.test(text)) continue;
    if (text.length < EXCERPT_MIN_CHARS || isPreamble(text)) {
      if (!weak) weak = text;
    } else if (bullet) {
      if (!firstBullet) firstBullet = text;
    } else {
      return text;
    }
  }
  return firstBullet || weak;
}

function clean(text: string): string {
  return text.replace(/\*\*|__|`/g, "").replace(/\s+/g, " ").trim();
}

// The card's excerpt. The summary goes first because it is the distilled
// version of everything else on the note; a note with no summary falls back to
// what the user typed, and a pure voice memo to what was said. A body below
// the fragment floor yields to the transcript — which at least says what the
// meeting was about — but is kept over showing nothing.
export function noteExcerpt(n: Note): string {
  const summary = summaryOpening(n.summary);
  if (summary) return summary;
  const body = clean(htmlToText(n.body));
  const bodyHasSubstance = SUBSTANCE.test(body);
  if (bodyHasSubstance && body.length >= EXCERPT_MIN_CHARS) return body;
  const transcript = clean(stripSpeakerLabels(n.transcript));
  if (SUBSTANCE.test(transcript)) return transcript;
  return bodyHasSubstance ? body : "";
}

// How far along a note is. Drives the card's state dot, and reads as the
// answer to "is there anything in here yet?".
export type NoteState = "summarized" | "recorded" | "notes" | "empty";

export function noteState(n: Note): NoteState {
  if (isSummarized(n)) return "summarized";
  if (isRecorded(n)) return "recorded";
  if (htmlToText(n.body).trim()) return "notes";
  return "empty";
}

// A note counts as "recorded" once it has any transcript text; "summarized"
// once a summary has been generated. Used by the All-notes filter chips.
export function isRecorded(n: Note): boolean {
  return n.transcript.trim().length > 0;
}

export function isSummarized(n: Note): boolean {
  return n.summary.trim().length > 0;
}

// Index a list the note views look rows up in by id.
export function indexById<T extends { id: string }>(items: T[]): Map<string, T> {
  return new Map(items.map((item) => [item.id, item]));
}
