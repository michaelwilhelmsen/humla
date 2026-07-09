import { describe, it, expect } from "vitest";
import { SECTIONS, resolveSectionId } from "./sections";

describe("resolveSectionId", () => {
  it("maps every legacy tab id to its new section", () => {
    expect(resolveSectionId("general")).toBe("general");
    expect(resolveSectionId("about")).toBe("general");
    expect(resolveSectionId("account")).toBe("account");
    expect(resolveSectionId("organization")).toBe("account");
    expect(resolveSectionId("transcription")).toBe("transcription");
    expect(resolveSectionId("keys")).toBe("transcription");
    expect(resolveSectionId("summary")).toBe("summaries");
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
