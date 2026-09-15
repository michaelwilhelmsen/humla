import { describe, it, expect } from "vitest";
import {
  filterActive,
  htmlToText,
  matchesFilter,
  noteExcerpt,
  noteState,
  NO_FILTER,
  UNASSIGNED,
} from "./noteList";
import { makeNote } from "../test/fixtures";
import type { Note } from "./ipc";

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

  it("steps over a horizontal rule at the top of the summary", () => {
    const n = makeNote({
      id: "a",
      summary: "---\n\nMøtet handlet om ny prismodell for kommunene.",
    });
    expect(noteExcerpt(n)).toBe("Møtet handlet om ny prismodell for kommunene.");
  });

  it("falls through a noise-only summary to the body", () => {
    const n = makeNote({
      id: "a",
      summary: "---",
      body: "<p>Planla permisjonen: tre måneder fra mars, deretter delt.</p>",
    });
    expect(noteExcerpt(n)).toBe("Planla permisjonen: tre måneder fra mars, deretter delt.");
  });

  it("steps over a fragment line to the first real paragraph", () => {
    const n = makeNote({
      id: "a",
      summary: "— Begge\n\nPetter og Michael ble enige om ny posisjonering mot helsesektoren.",
    });
    expect(noteExcerpt(n)).toBe(
      "Petter og Michael ble enige om ny posisjonering mot helsesektoren.",
    );
  });

  it("steps over model preamble to the first real bullet", () => {
    const n = makeNote({
      id: "a",
      summary:
        "Her er møtenotatene basert på dine notater og transkripsjonen:\n- Landet ny pris på lisensavtalen med kommunen",
    });
    expect(noteExcerpt(n)).toBe("Landet ny pris på lisensavtalen med kommunen");
  });

  it("steps over 'Her er …' preamble even without a trailing colon", () => {
    const n = makeNote({
      id: "a",
      summary:
        "Her er et sammendrag av møtet om Stund-appen.\n\nKommunen vil utvide piloten til tre nye avdelinger.",
    });
    expect(noteExcerpt(n)).toBe("Kommunen vil utvide piloten til tre nye avdelinger.");
  });

  it("keeps a weak summary line when the summary holds nothing stronger", () => {
    const n = makeNote({
      id: "a",
      summary: "Her er et sammendrag av møtet mellom Michael og Aldring og Helse.",
      body: "<p>notater som ikke skal vinne over sammendraget</p>",
    });
    expect(noteExcerpt(n)).toBe(
      "Her er et sammendrag av møtet mellom Michael og Aldring og Helse.",
    );
  });

  it("reads em-dash bullets as bullets", () => {
    const n = makeNote({
      id: "a",
      summary: "— Begge parter\n— Enige om ny retning for hele produktlinjen",
    });
    expect(noteExcerpt(n)).toBe("Enige om ny retning for hele produktlinjen");
  });

  it("keeps a label-style opening that ends in content, not a colon", () => {
    const n = makeNote({
      id: "a",
      summary: "Tema: Presentasjon og demo av Stund-appen for Stavanger kommune",
    });
    expect(noteExcerpt(n)).toBe(
      "Tema: Presentasjon og demo av Stund-appen for Stavanger kommune",
    );
  });

  it("prefers the transcript over a fragment body", () => {
    const n = makeNote({
      id: "a",
      body: "<p>— Begge</p>",
      transcript: "Michael: Vi bør flytte lanseringen til november.",
    });
    expect(noteExcerpt(n)).toBe("Vi bør flytte lanseringen til november.");
  });

  it("keeps a fragment body when there is nothing else", () => {
    const n = makeNote({ id: "a", body: "<p>— Begge</p>" });
    expect(noteExcerpt(n)).toBe("— Begge");
  });
});

