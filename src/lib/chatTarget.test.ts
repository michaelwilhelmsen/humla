import { describe, it, expect } from "vitest";
import { targetNoteId, targetKey, targetDefaultScope } from "./chatTarget";
describe("ChatTarget (#94)", () => {
  const note = { kind: "note", noteId: "n1" } as const;
  const global = { kind: "global" } as const;

  it("maps a note to its id and the library to null, never an empty string", () => {
    // null is what the backend reads as "the whole library"; "" is REJECTED there
    // (#93), so the distinction is load-bearing rather than cosmetic.
    expect(targetNoteId(note)).toBe("n1");
    expect(targetNoteId(global)).toBeNull();
  });

  it("keys a note distinctly from the library, even a note called 'global'", () => {
    expect(targetKey(note)).toBe("note:n1");
    expect(targetKey(global)).toBe("global");
    // Without the prefix these would collide, and the header's stale-projection
    // guard would accept one pane's controls for the other.
    expect(targetKey({ kind: "note", noteId: "global" })).not.toBe(targetKey(global));
  });

  it("is a stable scalar, so it can drive effect deps across fresh objects", () => {
    // The parent rebuilds the target object on most renders; depending on the
    // object would re-run the pane's load effect forever.
    expect(targetKey({ kind: "note", noteId: "n1" })).toBe(targetKey({ ...note }));
  });

  it("defaults the library to all notes and a note to itself", () => {
    // Mirrors `chat::default_breadth` on the Rust side — a library-wide
    // conversation has no anchor to narrow to (#82 fixed v1 at all-notes-only).
    expect(targetDefaultScope(global)).toBe("all");
    expect(targetDefaultScope(note)).toBe("note");
  });
});
