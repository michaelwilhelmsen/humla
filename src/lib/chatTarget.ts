// What a chat pane is about (#94). Pure and framework-free so it unit-tests
// without rendering — and its own module rather than a corner of chatSessions.ts,
// which is display helpers for the history popover; this is domain + wire
// semantics, and the two change for unrelated reasons.

import type { ChatScope } from "./ipc";

/** One Note, or the whole library.
 *
 *  A discriminated union rather than a nullable note id, so "the library" can
 *  never be confused with "a note we haven't loaded yet" — and specifically NOT a
 *  sentinel empty string, which is the ambiguity humla-cloud#26 records the server
 *  having avoided and #93 mirrored on the Rust side. */
export type ChatTarget = { kind: "note"; noteId: string } | { kind: "global" };

/** The anchor note id for an IPC call, or null for a library-wide pane.
 *
 *  The backend reads an ABSENT note id as the global scope and REJECTS an empty
 *  one (#93), so null is the only correct way to say "the whole library" over the
 *  wire. This doubles as the pane's dependency identity: it is already a stable
 *  scalar, and `null` ⟺ global, so a change in it is exactly a change of target. */
export function targetNoteId(target: ChatTarget): string | null {
  return target.kind === "note" ? target.noteId : null;
}

/** A non-nullable string identity, for the header's stale-projection guard.
 *
 *  The guard compares by value across a component boundary, where a nullable id
 *  would make "the library" and "not loaded yet" indistinguishable. The prefix
 *  keeps a note whose id is literally "global" from colliding with the library. */
export function targetKey(target: ChatTarget): string {
  return target.kind === "note" ? `note:${target.noteId}` : "global";
}

/** The breadth a pane falls back to when the persisted value can't be read or is
 *  no longer offerable. Mirrors `chat::default_breadth` on the Rust side (#93): a
 *  library-wide conversation has no anchor to narrow to, so it is always "all"
 *  (#82 fixed v1 at all-notes-only). */
export function targetDefaultScope(target: ChatTarget): ChatScope {
  return target.kind === "global" ? "all" : "note";
}

/** Whether opening this target's pane RESUMES its most-recent conversation, or
 *  starts an unsaved draft (#120).
 *
 *  Mirrors `commands::chat::resumes_on_open` on the Rust side, and the mirroring
 *  matters more than usual: the backend decides what a bare request resolves to,
 *  while the frontend decides whether "+" and a settings change persist anything.
 *  If the two disagree, the pane either shows a draft the next send files into an
 *  old thread, or creates rows the pane will never open again. Change both.
 *
 *  A library-wide pane drafts — `/chat` is a front door, and asking something new
 *  is what it is for. A Note's pane resumes, deliberately: a note is an anchor,
 *  and coming back to continue the same line of thinking is a plausible default
 *  there in a way it isn't library-wide. */
export function targetResumesOnOpen(target: ChatTarget): boolean {
  return target.kind === "note";
}
