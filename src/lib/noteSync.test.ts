import { describe, it, expect } from "vitest";
import { shouldAdoptRemoteBody } from "./noteSync";

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
