import { describe, it, expect, vi } from "vitest";
import {
  notesWithSpeaker,
  renameOutcomeMessage,
  renameSpeakerAcrossNotes,
} from "./crossNoteRename";
import { makeNote } from "../test/fixtures";

const note = (id: string, transcript: string) => makeNote({ id, transcript });

// Renaming a speaker across every note that names them (#116 part 2).
//
// ADR-0002 forbids an alias table, so rewriting the transcripts IS the only
// repair for four spellings of one person. That makes this the primitive the
// ADR's line 19 promises, and the reason it loops the SAME per-note path a
// single-note rename already takes rather than adding a Rust sweep: there is
// one rewrite implementation and no fourth copy of the label-parse rule.

describe("notesWithSpeaker", () => {
  it("finds notes whose transcript carries the label as a speaker turn", () => {
    const notes = [
      note("a", "Hege: hello\nMichael: hi"),
      note("b", "Michael: solo"),
      note("c", "Hege: again"),
    ];
    expect(notesWithSpeaker(notes, "Hege").map((n) => n.id)).toEqual(["a", "c"]);
  });

  it("ignores the label appearing mid-sentence", () => {
    // The rename rule is line-anchored precisely because a mid-sentence label is
    // ambiguous, so the count must be anchored the same way or the button
    // promises a rewrite that won't happen.
    const notes = [note("a", "Michael: I spoke to Hege about it")];
    expect(notesWithSpeaker(notes, "Hege")).toEqual([]);
  });

  it("is exact, so one label never sweeps up another", () => {
    const notes = [note("a", "Michael Berg: hi"), note("b", "Michael: hi")];
    expect(notesWithSpeaker(notes, "Michael").map((n) => n.id)).toEqual(["b"]);
  });

  it("matches the literal You label older recordings still carry", () => {
    expect(notesWithSpeaker([note("a", "You: hi\nSpeaker 2: hello")], "You")).toHaveLength(1);
  });

  it("finds nothing for a label nobody uses", () => {
    expect(notesWithSpeaker([note("a", "Hege: hi")], "Anna")).toEqual([]);
  });
});

