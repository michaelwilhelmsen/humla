import { describe, it, expect } from "vitest";
import { htmlToText, noteExcerpt, noteState } from "./noteList";
import { makeNote } from "../test/fixtures";

describe("htmlToText", () => {
  it("separates block elements so paragraphs don't run together", () => {
    expect(htmlToText("<p>Per sete er dyrere.</p><p>Kanskje 3 gratis?</p>")).toBe(
      "Per sete er dyrere. Kanskje 3 gratis? ",
    );
  });

  it("separates hard breaks — Tiptap writes Shift+Enter as <br>", () => {
    expect(htmlToText("<p>første linje<br>andre linje</p>").trim()).toBe(
      "første linje andre linje",
    );
    expect(htmlToText("<p>a<br />b</p>").trim()).toBe("a b");
  });

  it("keeps inline markup unspaced", () => {
    expect(htmlToText("<p>en <strong>viktig</strong> ting</p>").trim()).toBe(
      "en viktig ting",
    );
  });

  it("is empty for empty input", () => {
    expect(htmlToText("")).toBe("");
    expect(htmlToText(null)).toBe("");
  });
});

describe("noteExcerpt", () => {
  it("prefers the summary's first paragraph", () => {
    const n = makeNote({
      id: "a",
      summary: "# Oppsummering\n\nNordvik vil flytte arkivet.\n\n- Tre puljer",
      body: "<p>ignorert</p>",
      transcript: "Michael: ignorert",
    });
    expect(noteExcerpt(n)).toBe("Nordvik vil flytte arkivet.");
  });

  it("falls back to the first bullet when the summary is all list", () => {
    const n = makeNote({ id: "a", summary: "## Punkter\n- **Tre** puljer\n- Pris uendret" });
    expect(noteExcerpt(n)).toBe("Tre puljer");
  });

  it("falls back to the typed body when there is no summary", () => {
    const n = makeNote({ id: "a", body: "<p>Første.</p><p>Andre.</p>" });
    expect(noteExcerpt(n)).toBe("Første. Andre.");
  });

  it("falls back to the transcript with speaker labels stripped", () => {
    const n = makeNote({ id: "a", transcript: "Michael: Hei.\nHege: Hallo." });
    expect(noteExcerpt(n)).toBe("Hei. Hallo.");
  });

  it("is empty for an empty note", () => {
    expect(noteExcerpt(makeNote({ id: "a" }))).toBe("");
  });
});

describe("noteState", () => {
  it("reports the furthest stage the note has reached", () => {
    expect(noteState(makeNote({ id: "a", summary: "s", transcript: "t" }))).toBe("summarized");
    expect(noteState(makeNote({ id: "a", transcript: "t", body: "<p>b</p>" }))).toBe("recorded");
    expect(noteState(makeNote({ id: "a", body: "<p>b</p>" }))).toBe("notes");
    expect(noteState(makeNote({ id: "a" }))).toBe("empty");
  });

  it("does not count an empty HTML body as typed notes", () => {
    expect(noteState(makeNote({ id: "a", body: "<p></p>" }))).toBe("empty");
  });
});