describe("noteState", () => {
  it("reports the furthest stage the note has reached", () => {
    expect(noteState(makeNote({ id: "a", summary: "s", transcript: "t" }))).toBe("summarized");
    expect(noteState(makeNote({ id: "a", transcript: "t", body: "<p>b</p>" }))).toBe("transcribed");
    expect(noteState(makeNote({ id: "a", body: "<p>b</p>" }))).toBe("notes");
    expect(noteState(makeNote({ id: "a" }))).toBe("empty");
  });

  it("does not count an empty HTML body as typed notes", () => {
    expect(noteState(makeNote({ id: "a", body: "<p></p>" }))).toBe("empty");
  });

  // A deferred take (#146) has audio and no text. Without the flag it is
  // indistinguishable from a note nobody ever recorded.
  it("calls a note with untranscribed audio recorded, over its typed notes", () => {
    expect(noteState(makeNote({ id: "a" }), true)).toBe("recorded");
    expect(noteState(makeNote({ id: "a", body: "<p>b</p>" }), true)).toBe("recorded");
    // Text supersedes it: the take was replayed, the audio is no longer waiting.
    expect(noteState(makeNote({ id: "a", transcript: "t" }), true)).toBe("transcribed");
  });
});

describe("matchesFilter", () => {
  const note = (over: Partial<Note>) => makeNote({ id: "a", ...over });

  it("passes everything when no axis is set", () => {
    expect(filterActive(NO_FILTER)).toBe(false);
    expect(matchesFilter(note({}), NO_FILTER)).toBe(true);
  });

  it("narrows to one state", () => {
    const f = { ...NO_FILTER, state: "summarized" as const };
    expect(matchesFilter(note({ summary: "s" }), f)).toBe(true);
    expect(matchesFilter(note({ transcript: "t" }), f)).toBe(false);
  });

  it("reads the awaiting flag through the state axis", () => {
    const f = { ...NO_FILTER, state: "recorded" as const };
    expect(matchesFilter(note({}), f, { awaiting: true })).toBe(true);
    expect(matchesFilter(note({}), f)).toBe(false);
  });

  it("narrows to one client or to the untagged ones", () => {
    expect(matchesFilter(note({ client_id: "c1" }), { ...NO_FILTER, client: "c1" })).toBe(true);
    expect(matchesFilter(note({ client_id: "c2" }), { ...NO_FILTER, client: "c1" })).toBe(false);
    expect(matchesFilter(note({}), { ...NO_FILTER, client: UNASSIGNED })).toBe(true);
    expect(matchesFilter(note({ client_id: "c1" }), { ...NO_FILTER, client: UNASSIGNED })).toBe(false);
  });

  it("narrows to one folder or to the loose notes", () => {
    expect(matchesFilter(note({ folder_id: "f1" }), { ...NO_FILTER, folder: "f1" })).toBe(true);
    expect(matchesFilter(note({}), { ...NO_FILTER, folder: UNASSIGNED })).toBe(true);
    expect(matchesFilter(note({ folder_id: "f1" }), { ...NO_FILTER, folder: UNASSIGNED })).toBe(false);
  });

  it("counts an unsynced note as the signed-in user's own", () => {
    const mine = { ...NO_FILTER, owner: "u-me" };
    expect(matchesFilter(note({ owner: "" }), mine, { myId: "u-me" })).toBe(true);
    expect(matchesFilter(note({ owner: "u-anna" }), mine, { myId: "u-me" })).toBe(false);
    // Someone else's pin never absorbs an ownerless note.
    expect(matchesFilter(note({ owner: "" }), { ...NO_FILTER, owner: "u-anna" }, { myId: "u-me" })).toBe(
      false,
    );
  });

  it("requires every set axis", () => {
    const f = { ...NO_FILTER, state: "notes" as const, client: "c1" };
    expect(matchesFilter(note({ body: "<p>b</p>", client_id: "c1" }), f)).toBe(true);
    expect(matchesFilter(note({ body: "<p>b</p>", client_id: "c2" }), f)).toBe(false);
  });
});
