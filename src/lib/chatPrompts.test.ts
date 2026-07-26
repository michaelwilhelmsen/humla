import { describe, it, expect } from "vitest";
import { LIBRARY_PROMPTS, NOTE_PROMPTS, opensPromptPicker, promptsFor } from "./chatPrompts";

describe.each([
  ["NOTE_PROMPTS", NOTE_PROMPTS],
  ["LIBRARY_PROMPTS", LIBRARY_PROMPTS],
])("%s", (_name, set) => {
  it("is a non-empty set with a label, description and prompt on every entry", () => {
    expect(set.length).toBeGreaterThan(0);
    for (const p of set) {
      expect(p.label.trim()).not.toBe("");
      expect(p.description.trim()).not.toBe("");
      expect(p.prompt.trim()).not.toBe("");
    }
  });

  it("has unique labels, so popover rows can key on them", () => {
    const labels = set.map((p) => p.label);
    expect(new Set(labels).size).toBe(labels.length);
  });
});

describe("LIBRARY_PROMPTS", () => {
  it("is one-click: no prompt carries a placeholder to edit before sending", () => {
    // #82 originally proposed "…for a given client?", which forced an edit the
    // other three didn't (#95 changed it). Nothing here should regress to that.
    for (const p of LIBRARY_PROMPTS) {
      expect(p.prompt).not.toMatch(/\ba given\b|\[|\{|<|xxx|TODO/i);
    }
  });

  it("asks across notes rather than about one, which is the point of the surface", () => {
    // Each prompt names a scope wider than a single note; none says "this".
    for (const p of LIBRARY_PROMPTS) {
      expect(p.prompt).not.toMatch(/\bthis (meeting|note|call)\b/i);
    }
  });
});

describe("promptsFor", () => {
  it("gives a note pane the note set and /chat the library set", () => {
    expect(promptsFor({ kind: "note", noteId: "n1" })).toBe(NOTE_PROMPTS);
    expect(promptsFor({ kind: "global" })).toBe(LIBRARY_PROMPTS);
  });
});

describe("opensPromptPicker", () => {
  it("opens on a slash typed into an empty composer", () => {
    expect(opensPromptPicker("/", "")).toBe(true);
  });

  it("ignores a slash typed mid-sentence, so 'and/or' and paths still work", () => {
    expect(opensPromptPicker("/", "and")).toBe(false);
    expect(opensPromptPicker("/", "~")).toBe(false);
  });

  it("ignores every other key", () => {
    expect(opensPromptPicker("a", "")).toBe(false);
    expect(opensPromptPicker("Enter", "")).toBe(false);
  });
});
