import { describe, expect, it } from "vitest";
import {
  cleanCoverage,
  cleanDisplayTimeline,
  cleanTranscriptText,
  emptyTurn,
  parseTranscriptView,
} from "./cleanView";
import type { TimelineEntry, TimelineWord } from "./ipc";
import { groupTimeline } from "./sessions";

function words(text: string, startMs: number): TimelineWord[] {
  return text
    .split(/\s+/)
    .filter(Boolean)
    .map((t, i) => ({ text: t, start_ms: startMs + i * 300, end_ms: startMs + i * 300 + 250 }));
}

function entry(
  label: string,
  text: string,
  startMs: number,
  clean?: { text: string; words: TimelineWord[] },
): TimelineEntry {
  const ws = words(text, startMs);
  return {
    startMs,
    endMs: ws.length > 0 ? ws[ws.length - 1].end_ms : startMs,
    label,
    text,
    words: ws,
    sessionId: "s1",
    sessionIndex: 1,
    chunkIdx: 0,
    clean,
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

describe("cleanDisplayTimeline", () => {
  const segmentWords = words("Ja, jeg har notatene klare og tallene.", 0);
  const timeline = [
    entry("Hege", "Ja, ehm, jeg har jeg har", 0, { text: "Ja, jeg har notatene klare og tallene.", words: segmentWords }),
    entry("Hege", "notatene klare og og tallene.", 1800, { text: "", words: [] }),
    entry("Michael", "Bra.", 4000),
  ];

  it("swaps in the clean text and words, and leaves uncleaned entries verbatim", () => {
    const shown = cleanDisplayTimeline(timeline);
    expect(shown[0].text).toBe("Ja, jeg har notatene klare og tallene.");
    expect(shown[0].words).toBe(segmentWords);
    expect(shown[1].words).toEqual([]);
    expect(shown[2]).toBe(timeline[2]);
  });

  it("stretches a segment's first entry over the whole segment, so its words stay lit", () => {
    const shown = cleanDisplayTimeline(timeline);
    expect(shown[0].endMs).toBe(segmentWords[segmentWords.length - 1].end_ms);
    expect(shown[0].endMs).toBeGreaterThan(timeline[0].endMs);
  });

  it("keeps entry indices one to one, for per-turn actions", () => {
    const shown = cleanDisplayTimeline(timeline);
    expect(shown.map((e) => e.chunkIdx)).toEqual(timeline.map((e) => e.chunkIdx));
    expect(groupTimeline(shown)[0].indices).toEqual([0, 1]);
  });
});

describe("emptyTurn", () => {
  it("is a turn cleaned down to nothing", () => {
    const [filler, reply] = groupTimeline(
      cleanDisplayTimeline([
        entry("Hege", "Mm.", 0, { text: "", words: [] }),
        entry("Michael", "Bra.", 1000, { text: "Bra.", words: words("Bra.", 1000) }),
      ]),
    );
    expect(emptyTurn(filler)).toBe(true);
    expect(emptyTurn(reply)).toBe(false);
  });
});

describe("cleanTranscriptText", () => {
  it("labels each turn like the stored transcript and drops emptied ones", () => {
    const groups = groupTimeline(
      cleanDisplayTimeline([
        entry("Michael", "Eh, skal vi, skal vi starte?", 0, { text: "Skal vi starte?", words: words("Skal vi starte?", 0) }),
        entry("Hege", "Mm.", 2000, { text: "", words: [] }),
        entry("Stian", "Ja, ja. Det, eh, det fikser jeg.", 3000),
      ]),
    );
    expect(cleanTranscriptText(groups)).toBe(
      "Michael: Skal vi starte?\nStian: Ja, ja. Det, eh, det fikser jeg.",
    );
  });
});

describe("cleanCoverage", () => {
  it("counts the entries a clean segment covers", () => {
    expect(
      cleanCoverage([
        entry("A", "en to tre", 0, { text: "En, to, tre.", words: [] }),
        entry("B", "fire fem", 1000, { text: "", words: [] }),
        entry("A", "seks", 2000),
        { ...entry("B", "", 3000), words: [] },
      ]),
    ).toEqual({ entries: 3, cleaned: 2 });
  });
});
