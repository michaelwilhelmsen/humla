import { describe, it, expect } from "vitest";
import {
  targetNoteId,
  targetFromKey,
  targetFolderId,
  targetKey,
  targetDefaultScope,
  targetPinsScope,
  targetResumesOnOpen,
} from "./chatTarget";
describe("ChatTarget (#94)", () => {
  const note = { kind: "note", noteId: "n1" } as const;
  const folder = { kind: "folder", folderId: "f1" } as const;
  const global = { kind: "global" } as const;

  it("maps a note to its id and every note-less target to null, never an empty string", () => {
    // null is what the backend reads as "no anchor"; "" is REJECTED there
    // (#93), so the distinction is load-bearing rather than cosmetic.
    expect(targetNoteId(note)).toBe("n1");
    expect(targetNoteId(global)).toBeNull();
    // A folder pane has no anchor note either (#110) — which is exactly why a
    // null note id can no longer be read as "the whole library" on its own.
    expect(targetNoteId(folder)).toBeNull();
  });

  it("sends the folder id only for a folder target, never beside a note", () => {
    // The backend rejects both ids at once (`ChatTarget::from_ids`), so these two
    // accessors must never both be non-null for the same target.
    expect(targetFolderId(folder)).toBe("f1");
    expect(targetFolderId(note)).toBeNull();
    expect(targetFolderId(global)).toBeNull();
    for (const t of [note, folder, global]) {
      expect(targetNoteId(t) !== null && targetFolderId(t) !== null).toBe(false);
    }
  });

  it("keys each target distinctly, even when their ids collide", () => {
    expect(targetKey(note)).toBe("note:n1");
    expect(targetKey(folder)).toBe("folder:f1");
    expect(targetKey(global)).toBe("global");
    // Without the prefixes these would collide, and the header's stale-projection
    // guard would accept one pane's controls for the other.
    expect(targetKey({ kind: "note", noteId: "global" })).not.toBe(targetKey(global));
    expect(targetKey({ kind: "folder", folderId: "n1" })).not.toBe(targetKey(note));
  });

  it("is a stable scalar, so it can drive effect deps across fresh objects", () => {
    // The parent rebuilds the target object on most renders; depending on the
    // object would re-run the pane's load effect forever.
    expect(targetKey({ kind: "note", noteId: "n1" })).toBe(targetKey({ ...note }));
    expect(targetKey({ kind: "folder", folderId: "f1" })).toBe(targetKey({ ...folder }));
  });

  it("round-trips through its key, which is what lets a pane depend on the key alone", () => {
    // `ChatPanel` re-derives its target from this key so the two can't drift: the
    // parent rebuilds the prop object every render, so the key is the only stable
    // identity, and a lossy inverse would silently retarget the pane.
    for (const t of [note, folder, global]) {
      expect(targetFromKey(targetKey(t))).toEqual(t);
    }
    // Ids containing the separator survive — a folder id is opaque to us.
    const odd = { kind: "folder", folderId: "folder:weird:id" } as const;
    expect(targetFromKey(targetKey(odd))).toEqual(odd);
  });

  it("defaults each target to its own reach", () => {
    // Mirrors `chat::default_breadth` on the Rust side.
    expect(targetDefaultScope(global)).toBe("all");
    expect(targetDefaultScope(note)).toBe("note");
    expect(targetDefaultScope(folder)).toBe("folder");
  });

  it("pins every note-less target's scope, leaving only a note a real choice", () => {
    // Mirrors `commands::chat::pinned_breadth`. With no anchor the target's
    // identity IS its reach, so the picker has nothing to offer — and for a folder
    // the clamp is the entire point of the surface (#110).
    expect(targetPinsScope(folder)).toBe(true);
    expect(targetPinsScope(global)).toBe(true);
    expect(targetPinsScope(note)).toBe(false);
    // A pinned target's default is therefore the only value it can hold, so the
    // fallback the picker falls back TO is never one the backend would reject.
    expect(targetDefaultScope(folder)).toBe("folder");
  });

  it("resumes only a note's pane, drafting on both routes", () => {
    // Mirrors `commands::chat::resumes_on_open` — if the two sides disagree the
    // pane either shows a draft the next send files into an old thread, or creates
    // rows it will never open again.
    expect(targetResumesOnOpen(note)).toBe(true);
    expect(targetResumesOnOpen(global)).toBe(false);
    expect(targetResumesOnOpen(folder)).toBe(false);
  });
});
