import { act } from "react";
import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { renderApp } from "../test/app";
import { makeNote } from "../test/fixtures";
import { mockLayoutBox } from "../test/layout";
import type { CleanItem, CleanView, NoteSession, TimelineEntry } from "../lib/ipc";
import { useRecordingStore } from "../lib/store";

// #217: the Clean view is the note's clean transcript, paragraphs the user
// can edit and give another speaker. It is offered when there are paragraphs
// to show or a model to clean with.

beforeAll(() => mockLayoutBox());
beforeEach(() => useRecordingStore.setState({ cleaning: {} }));

const VERBATIM = "eh, skal vi, skal vi starte";
const CLEAN = "Skal vi starte?";

function entry(label: string, text: string, startMs: number): TimelineEntry {
  return {
    startMs,
    endMs: startMs + 1_000,
    label,
    text,
    words: [],
    sessionId: "s1",
    sessionIndex: 1,
    chunkIdx: 0,
  };
}

function view(items: CleanItem[], edited = 0): CleanView {
  return {
    takes: [{ sessionId: "s1", items, stale: false }],
    words: 7,
    cleanedWords: 7,
    edited,
    unreadable: false,
  };
}

const paragraph = (text: string, speaker = "Speaker 1", edited = false): CleanItem => ({
  kind: "turn",
  id: `t-${text}`,
  speaker,
  text,
  startMs: 0,
  edited,
});

async function openTranscriptTab({
  available,
  clean,
  onEdit = () => null,
  onClean = () => ({ segments: 0, keptVerbatim: 0 }),
  sessions = [],
}: {
  available: boolean;
  clean: CleanView;
  onEdit?: (args: unknown) => unknown;
  onClean?: (args: unknown) => unknown;
  sessions?: NoteSession[];
}) {
  const note = makeNote({
    id: "n1",
    title: "Weekly sync",
    transcript: `Speaker 1: ${VERBATIM}\nSpeaker 2: ja`,
  });
  renderApp("/note/n1", {
    notes_list: () => [note],
    notes_get: () => note,
    note_timeline: () => [entry("Speaker 1", VERBATIM, 0), entry("Speaker 2", "ja", 2_000)],
    note_timeline_repair: () => ({ repaired: false, coversTranscript: true }),
    note_clean_available: () => available,
    note_clean_view: () => clean,
    note_clean_edit: onEdit,
    note_clean: onClean,
    note_sessions: () => sessions,
  });
  await userEvent.click(await screen.findByRole("button", { name: /transcript/i }));
}

