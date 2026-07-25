import { describe, it, expect } from "vitest";
import { NOTE_PROMPTS, opensPromptPicker } from "./chatPrompts";

describe("NOTE_PROMPTS", () => {
  it("is a non-empty set with a label, description and prompt on every entry", () => {
    expect(NOTE_PROMPTS.length).toBeGreaterThan(0);
    for (const p of NOTE_PROMPTS) {
      expect(p.label.trim()).not.toBe("");
      expect(p.description.trim()).not.toBe("");
      expect(p.prompt.trim()).not.toBe("");
    }
  });

  it("has unique labels, so popover rows can key on them", () => {
    const labels = NOTE_PROMPTS.map((p) => p.label);
    expect(new Set(labels).size).toBe(labels.length);
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
