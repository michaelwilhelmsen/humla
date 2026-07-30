// What a chat pane is about (#94). Pure and framework-free so it unit-tests
// without rendering — and its own module rather than a corner of chatSessions.ts,
// which is display helpers for the history popover; this is domain + wire
// semantics, and the two change for unrelated reasons.

import type { ChatScope } from "./ipc";

/** One Note, one folder, or the whole library.
 *
 *  A discriminated union rather than a nullable note id, so "the library" can
 *  never be confused with "a note we haven't loaded yet" — and specifically NOT a
 *  sentinel empty string, which is the ambiguity humla-cloud#26 records the server
 *  having avoided and #93 mirrored on the Rust side. #110 adds the folder arm,
 *  which is why the union earns its keep a second time: a folder pane also has no
 *  note id, and a nullable one would have made it indistinguishable from the
 *  library-wide pane. */
export type ChatTarget =
  | { kind: "note"; noteId: string }
  | { kind: "folder"; folderId: string }
  | { kind: "global" };

/** The anchor note id for an IPC call, or null for a note-less pane.
 *
 *  The backend reads an ABSENT note id as a note-less scope and REJECTS an empty
 *  one (#93), so null is the only correct way to say "no anchor" over the wire.
 *
 *  Null no longer means "the whole library" on its own — a folder pane returns
 *  null here too — so pair it with `targetFolderId` rather than reading it alone. */
export function targetNoteId(target: ChatTarget): string | null {
  return target.kind === "note" ? target.noteId : null;
}

/** The folder id for an IPC call, or null (#110). Sent beside `targetNoteId`;
 *  the two are alternatives and the backend rejects both at once. */
export function targetFolderId(target: ChatTarget): string | null {
  return target.kind === "folder" ? target.folderId : null;
}

/** A non-nullable string identity, for the header's stale-projection guard.
 *
 *  The guard compares by value across a component boundary, where a nullable id
 *  would make "the library" and "not loaded yet" indistinguishable. The prefix
 *  keeps a note whose id is literally "global" from colliding with the library —
 *  and a folder whose id matches a note's from colliding with that note. */
export function targetKey(target: ChatTarget): string {
  if (target.kind === "note") return `note:${target.noteId}`;
  if (target.kind === "folder") return `folder:${target.folderId}`;
  return "global";
}

/** Rebuild a target from its key — the exact inverse of `targetKey`.
 *
 *  Exists so a component can hold a target whose IDENTITY is the key rather than
 *  the object literal its parent rebuilds on every render. Before this, panes
 *  keyed their effects on a scalar while calling IPC with the prop, which meant
 *  every hook dependency list deliberately omitted the object — eight lint
 *  warnings standing in for one idea. Memoising this on the key states the idea
 *  once, structurally, and the round-trip is unit-tested.
 *
 *  An unrecognised key falls back to the library-wide target: keys come from
 *  `targetKey` and nowhere else, so this is unreachable rather than lenient. */
export function targetFromKey(key: string): ChatTarget {
  if (key.startsWith("note:")) return { kind: "note", noteId: key.slice(5) };
  if (key.startsWith("folder:")) return { kind: "folder", folderId: key.slice(7) };
  return { kind: "global" };
}

/** The breadth a pane falls back to when the persisted value can't be read or is
 *  no longer offerable. Mirrors `chat::default_breadth` on the Rust side (#93,
 *  #110): each surface starts at its own reach. For a note-less pane this is also
 *  the ONLY breadth it may hold — see `targetPinsScope`. */
export function targetDefaultScope(target: ChatTarget): ChatScope {
  if (target.kind === "global") return "all";
  if (target.kind === "folder") return "folder";
  return "note";
}

/** Whether this target holds exactly one breadth, with no choice to offer.
 *
 *  Mirrors `commands::chat::pinned_breadth`. With no anchor note, the target's
 *  identity IS its reach: a library-wide pane always searches everything, and a
 *  folder pane always searches that folder — the clamp being the entire point of
 *  the surface. A Note is the one target with a real choice, which is what the
 *  breadth picker is for. */
export function targetPinsScope(target: ChatTarget): boolean {
  return target.kind !== "note";
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
 *  is what it is for. A folder pane drafts for the same reason (#110): it is a
 *  route you navigate to in order to ask something, and a folder narrows what is
 *  in reach without being a text you were reading. A Note's pane resumes,
 *  deliberately: a note IS an anchor, and coming back to continue the same line of
 *  thinking is a plausible default there in a way it isn't on a route. */
export function targetResumesOnOpen(target: ChatTarget): boolean {
  return target.kind === "note";
}
