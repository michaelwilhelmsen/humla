// The Transcript tab's Clean view (#217): the timeline with the words a clean
// pass marked hidden left out. Pure helpers, so the reader, copy and tests
// share one reading of a mask.
//
// A mask never rewrites: every word shown is a transcribed word with its own
// timing, which is why playback highlighting and click-to-seek work in Clean
// exactly as in Verbatim. The only liberties taken are typographic, and only
// next to a hidden word: a word that now opens a sentence gets its capital,
// and a sentence end a hidden word carried moves onto the word before it.

import type { TimelineEntry } from "./ipc";
import type { TimelineGroup } from "./sessions";

export type TranscriptView = "clean" | "verbatim";

/** The stored `transcript_view` setting as a view; anything but "verbatim" is
 *  Clean, so a fresh install opens on it. */
export function parseTranscriptView(raw: string | null | undefined): TranscriptView {
  return raw === "verbatim" ? "verbatim" : "clean";
}

/** One word the Clean view shows: its text as shown, and its position in the
 *  verbatim word list it came from (for highlighting and seeking). */
export type ShownWord = { text: string; index: number };

// Closing quotes and brackets a sentence end may sit inside. `]` is escaped:
// this goes into a character class.
const CLOSERS = `"'»”’)\\]`;
const SENTENCE_END = new RegExp(`[.!?…][${CLOSERS}]*$`);
const TERMINATOR = new RegExp(`([.!?…]+)[${CLOSERS}]*$`);

function endsSentence(word: string): boolean {
  return SENTENCE_END.test(word);
}

function capitalise(word: string): string {
  const first = word.charAt(0);
  return first.toLowerCase() === first && first.toUpperCase() !== first
    ? first.toUpperCase() + word.slice(1)
    : word;
}

/**
 * The words of one turn left after a mask, spelled for reading.
 *
 * - A word right after hidden words that now opens a sentence (first in the
 *   turn, or after a word ending one) gets a capital: "Eh, skal vi" → "Skal
 *   vi".
 * - Hidden words that ended a sentence hand their terminator to the shown
 *   word before them, replacing a trailing comma: "vi gjør det, eh." → "vi
 *   gjør det."
 *
 * Words outside a hidden run are shown exactly as transcribed.
 */
export function showWords(words: readonly string[], hidden: ReadonlySet<number>): ShownWord[] {
  const out: ShownWord[] = [];
  let runEnd: string | null = null; // terminator the current hidden run ended on
  let inRun = false;
  const closeRun = () => {
    if (!inRun) return;
    const prev = out[out.length - 1];
    if (runEnd && prev && !endsSentence(prev.text)) {
      prev.text = prev.text.replace(/[,;:—–-]+$/, "") + runEnd;
    }
  };
  for (let i = 0; i < words.length; i++) {
    if (hidden.has(i)) {
      inRun = true;
      const m = words[i].match(TERMINATOR);
      runEnd = m ? m[1] : null;
      continue;
    }
    closeRun();
    let text = words[i];
    if (inRun) {
      const prev = out[out.length - 1];
      if (!prev || endsSentence(prev.text)) text = capitalise(text);
    }
    inRun = false;
    runEnd = null;
    out.push({ text, index: i });
  }
  closeRun();
  return out;
}

/** The flat word indices (into `g.words`) a group's mask hides. */
export function hiddenWordSet(g: TimelineGroup): Set<number> {
  return new Set(g.hiddenWords);
}

/** Whether Clean has nothing left to show of this turn: every word hidden.
 *  A turn with no mask at all is never fully hidden. */
export function fullyHidden(g: TimelineGroup): boolean {
  if (g.words.length > 0) return g.hiddenWords.length >= g.words.length;
  return g.textTokens.length > 0 && g.hiddenTokens.length >= g.textTokens.length;
}

/** A turn's text as Clean shows it: its timed words after the mask, or (for a
 *  turn without word timings) its text's tokens after the mask. */
export function cleanGroupText(g: TimelineGroup): string {
  if (g.words.length > 0) {
    return showWords(g.words.map((w) => w.text), new Set(g.hiddenWords))
      .map((w) => w.text)
      .join(" ");
  }
  return showWords(g.textTokens, new Set(g.hiddenTokens))
    .map((w) => w.text)
    .join(" ");
}

/** The whole transcript as Clean shows it, labelled like the stored one
 *  ("Label: text" per turn), for Copy. */
export function cleanTranscriptText(groups: readonly TimelineGroup[]): string {
  return groups
    .map((g) => {
      const text = cleanGroupText(g);
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
  /** Of those, the ones with a mask for their current words. */
  cleaned: number;
  /** Words hidden across them. */
  hiddenWords: number;
};

export function cleanCoverage(timeline: readonly TimelineEntry[]): CleanCoverage {
  let entries = 0;
  let cleaned = 0;
  let hiddenWords = 0;
  for (const e of timeline) {
    const hasWords = (e.words?.length ?? 0) > 0 || e.text.trim().length > 0;
    if (!hasWords) continue;
    entries++;
    if (e.hidden) {
      cleaned++;
      hiddenWords += e.hidden.length;
    }
  }
  return { entries, cleaned, hiddenWords };
}
