import { describe, it, expect } from "vitest";
import {
  foldLabel,
  isPlaceholderLabel,
  suggestSpeakerLabels,
  caseVariantTarget,
  MAX_SUGGESTIONS,
} from "./speakerSuggest";
import type { SpeakerLabelStat } from "./speakerSuggest";

// Ranking and matching for the speaker-rename picker (#116 part 1). This is the
// cross-note identity strategy, not a convenience: ADR-0002 has no alias table,
// so the ONLY thing keeping "Hege" / "hege" / "Hege T" from becoming three
// people is that the picker makes people converge on one spelling at write
// time. Hence the tests.

const stat = (label: string, note_count = 1, last_used_at = 1_767_225_600): SpeakerLabelStat => ({
  label,
  note_count,
  last_used_at,
});

describe("foldLabel", () => {
  it("folds case and diacritics onto one key", () => {
    // SQLite LIKE folds ASCII only, so Åse and åse are different keys on both
    // retrieval paths — the fold has to happen here or the split it exists to
    // prevent happens anyway.
    expect(foldLabel("Åse")).toBe(foldLabel("åse"));
    expect(foldLabel("Åse")).toBe(foldLabel("Ase"));
    expect(foldLabel("Hege Tronshaugen")).toBe("hege tronshaugen");
  });

  it("trims surrounding whitespace", () => {
    expect(foldLabel("  Hege  ")).toBe("hege");
  });
});

describe("isPlaceholderLabel", () => {
  it("recognises the placeholders that must never be suggested", () => {
    // Converging on a placeholder is the opposite of the point.
    expect(isPlaceholderLabel("Speaker 1")).toBe(true);
    expect(isPlaceholderLabel("Speaker 12")).toBe(true);
    expect(isPlaceholderLabel("You")).toBe(true);
    expect(isPlaceholderLabel("you")).toBe(true);
  });

  it("leaves real names alone", () => {
    expect(isPlaceholderLabel("Hege")).toBe(false);
    expect(isPlaceholderLabel("Speaker Deck")).toBe(false);
    expect(isPlaceholderLabel("Youssef")).toBe(false);
  });
});

describe("suggestSpeakerLabels matching", () => {
  it("matches at the start of any word, so a surname reaches a full name", () => {
    const out = suggestSpeakerLabels({
      query: "tron",
      stats: [stat("Hege Tronshaugen")],
    });
    expect(out.map((s) => s.label)).toEqual(["Hege Tronshaugen"]);
  });

  it("does not match mid-word", () => {
    const out = suggestSpeakerLabels({ query: "ege", stats: [stat("Hege")] });
    expect(out).toEqual([]);
  });

  it("matches case- and diacritic-insensitively", () => {
    const out = suggestSpeakerLabels({ query: "ase", stats: [stat("Åse Berg")] });
    expect(out.map((s) => s.label)).toEqual(["Åse Berg"]);
  });

  it("returns every candidate for an empty query", () => {
    const out = suggestSpeakerLabels({ query: "", stats: [stat("Hege"), stat("Michael")] });
    expect(out).toHaveLength(2);
  });

  it("never suggests Speaker N or You", () => {
    const out = suggestSpeakerLabels({
      query: "",
      stats: [stat("Speaker 1"), stat("You"), stat("Hege")],
    });
    expect(out.map((s) => s.label)).toEqual(["Hege"]);
  });

  it("never suggests the label being renamed", () => {
    const out = suggestSpeakerLabels({
      query: "",
      stats: [stat("Hege"), stat("Michael")],
      renaming: "Hege",
    });
    expect(out.map((s) => s.label)).toEqual(["Michael"]);
  });

  it("caps the list so the popover never covers the transcript", () => {
    const stats = Array.from({ length: 20 }, (_, i) => stat(`Person ${String.fromCharCode(97 + i)}`));
    expect(suggestSpeakerLabels({ query: "", stats })).toHaveLength(MAX_SUGGESTIONS);
  });
});

