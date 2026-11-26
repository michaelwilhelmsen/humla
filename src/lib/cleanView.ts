// The Transcript tab's Clean view (#217): a clean read of the transcript,
// written by a model per segment of one speaker's words, beside the verbatim
// one. Pure helpers, so the reader, copy and tests share one reading of it.
//
// The backend hands each timeline entry its Clean text (`entry.clean`), with
// words already aligned to what was said and timed in the take's clock. The
// reader shows Clean by swapping each entry's words and text for those, so
// grouping, playback highlighting and click-to-seek run unchanged on top.

import type { TimelineEntry } from "./ipc";
import type { TimelineGroup } from "./sessions";

export type TranscriptView = "clean" | "verbatim";

/** The stored `transcript_view` setting as a view; anything but "verbatim" is
 *  Clean, so a fresh install opens on it. */
export function parseTranscriptView(raw: string | null | undefined): TranscriptView {
  return raw === "verbatim" ? "verbatim" : "clean";
}

/**
 * The timeline as the Clean view shows it: every entry with a clean text
 * takes it. The rest stay verbatim, or, while a pass is on its way to them
 * (`pending`), are emptied so the reader draws a placeholder rather than
 * passing verbatim off as clean. A segment's first entry carries the whole
 * segment, so it is stretched to the segment's last word: the player lights
 * an entry's words only while the playhead is inside the entry. Indices line
 * up one to one with `timeline`, so per-turn actions still address the real
 * entries.
 */
export function cleanDisplayTimeline(
  timeline: readonly TimelineEntry[],
  { pending = false }: { pending?: boolean } = {},
): TimelineEntry[] {
  return timeline.map((e) => {
    if (!e.clean) return pending ? { ...e, text: "", words: [] } : e;
    const lastEnd = e.clean.words.length > 0 ? e.clean.words[e.clean.words.length - 1].end_ms : 0;
    return {
      ...e,
      text: e.clean.text,
      words: e.clean.words,
      endMs: Math.max(e.endMs, lastEnd),
    };
  });
}

function entryWords(e: TimelineEntry): number {
  const timed = e.words?.length ?? 0;
  return timed > 0 ? timed : e.text.split(/\s+/).filter(Boolean).length;
}

/** Verbatim words among `indices` that no clean segment covers yet. */
export function pendingWords(timeline: readonly TimelineEntry[], indices: readonly number[]): number {
  let n = 0;
  for (const i of indices) {
    const e = timeline[i];
    if (e && !e.clean) n += entryWords(e);
  }
  return n;
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

/** Whether Clean has nothing left to show of this turn: it was only filler. */
export function emptyTurn(g: TimelineGroup): boolean {
  return g.words.length === 0 && g.text.trim() === "";
}

/** The whole transcript as Clean shows it, labelled like the stored one
 *  ("Label: text" per turn), for Copy. Expects groups of the clean display
 *  timeline. */
export function cleanTranscriptText(groups: readonly TimelineGroup[]): string {
  return groups
    .map((g) => {
      const text = g.words.length > 0 ? g.words.map((w) => w.text).join(" ") : g.text.trim();
      if (!text) return "";
      return g.label ? `${g.label}: ${text}` : text;
    })
    .filter(Boolean)
    .join("\n");
}

/** Where a note's timeline stands with Clean. */
export type CleanCoverage = {
  /** Entries with words a clean pass could read. */
  entries: number;
  /** Of those, the ones a clean segment covers. */
  cleaned: number;
};

export function cleanCoverage(timeline: readonly TimelineEntry[]): CleanCoverage {
  let entries = 0;
  let cleaned = 0;
  for (const e of timeline) {
    const hasWords = (e.words?.length ?? 0) > 0 || e.text.trim().length > 0;
    if (!hasWords) continue;
    entries++;
    if (e.clean) cleaned++;
  }
  return { entries, cleaned };
}
