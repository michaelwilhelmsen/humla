import { describe, it, expect } from "vitest";
import { SECTIONS, SECTION_GROUPS, resolveSectionId } from "./sections";

describe("resolveSectionId", () => {
  it("maps every legacy tab id to its new section", () => {
    expect(resolveSectionId("general")).toBe("general");
    expect(resolveSectionId("about")).toBe("general");
    expect(resolveSectionId("account")).toBe("account");
    expect(resolveSectionId("organization")).toBe("account");
    expect(resolveSectionId("transcription")).toBe("transcription");
    expect(resolveSectionId("keys")).toBe("transcription");
    expect(resolveSectionId("summary")).toBe("summaries");
    expect(resolveSectionId("prompts")).toBe("templates");
    expect(resolveSectionId("summary-prompts")).toBe("templates");
    expect(resolveSectionId("custom-vocabulary")).toBe("vocabulary");
  });

  it("passes through current section ids", () => {
    for (const s of SECTIONS) {
      expect(resolveSectionId(s.id)).toBe(s.id);
    }
  });

  it("falls back to the first section when the id is missing or unknown", () => {
    expect(resolveSectionId(null)).toBe(SECTIONS[0].id);
    expect(resolveSectionId("nonsense")).toBe(SECTIONS[0].id);
  });
});

describe("SECTIONS", () => {
  it("gives every section its own icon", () => {
    const icons = SECTIONS.map((s) => s.icon);
    expect(icons.every(Boolean)).toBe(true);
    expect(new Set(icons).size).toBe(icons.length);
  });
});

describe("SECTION_GROUPS", () => {
  it("puts every section in exactly one group, in sidebar order", () => {
    expect(SECTION_GROUPS.map((g) => g.label)).toEqual(["Content", "Setup", "Account"]);
    expect(SECTION_GROUPS.flatMap((g) => g.sections.map((s) => s.id))).toEqual(
      SECTIONS.map((s) => s.id),
    );
    expect(new Set(SECTIONS.map((s) => s.id)).size).toBe(SECTIONS.length);
  });
});