describe("suggestSpeakerLabels ranking", () => {
  it("ranks a whole-label match above a later-word match", () => {
    const out = suggestSpeakerLabels({
      query: "berg",
      // The later-word match has the bigger note count, and still loses: where
      // the match lands beats how often the label is used.
      stats: [stat("Åse Berg", 9), stat("Berger", 1)],
    });
    expect(out.map((s) => s.label)).toEqual(["Berger", "Åse Berg"]);
  });

  it("keeps a fold-equal label at the top, so the cap can never hide it", () => {
    // The preselect exception is read off the capped list, so a case variant
    // pushed past rank 8 by higher-count siblings would silently stop being
    // corrected — for exactly the names most likely to have variants.
    const crowd = Array.from({ length: 10 }, (_, i) => stat(`Åse Berg ${i}`, 50 + i));
    const out = suggestSpeakerLabels({ query: "ase", stats: [...crowd, stat("Åse", 1)] });
    expect(out[0].label).toBe("Åse");
    expect(caseVariantTarget("ase", out)).toBe("Åse");
  });

  it("breaks ties on note count, then recency", () => {
    const out = suggestSpeakerLabels({
      query: "",
      stats: [
        stat("Anna", 1, 1_777_000_000),
        stat("Hege", 5, 1_767_000_000),
        stat("Bo", 1, 1_779_000_000),
      ],
    });
    expect(out.map((s) => s.label)).toEqual(["Hege", "Bo", "Anna"]);
  });

  it("does not rank on recency first, so an empty query has a stable order", () => {
    // Recency-first would reorder the list every session and the muscle memory
    // the picker exists to build never forms.
    const first = suggestSpeakerLabels({
      query: "",
      stats: [stat("Hege", 5, 1_767_000_000), stat("Bo", 2, 1_789_000_000)],
    });
    expect(first.map((s) => s.label)).toEqual(["Hege", "Bo"]);
  });
});

describe("suggestSpeakerLabels sources", () => {
  it("suggests a workspace member you have never labelled", () => {
    // Most speakers are not members, but a member who joined a call should be
    // suggested before you have ever typed their name.
    const out = suggestSpeakerLabels({
      query: "hege",
      stats: [],
      roster: ["Hege Tronshaugen"],
    });
    expect(out).toEqual([{ label: "Hege Tronshaugen", kind: "member", inNote: false }]);
  });

  it("marks a member you HAVE labelled as a used label, not a stranger", () => {
    const out = suggestSpeakerLabels({
      query: "hege",
      stats: [stat("Hege Tronshaugen", 3)],
      roster: ["Hege Tronshaugen"],
    });
    expect(out).toHaveLength(1);
    expect(out[0].kind).toBe("used");
  });

  it("ranks labels you have used above members you have not", () => {
    const out = suggestSpeakerLabels({
      query: "",
      stats: [stat("Hege", 1)],
      roster: ["Anna"],
    });
    expect(out.map((s) => s.label)).toEqual(["Hege", "Anna"]);
  });

  it("flags a label already on this note, because picking it merges", () => {
    const out = suggestSpeakerLabels({
      query: "",
      stats: [stat("Hege"), stat("Michael")],
      inNoteLabels: ["Michael"],
    });
    expect(out.find((s) => s.label === "Michael")?.inNote).toBe(true);
    expect(out.find((s) => s.label === "Hege")?.inNote).toBe(false);
  });

  it("shows an in-note label rather than hiding it", () => {
    // Typing an in-note name in full merges anyway, so hiding the row would
    // only remove the warning.
    const out = suggestSpeakerLabels({
      query: "mich",
      stats: [stat("Michael")],
      inNoteLabels: ["Michael"],
    });
    expect(out).toHaveLength(1);
  });
});

describe("caseVariantTarget", () => {
  it("preselects an existing label the typed text differs from only in case", () => {
    // "åse" next to "Åse" is never a deliberate second person.
    expect(caseVariantTarget("åse", [{ label: "Åse", kind: "used", inNote: false }])).toBe("Åse");
  });

  it("preselects nothing when the typed text is an exact match", () => {
    // Enter already commits exactly that; there is nothing to correct.
    expect(caseVariantTarget("Åse", [{ label: "Åse", kind: "used", inNote: false }])).toBeNull();
  });

  it("preselects nothing for a mere prefix", () => {
    // Hege + Enter must not silently write Hege Tronshaugen — a new person is
    // never harder to enter than an existing one.
    expect(
      caseVariantTarget("Hege", [{ label: "Hege Tronshaugen", kind: "used", inNote: false }]),
    ).toBeNull();
  });

  it("preselects nothing for an unrelated name", () => {
    expect(caseVariantTarget("Anna", [{ label: "Hege", kind: "used", inNote: false }])).toBeNull();
  });
});