describe("Clean view (#217)", () => {
  it("shows the paragraphs in place of the words said", async () => {
    await openTranscriptTab({ available: true, clean: view([paragraph(CLEAN)]) });
    expect(await screen.findByText(CLEAN)).toBeInTheDocument();
    expect(screen.queryByText(VERBATIM)).toBeNull();
  });

  it("offers Clean for paragraphs it has, with no model to clean with", async () => {
    await openTranscriptTab({ available: false, clean: view([paragraph(CLEAN)]) });
    expect(await screen.findByRole("radiogroup", { name: /transcript version/i })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /clean again/i })).toBeNull();
  });

  it("offers no Clean view with neither paragraphs nor a model", async () => {
    await openTranscriptTab({ available: false, clean: view([]) });
    expect(await screen.findByText(new RegExp(VERBATIM))).toBeInTheDocument();
    expect(screen.queryByRole("radiogroup", { name: /transcript version/i })).toBeNull();
  });

  it("saves an edited paragraph", async () => {
    // The backend keeps what it was sent: the view read back after the save has it.
    const clean = view([paragraph(CLEAN)]);
    const onEdit = vi.fn((args: unknown) => {
      const { edit } = args as { edit: { text: string } };
      clean.takes[0].items[0].text = edit.text;
      return null;
    });
    await openTranscriptTab({ available: true, clean, onEdit });
    await userEvent.click(await screen.findByText(CLEAN));
    const box = screen.getByRole("textbox", { name: "Edit this paragraph" });
    await userEvent.clear(box);
    await userEvent.type(box, "Skal vi starte med Cefor?{Enter}");
    await waitFor(() =>
      expect(onEdit).toHaveBeenCalledWith(
        expect.objectContaining({
          noteId: "n1",
          sessionId: "s1",
          turnId: `t-${CLEAN}`,
          edit: { text: "Skal vi starte med Cefor?" },
        }),
      ),
    );
    expect(screen.getByText("Skal vi starte med Cefor?")).toBeInTheDocument();
  });

  it("leaves a paragraph as it was on Cancel", async () => {
    const onEdit = vi.fn(() => null);
    await openTranscriptTab({ available: true, clean: view([paragraph(CLEAN)]), onEdit });
    await userEvent.click(await screen.findByText(CLEAN));
    await userEvent.type(screen.getByRole("textbox", { name: "Edit this paragraph" }), " Kanskje.");
    await userEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("textbox", { name: "Edit this paragraph" })).toBeNull();
    expect(screen.getByText(CLEAN)).toBeInTheDocument();
    expect(onEdit).not.toHaveBeenCalled();
  });

  it("keeps the player to Verbatim", async () => {
    await openTranscriptTab({ available: true, clean: view([paragraph(CLEAN)]) });
    await screen.findByText(CLEAN);
    const player = screen.getByText(/no audio saved|audio not stored/i);
    expect(player).not.toBeVisible();
    await userEvent.click(screen.getByRole("radio", { name: "Verbatim" }));
    await waitFor(() => expect(player).toBeVisible());
  });

  it("offers the recordings in Verbatim when there is no audio to play", async () => {
    const take = (id: string, index: number): NoteSession => ({
      id,
      index,
      startedAt: "",
      durationMs: 1000,
      streams: ["mic"],
      hasPlayback: false,
      canTranscribe: false,
      canRetranscribe: false,
    });
    await openTranscriptTab({
      available: true,
      clean: view([paragraph(CLEAN)]),
      sessions: [take("s1", 1), take("s2", 2)],
    });
    await userEvent.click(await screen.findByRole("radio", { name: "Verbatim" }));
    expect(await screen.findByRole("combobox", { name: "Recording" })).toBeVisible();
  });

  it("gives a paragraph another speaker when it is saved", async () => {
    const onEdit = vi.fn(() => null);
    await openTranscriptTab({ available: true, clean: view([paragraph(CLEAN)]), onEdit });
    await userEvent.click(await screen.findByText(CLEAN));
    await userEvent.click(screen.getByRole("button", { name: "Speaker: Speaker 1" }));
    await userEvent.click(await screen.findByRole("menuitem", { name: "Speaker 2" }));
    expect(onEdit).not.toHaveBeenCalled();
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() =>
      expect(onEdit).toHaveBeenCalledWith(expect.objectContaining({ edit: { speaker: "Speaker 2" } })),
    );
  });

  it("leaves the speaker as it was on Cancel", async () => {
    const onEdit = vi.fn(() => null);
    await openTranscriptTab({ available: true, clean: view([paragraph(CLEAN)]), onEdit });
    await userEvent.click(await screen.findByText(CLEAN));
    await userEvent.click(screen.getByRole("button", { name: "Speaker: Speaker 1" }));
    await userEvent.click(await screen.findByRole("menuitem", { name: "Speaker 2" }));
    await userEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(onEdit).not.toHaveBeenCalled();
  });

  it("shows a clean pass's progress in Verbatim too", async () => {
    await openTranscriptTab({ available: true, clean: view([paragraph(CLEAN)]) });
    await userEvent.click(await screen.findByRole("radio", { name: "Verbatim" }));
    act(() =>
      useRecordingStore.getState().setCleaning("n1", { active: true, done: 1, total: 4, error: null }),
    );
    expect(await screen.findByText("Cleaning… 25%")).toBeInTheDocument();
  });

  it("shows the language and how many speakers there are", async () => {
    await openTranscriptTab({ available: true, clean: view([paragraph(CLEAN)]) });
    expect(
      await screen.findByRole("button", { name: /transcription settings: .* · 2 speakers/i }),
    ).toBeInTheDocument();
  });

  it("asks before Clean again replaces edited paragraphs", async () => {
    const onClean = vi.fn(() => ({ segments: 1, keptVerbatim: 0 }));
    await openTranscriptTab({
      available: true,
      clean: view([paragraph(CLEAN, "Speaker 1", true)], 1),
      onClean,
    });
    await screen.findByText(CLEAN);
    await userEvent.click(screen.getByRole("button", { name: /transcription settings/i }));
    await userEvent.click(await screen.findByRole("button", { name: "Clean again" }));
    expect(screen.getByText("Clean again replaces the paragraph you edited.")).toBeInTheDocument();
    expect(onClean).not.toHaveBeenCalled();
    await userEvent.click(screen.getByRole("button", { name: "Clean again" }));
    await waitFor(() => expect(onClean).toHaveBeenCalledWith(expect.objectContaining({ request: "all" })));
  });
});
