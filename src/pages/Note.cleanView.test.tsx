import { beforeAll, describe, expect, it } from "vitest";
import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { renderApp } from "../test/app";
import { makeNote } from "../test/fixtures";
import { mockLayoutBox } from "../test/layout";
import type { TimelineEntry } from "../lib/ipc";

// #217: the Clean view needs a model to clean with. Without one the reader is
// Verbatim and offers no switch to a view that would only say "Not cleaned".

beforeAll(() => mockLayoutBox());

const TEXT = "eh, skal vi, skal vi starte";

function entry(): TimelineEntry {
  return {
    startMs: 0,
    endMs: 1_000,
    label: "Speaker 1",
    text: TEXT,
    words: [],
    sessionId: "s1",
    sessionIndex: 1,
    chunkIdx: 0,
  };
}

async function openTranscriptTab(available: boolean) {
  const note = makeNote({ id: "n1", title: "Weekly sync", transcript: `Speaker 1: ${TEXT}` });
  renderApp("/note/n1", {
    notes_list: () => [note],
    notes_get: () => note,
    note_timeline: () => [entry()],
    note_timeline_repair: () => ({ repaired: false, coversTranscript: true }),
    note_clean_available: () => available,
    note_clean: () => ({ segments: 0, keptVerbatim: 0 }),
  });
  await userEvent.click(await screen.findByRole("button", { name: /transcript/i }));
  await screen.findByText(new RegExp(TEXT));
}

describe("Clean view (#217)", () => {
  it("offers Clean when the note's summary provider can clean", async () => {
    await openTranscriptTab(true);
    await waitFor(() => expect(screen.getByRole("radiogroup", { name: /transcript version/i })).toBeInTheDocument());
  });

  it("offers no Clean view without a model to clean with", async () => {
    await openTranscriptTab(false);
    expect(screen.queryByRole("radiogroup", { name: /transcript version/i })).toBeNull();
    expect(screen.getByText(new RegExp(TEXT))).toBeInTheDocument();
  });
});
