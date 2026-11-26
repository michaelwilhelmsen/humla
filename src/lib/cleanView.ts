// The Transcript tab's Clean view (#217): the note's clean transcript, one
// paragraph per segment of a speaker's words, which the user may edit. The
// backend hands it over as a `CleanView`; these pure helpers are what the
// reader, Copy and tests share of it.

import type { CleanItem, CleanView } from "./ipc";

export type TranscriptView = "clean" | "verbatim";

/** The stored `transcript_view` setting as a view; anything but "verbatim" is
 *  Clean, so a fresh install opens on it. */
export function parseTranscriptView(raw: string | null | undefined): TranscriptView {
  return raw === "verbatim" ? "verbatim" : "clean";
}

/** Words on one line of the reader at its usual width. */
const WORDS_PER_LINE = 18;

/** Placeholder lines for a turn of `words` words, each as a percentage of the
 *  column: full lines, then a last one as long as what is left. */
export function pendingLineWidths(words: number): number[] {
  if (words <= 0) return [];
  const lines = Math.ceil(words / WORDS_PER_LINE);
  const rest = words - (lines - 1) * WORDS_PER_LINE;
  const last = Math.max(15, Math.round((rest / WORDS_PER_LINE) * 100));
  return [...Array<number>(lines - 1).fill(100), last];
}

/** How far a clean pass has come, in percent of its model calls; `null`
 *  until it knows how many it will make. */
export function cleanPercent({ done, total }: { done: number; total: number }): number | null {
  return total > 0 ? Math.floor((done / total) * 100) : null;
}

/** Whether the view has a paragraph to show. */
export function hasParagraphs(view: CleanView | null | undefined): boolean {
  return !!view?.takes.some((t) => t.items.some((i) => i.kind === "turn"));
}

/** Whether a take's verbatim changed under paragraphs the user edited. */
export function hasStaleTake(view: CleanView | null | undefined): boolean {
  return !!view?.takes.some((t) => t.stale);
}

/** The Clean view as text, labelled like the stored transcript ("Label: text",
 *  one line per speaker's run within a take), for Copy. Mirrors
 *  `clean::view_text`. */
export function cleanViewText(view: CleanView): string {
  const lines: string[] = [];
  for (const take of view.takes) {
    let last: string | null = null;
    for (const item of take.items) {
      const text = item.text.trim();
      if (!text) continue;
      if (last === item.speaker && lines.length > 0) {
        lines[lines.length - 1] += ` ${text}`;
      } else {
        lines.push(item.speaker ? `${item.speaker}: ${text}` : text);
        last = item.speaker;
      }
    }
  }
  return lines.join("\n");
}

/** Whether an item starts a new speaker's run in its take. */
export function startsRun(items: readonly CleanItem[], index: number): boolean {
  return index === 0 || items[index - 1].speaker !== items[index].speaker;
}
