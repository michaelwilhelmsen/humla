import { describe, it, expect } from "vitest";
import { extractSpeakerLabels, renameSpeakerInTranscript } from "./speakers";

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
