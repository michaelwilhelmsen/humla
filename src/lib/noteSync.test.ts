import { describe, it, expect } from "vitest";
import {
  isReplaceableTitle,
  shouldAdoptRemoteBody,
  shouldAdoptRemoteTitle,
  shouldRequestTitleForBody,
  TITLE_MIN_BODY_CHARS,
} from "./noteSync";

describe("shouldAdoptRemoteBody", () => {
  it("adopts a body that arrives after the draft was seeded empty", () => {
    // The bug: a workspace note mounted between the row landing and the body
    // landing stayed blank until the user navigated away and back.
    expect(shouldAdoptRemoteBody("", "<p>from the workspace</p>", false)).toBe(true);
  });

  it("treats a whitespace-only local body as empty", () => {
    expect(shouldAdoptRemoteBody("   \n ", "<p>real content</p>", false)).toBe(true);
  });

  it("tolerates null and undefined on either side", () => {
    expect(shouldAdoptRemoteBody(null, "<p>hi</p>", false)).toBe(true);
    expect(shouldAdoptRemoteBody(undefined, "<p>hi</p>", false)).toBe(true);
    expect(shouldAdoptRemoteBody("", null, false)).toBe(false);
    expect(shouldAdoptRemoteBody("", undefined, false)).toBe(false);
  });

  it("never overwrites a local body the user has typed into", () => {
    expect(shouldAdoptRemoteBody("<p>my notes</p>", "<p>theirs</p>", false)).toBe(false);
  });

  it("never overwrites while a save is queued, even if local looks empty", () => {
    // The user cleared the body deliberately and the debounced save hasn't fired
    // yet. Adopting here would resurrect content they just deleted.
    expect(shouldAdoptRemoteBody("", "<p>stale remote</p>", true)).toBe(false);
  });

  it("does not adopt an empty remote body over an empty local one", () => {
    // No-op case: must return false so the caller can bail out of setDraft and
    // avoid an identity-changing state write on every pull.
    expect(shouldAdoptRemoteBody("", "", false)).toBe(false);
    expect(shouldAdoptRemoteBody("", "  ", false)).toBe(false);
  });
});

describe("isReplaceableTitle", () => {
  it("owns an unnamed note", () => {
    expect(isReplaceableTitle("")).toBe(true);
    expect(isReplaceableTitle("   ")).toBe(true);
    expect(isReplaceableTitle(null)).toBe(true);
    expect(isReplaceableTitle(undefined)).toBe(true);
  });

  it("owns a timestamp it wrote itself", () => {
    expect(isReplaceableTitle("Recording 19 Aug 14:32")).toBe(true);
    expect(isReplaceableTitle("Recording 3 Aug 09:05")).toBe(true);
  });

  it("owns the import fallback but not a real filename", () => {
    expect(isReplaceableTitle("Imported audio")).toBe(true);
    expect(isReplaceableTitle("standup_2026-07-09")).toBe(false);
  });

  it("never owns a title a human wrote", () => {
    expect(isReplaceableTitle("Recording")).toBe(false);
    expect(isReplaceableTitle("Recording kickoff with Hege")).toBe(false);
    expect(isReplaceableTitle("Standup 19 Aug 14:32")).toBe(false);
  });
});

describe("shouldAdoptRemoteTitle", () => {
  it("adopts a generated title over the timestamp it replaces", () => {
    expect(shouldAdoptRemoteTitle("Recording 19 Aug 14:32", "Kickoff with Hege", false)).toBe(true);
  });

  it("adopts into an untitled note", () => {
    expect(shouldAdoptRemoteTitle("", "Kickoff with Hege", false)).toBe(true);
  });

  it("never overwrites a title the user typed", () => {
    expect(shouldAdoptRemoteTitle("My own title", "Kickoff with Hege", false)).toBe(false);
  });

  it("never overwrites an edit still queued for save", () => {
    // The user is mid-rename; the debounced save hasn't fired yet.
    expect(shouldAdoptRemoteTitle("Recording 19 Aug 14:32", "Kickoff with Hege", true)).toBe(false);
  });

  it("does not adopt an empty remote title", () => {
    expect(shouldAdoptRemoteTitle("Recording 19 Aug 14:32", "", false)).toBe(false);
    expect(shouldAdoptRemoteTitle("", "  ", false)).toBe(false);
  });
});

describe("shouldRequestTitleForBody", () => {
  const typed = {
    title: "",
    bodyText: "x".repeat(TITLE_MIN_BODY_CHARS),
    hasTranscript: false,
    recording: false,
    readOnly: false,
  };

  it("titles a note that was typed and never recorded", () => {
    expect(shouldRequestTitleForBody(typed)).toBe(true);
  });

  it("stands down while a recording is in flight", () => {
    // No model call during a capture — it would spend GPU exactly when local
    // Whisper needs it.
    expect(shouldRequestTitleForBody({ ...typed, recording: true })).toBe(false);
  });

  it("leaves a recorded note to the post-stop chain", () => {
    expect(shouldRequestTitleForBody({ ...typed, hasTranscript: true })).toBe(false);
  });

  it("does not touch a title the user owns", () => {
    expect(shouldRequestTitleForBody({ ...typed, title: "My own title" })).toBe(false);
  });

  it("waits for enough body to name", () => {
    expect(shouldRequestTitleForBody({ ...typed, bodyText: "hi" })).toBe(false);
    expect(
      shouldRequestTitleForBody({ ...typed, bodyText: " ".repeat(400) }),
    ).toBe(false);
  });

  it("never fires on a note the user can't edit", () => {
    expect(shouldRequestTitleForBody({ ...typed, readOnly: true })).toBe(false);
  });
});

// The Rust side parses this shape with chrono, so a loose regex here would have
// the client adopt over a title the backend calls user-owned.
describe("isReplaceableTitle agrees with the Rust predicate on near misses", () => {
  it("rejects a timestamp shape with impossible values", () => {
    expect(isReplaceableTitle("Recording 99 Aug 14:32")).toBe(false);
    expect(isReplaceableTitle("Recording 19 Zzz 14:32")).toBe(false);
    expect(isReplaceableTitle("Recording 19 Aug 99:32")).toBe(false);
    expect(isReplaceableTitle("Recording 19 Aug 14:99")).toBe(false);
  });

  it("still accepts every month it actually writes", () => {
    for (const m of ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"]) {
      expect(isReplaceableTitle(`Recording 1 ${m} 00:00`)).toBe(true);
    }
  });
});
