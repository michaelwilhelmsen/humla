import { describe, expect, it } from "vitest";
import {
  cleanPercent,
  cleanViewText,
  hasParagraphs,
  hasStaleTake,
  parseTranscriptView,
  pendingLineWidths,
  startsRun,
} from "./cleanView";
import type { CleanItem, CleanView } from "./ipc";

const turn = (speaker: string, text: string, id = text): CleanItem => ({
  kind: "turn",
  id,
  speaker,
  text,
  startMs: 0,
  edited: false,
});
const gap = (speaker: string, text: string): CleanItem => ({
  kind: "gap",
  speaker,
  text,
  startMs: 0,
  words: text.split(" ").length,
});

function view(...takes: CleanItem[][]): CleanView {
  return {
    takes: takes.map((items, i) => ({ sessionId: `s${i + 1}`, items, stale: false })),
    words: 0,
    cleanedWords: 0,
    edited: 0,
    unreadable: false,
  };
}

describe("parseTranscriptView", () => {
  it("opens on Clean unless Verbatim was picked", () => {
    expect(parseTranscriptView(null)).toBe("clean");
    expect(parseTranscriptView("clean")).toBe("clean");
    expect(parseTranscriptView("verbatim")).toBe("verbatim");
    expect(parseTranscriptView("junk")).toBe("clean");
  });
});

describe("cleanViewText", () => {
  it("joins a speaker's paragraphs into one line per take, mirroring the backend", () => {
    const v = view(
      [turn("Michael", "Skal vi?"), turn("Michael", "La oss."), gap("Hege", "eh ok"), turn("Hege", "")],
      [turn("Hege", "Bra.")],
    );
    expect(cleanViewText(v)).toBe("Michael: Skal vi? La oss.\nHege: eh ok\nHege: Bra.");
  });
});

describe("hasParagraphs / hasStaleTake", () => {
  it("reads whether there is anything clean, and whether a take went stale", () => {
    expect(hasParagraphs(null)).toBe(false);
    expect(hasParagraphs(view([gap("Hege", "eh ja")]))).toBe(false);
    expect(hasParagraphs(view([turn("Hege", "Ja.")]))).toBe(true);
    const stale = view([turn("Hege", "Ja.")]);
    expect(hasStaleTake(stale)).toBe(false);
    stale.takes[0].stale = true;
    expect(hasStaleTake(stale)).toBe(true);
  });
});

describe("startsRun", () => {
  it("is true where the speaker changes", () => {
    const items = [turn("A", "1"), turn("A", "2"), gap("B", "3"), turn("A", "4")];
    expect(items.map((_, i) => startsRun(items, i))).toEqual([true, false, true, true]);
  });
});

describe("pendingLineWidths", () => {
  it("draws full lines, then the rest", () => {
    expect(pendingLineWidths(0)).toEqual([]);
    expect(pendingLineWidths(9)).toEqual([50]);
    expect(pendingLineWidths(40)).toEqual([100, 100, 22]);
    expect(pendingLineWidths(1)).toEqual([15]);
  });
});

describe("cleanPercent", () => {
  it("is unknown until the pass knows its calls", () => {
    expect(cleanPercent({ done: 0, total: 0 })).toBeNull();
    expect(cleanPercent({ done: 1, total: 3 })).toBe(33);
  });
});
