import { describe, expect, it } from "vitest";
import {
  cleanCoverage,
  cleanGroupText,
  cleanTranscriptText,
  fullyHidden,
  parseTranscriptView,
  showWords,
} from "./cleanView";
import type { TimelineEntry } from "./ipc";
import { groupTimeline } from "./sessions";

function entry(label: string, text: string, opts: { hidden?: number[]; timed?: boolean } = {}): TimelineEntry {
  const timed = opts.timed ?? true;
  return {
    startMs: 0,
    endMs: 1000,
    label,
    text,
    words: timed
      ? text.split(/\s+/).filter(Boolean).map((t, i) => ({ text: t, start_ms: i * 300, end_ms: i * 300 + 250 }))
      : [],
    sessionId: "s1",
    sessionIndex: 1,
    chunkIdx: 0,
    hidden: opts.hidden,
  };
}

const shown = (words: string, hidden: number[]) =>
  showWords(words.split(" "), new Set(hidden));

describe("parseTranscriptView", () => {
  it("opens on Clean unless Verbatim was picked", () => {
    expect(parseTranscriptView(null)).toBe("clean");
    expect(parseTranscriptView("clean")).toBe("clean");
    expect(parseTranscriptView("verbatim")).toBe("verbatim");
    expect(parseTranscriptView("junk")).toBe("clean");
  });
});

describe("showWords", () => {
  it("keeps every word and its position when nothing is hidden", () => {
    expect(shown("Ja, det går.", [])).toEqual([
      { text: "Ja,", index: 0 },
      { text: "det", index: 1 },
      { text: "går.", index: 2 },
    ]);
  });

  it("capitalises the word that now opens the turn", () => {
    expect(shown("Eh, skal vi, skal vi ta det?", [0, 1, 2]).map((w) => w.text)).toEqual([
      "Skal",
      "vi",
      "ta",
      "det?",
    ]);
  });

  it("capitalises after a sentence end, and only next to a hidden word", () => {
    expect(shown("Bra. øh, da starter vi.", [1]).map((w) => w.text)).toEqual(["Bra.", "Da", "starter", "vi."]);
    // No hidden word before it: shown exactly as transcribed.
    expect(shown("bra. da starter vi.", []).map((w) => w.text)).toEqual(["bra.", "da", "starter", "vi."]);
  });

  it("keeps the case mid-sentence", () => {
    expect(shown("Ja, ehm, jeg har notatene.", [1]).map((w) => w.text)).toEqual(["Ja,", "jeg", "har", "notatene."]);
  });

  it("moves a sentence end a hidden word carried onto the word before it", () => {
    expect(shown("vi gjør det, eh.", [3]).map((w) => w.text)).toEqual(["vi", "gjør", "det."]);
    expect(shown("Så det, liksom? Ja.", [2]).map((w) => w.text)).toEqual(["Så", "det?", "Ja."]);
  });

  it("keeps the verbatim index of every shown word, for highlighting and seeking", () => {
    expect(shown("en eh to", [1]).map((w) => w.index)).toEqual([0, 2]);
  });
});

describe("groupTimeline with masks", () => {
  it("flattens each entry's mask into its turn's word positions", () => {
    const groups = groupTimeline([
      entry("Hege", "Ja, ehm, jeg", { hidden: [1] }),
      entry("Hege", "har jeg har notatene.", { hidden: [0, 1] }),
    ]);
    expect(groups).toHaveLength(1);
    expect(groups[0].hiddenWords).toEqual([1, 3, 4]);
    expect(cleanGroupText(groups[0])).toBe("Ja, jeg har notatene.");
  });

  it("masks a turn without word timings over its text's tokens", () => {
    const [g] = groupTimeline([entry("Hege", "Eh, det fikser jeg.", { hidden: [0], timed: false })]);
    expect(g.textTokens).toEqual(["Eh,", "det", "fikser", "jeg."]);
    expect(cleanGroupText(g)).toBe("Det fikser jeg.");
  });

  it("ignores mask positions past the entry's words", () => {
    const [g] = groupTimeline([entry("Hege", "Ja klart", { hidden: [0, 7] })]);
    expect(g.hiddenWords).toEqual([0]);
  });

  it("treats a turn as fully hidden only when every word is", () => {
    const [mm, reply, unmasked] = groupTimeline([
      entry("Hege", "Mm.", { hidden: [0] }),
      entry("Michael", "Bra.", { hidden: [] }),
      entry("Hege", "Ja"),
    ]);
    expect(fullyHidden(mm)).toBe(true);
    expect(fullyHidden(reply)).toBe(false);
    expect(fullyHidden(unmasked)).toBe(false);
  });
});

describe("cleanTranscriptText", () => {
  it("labels each turn like the stored transcript and drops emptied ones", () => {
    const groups = groupTimeline([
      entry("Michael", "Eh, skal vi starte?", { hidden: [0] }),
      entry("Hege", "Mm.", { hidden: [0] }),
      entry("Stian", "Ja, ja. Det fikser jeg.", { hidden: [0] }),
    ]);
    expect(cleanTranscriptText(groups)).toBe("Michael: Skal vi starte?\nStian: Ja. Det fikser jeg.");
  });
});

describe("cleanCoverage", () => {
  it("counts the entries with a mask for their words and the words hidden", () => {
    expect(
      cleanCoverage([
        entry("A", "en to tre", { hidden: [1] }),
        entry("B", "fire fem", { hidden: [] }),
        entry("A", "seks"),
        entry("B", "", { timed: false }),
      ]),
    ).toEqual({ entries: 3, cleaned: 2, hiddenWords: 1 });
  });
});
