import { describe, it, expect } from "vitest";
import { extractSpeakerLabels, renameSpeakerInTranscript, stripSpeakerLabels } from "./speakers";

describe("extractSpeakerLabels", () => {
  it("returns [] for an empty transcript", () => {
    expect(extractSpeakerLabels("")).toEqual([]);
  });

  it("returns [] for a transcript with no speaker prefixes", () => {
    expect(extractSpeakerLabels("just some text\nwith no labels")).toEqual([]);
  });

  it("extracts a single speaker", () => {
    expect(extractSpeakerLabels("Michael: hello there")).toEqual(["Michael"]);
  });

  it("extracts multiple speakers in first-encounter order", () => {
    const t = "Speaker 1: hi\nSpeaker 2: hello\nSpeaker 1: bye";
    expect(extractSpeakerLabels(t)).toEqual(["Speaker 1", "Speaker 2"]);
  });

  it("deduplicates repeated speakers", () => {
    expect(extractSpeakerLabels("Bob: a\nBob: b\nBob: c")).toEqual(["Bob"]);
  });

  it("matches lines with leading whitespace", () => {
    expect(extractSpeakerLabels("   Alice: hi")).toEqual(["Alice"]);
  });

  it("ignores a colon not followed by whitespace (timestamps, URLs)", () => {
    expect(extractSpeakerLabels("meeting at 10:30 sharp")).toEqual([]);
    expect(extractSpeakerLabels("see https://example.com now")).toEqual([]);
  });

  it("ignores labels longer than 40 chars", () => {
    expect(extractSpeakerLabels(`${"x".repeat(41)}: hi`)).toEqual([]);
  });

  it("accepts a label exactly 40 chars long", () => {
    const label = "x".repeat(40);
    expect(extractSpeakerLabels(`${label}: hi`)).toEqual([label]);
  });

  it("trims whitespace between the label and the colon", () => {
    expect(extractSpeakerLabels("Alice : hi")).toEqual(["Alice"]);
  });

  // ── Pinned parity with the Rust parse (issue #104) ────────────────────────
  //
  // The same label rule now exists three times: here, in `db::parse_speaker_turns`
  // (which decides how transcripts are chunked and which speakers each chunk is
  // attributed to), and in humla-cloud's indexer. They MUST agree — a one-sided
  // change means a chunk is attributed to someone the UI never shows as a speaker,
  // or vice versa. #105's `client_id` drift passed every test on both sides, so the
  // mitigation is the same one used for the tool schemas: pin the identical case
  // table in each suite.
  //
  // The mirror of this block is `db::tests::parse_speaker_turns_mirrors_the_frontend_label_rule`.
  // Change one, change both, or the pair stops meaning anything.
  const PINNED_LABEL_CASES: Array<[input: string, expected: string | null]> = [
    ["Michael: hello", "Michael"],
    ["  Michael: hello", "Michael"],
    ["Alice : hi", "Alice"],
    ["Hege Tronshaugen: ja", "Hege Tronshaugen"],
    ["Speaker 1: hi", "Speaker 1"],
    ["You: hi", "You"],
    ["12:30 standup", null],
    ["see https://example.com now", null],
    ["Michael:hello", null],
    ["no colon at all", null],
    [`${"x".repeat(41)}: over the bound`, null],
    [`${"x".repeat(40)}: at the bound`, "x".repeat(40)],
  ];

  it.each(PINNED_LABEL_CASES)("pinned: %j → %j", (input, expected) => {
    expect(extractSpeakerLabels(input)).toEqual(expected === null ? [] : [expected]);
  });
});

describe("renameSpeakerInTranscript", () => {
  it("returns the transcript unchanged when old === new", () => {
    const t = "Bob: hi";
    expect(renameSpeakerInTranscript(t, "Bob", "Bob")).toBe(t);
  });

  it("renames every turn by the speaker", () => {
    const t = "Speaker 1: hi\nSpeaker 2: yo\nSpeaker 1: bye";
    expect(renameSpeakerInTranscript(t, "Speaker 1", "Michael")).toBe(
      "Michael: hi\nSpeaker 2: yo\nMichael: bye",
    );
  });

  it("only rewrites line-start labels, not mid-line mentions", () => {
    const t = "Alice: I talked to Bob: about it";
    expect(renameSpeakerInTranscript(t, "Bob", "Robert")).toBe(t);
  });

  it("preserves leading whitespace", () => {
    expect(renameSpeakerInTranscript("   Speaker 1: hi", "Speaker 1", "Bob")).toBe(
      "   Bob: hi",
    );
  });

  it("does not clobber a label that is a prefix of another", () => {
    const t = "Speaker 1: a\nSpeaker 10: b";
    expect(renameSpeakerInTranscript(t, "Speaker 1", "Bob")).toBe(
      "Bob: a\nSpeaker 10: b",
    );
  });

  it("escapes regex metacharacters in the old label", () => {
    const t = "Speaker (1)?: hi\nSpeaker (1)?: bye";
    expect(renameSpeakerInTranscript(t, "Speaker (1)?", "Anna")).toBe(
      "Anna: hi\nAnna: bye",
    );
  });

  it("renames to a new label containing special chars", () => {
    expect(renameSpeakerInTranscript("Bob: hi", "Bob", "Speaker 1?")).toBe(
      "Speaker 1?: hi",
    );
  });
});

describe("stripSpeakerLabels", () => {
  it("removes the label prefix from every turn", () => {
    expect(stripSpeakerLabels("Michael: Hei.\nHege: Hallo.")).toBe("Hei.\nHallo.");
  });

  it("keeps leading whitespace and leaves colons inside text alone", () => {
    expect(stripSpeakerLabels("  Michael: se her: dette")).toBe("  se her: dette");
  });

  it("leaves a line with no label untouched", () => {
    expect(stripSpeakerLabels("bare tekst uten prefiks")).toBe("bare tekst uten prefiks");
  });
});