describe("renameSpeakerAcrossNotes", () => {
  function deps() {
    return {
      updateNote: vi.fn().mockResolvedValue(undefined),
      noteTimelineRename: vi.fn().mockResolvedValue(undefined),
      uploadNoteSessions: vi.fn().mockResolvedValue(undefined),
      reindexNote: vi.fn().mockResolvedValue(undefined),
      onRewritten: vi.fn(),
    };
  }

  it("rewrites the transcript of every affected note", async () => {
    const d = deps();
    const notes = [note("a", "Hege: one"), note("b", "Hege: two\nMichael: three")];

    const out = await renameSpeakerAcrossNotes({
      notes,
      oldLabel: "Hege",
      newLabel: "Hege Tronshaugen",
      ...d,
    });

    expect(out).toEqual({ renamed: ["a", "b"], failed: [] });
    expect(d.updateNote).toHaveBeenCalledWith("a", { transcript: "Hege Tronshaugen: one" });
    expect(d.updateNote).toHaveBeenCalledWith("b", {
      transcript: "Hege Tronshaugen: two\nMichael: three",
    });
  });

  it("renames the timeline too, and re-uploads and reindexes each note", async () => {
    const d = deps();
    await renameSpeakerAcrossNotes({
      notes: [note("a", "You: hi")],
      oldLabel: "You",
      newLabel: "Michael",
      ...d,
    });

    // The same chain a single-note rename takes, so the sync ping and the
    // retrieval index stay correct rather than being reimplemented here.
    expect(d.noteTimelineRename).toHaveBeenCalledWith("a", "You", "Michael");
    expect(d.uploadNoteSessions).toHaveBeenCalledWith("a");
    expect(d.reindexNote).toHaveBeenCalledWith("a");
  });

  it("updates the store before awaiting anything, so every pill flips at once", async () => {
    const d = deps();
    const order: string[] = [];
    d.onRewritten.mockImplementation((id: string) => order.push(`local:${id}`));
    d.updateNote.mockImplementation((id: string) => {
      order.push(`write:${id}`);
      return Promise.resolve();
    });

    await renameSpeakerAcrossNotes({
      notes: [note("a", "Hege: one"), note("b", "Hege: two")],
      oldLabel: "Hege",
      newLabel: "H",
      ...d,
    });

    // Both optimistic updates land before the first write is awaited.
    expect(order.slice(0, 2)).toEqual(["local:a", "local:b"]);
  });

  it("hands the rewritten transcript to the store, not just the id", async () => {
    const d = deps();
    await renameSpeakerAcrossNotes({
      notes: [note("a", "Hege: one")],
      oldLabel: "Hege",
      newLabel: "Anna",
      ...d,
    });
    expect(d.onRewritten).toHaveBeenCalledWith("a", "Anna: one");
  });

  it("skips notes that do not carry the label", async () => {
    const d = deps();
    const out = await renameSpeakerAcrossNotes({
      notes: [note("a", "Hege: one"), note("b", "Michael: two")],
      oldLabel: "Hege",
      newLabel: "Anna",
      ...d,
    });
    expect(out.renamed).toEqual(["a"]);
    expect(d.updateNote).toHaveBeenCalledTimes(1);
  });

  it("reports which notes failed instead of stopping at the first", async () => {
    const d = deps();
    d.updateNote.mockImplementation((id: string) =>
      id === "b" ? Promise.reject(new Error("disk full")) : Promise.resolve(),
    );

    const out = await renameSpeakerAcrossNotes({
      notes: [note("a", "Hege: 1"), note("b", "Hege: 2"), note("c", "Hege: 3")],
      oldLabel: "Hege",
      newLabel: "Anna",
      ...d,
    });

    // "Renamed in 2 of 3" is the honest report; a silent partial is not.
    expect(out).toEqual({ renamed: ["a", "c"], failed: ["b"] });
  });

  it("counts a note as renamed when only the timeline rename fails", async () => {
    const d = deps();
    // The transcript is the source of truth and it was rewritten; a failed
    // timeline rename degrades playback labels, it doesn't undo the rename.
    d.noteTimelineRename.mockRejectedValue(new Error("no timeline"));

    const out = await renameSpeakerAcrossNotes({
      notes: [note("a", "Hege: 1")],
      oldLabel: "Hege",
      newLabel: "Anna",
      ...d,
    });
    expect(out).toEqual({ renamed: ["a"], failed: [] });
  });

  it("still reindexes when the timeline rename fails", async () => {
    const d = deps();
    d.noteTimelineRename.mockRejectedValue(new Error("no timeline"));

    await renameSpeakerAcrossNotes({
      notes: [note("a", "Hege: 1")],
      oldLabel: "Hege",
      newLabel: "Anna",
      ...d,
    });

    // Sharing one try meant a failed timeline rename also skipped these, so the
    // note dropped out of retrieval freshness while the toast said "renamed".
    expect(d.uploadNoteSessions).toHaveBeenCalledWith("a");
    expect(d.reindexNote).toHaveBeenCalledWith("a");
  });

  it("does nothing when the label is unchanged", async () => {
    const d = deps();
    const out = await renameSpeakerAcrossNotes({
      notes: [note("a", "Hege: 1")],
      oldLabel: "Hege",
      newLabel: "Hege",
      ...d,
    });
    expect(out).toEqual({ renamed: [], failed: [] });
    expect(d.updateNote).not.toHaveBeenCalled();
  });
});

describe("renameOutcomeMessage", () => {
  it("reports a clean sweep", () => {
    expect(renameOutcomeMessage({ renamed: ["a", "b"], failed: [] })).toBe("Renamed in 2 notes");
  });

  it("says note, singular, for one", () => {
    expect(renameOutcomeMessage({ renamed: ["a"], failed: [] })).toBe("Renamed in 1 note");
  });

  it("says how many failed rather than reporting success", () => {
    // No silence when it didn't fully work — a partial rewrite the user was
    // never told about is the failure mode worth avoiding.
    expect(renameOutcomeMessage({ renamed: ["a", "b"], failed: ["c"] })).toBe(
      "Renamed in 2 of 3 — 1 failed",
    );
  });

  it("reports a total failure as such", () => {
    expect(renameOutcomeMessage({ renamed: [], failed: ["a", "b"] })).toBe(
      "Renamed in 0 of 2 — 2 failed",
    );
  });
});
