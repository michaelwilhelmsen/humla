import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { screen, waitFor } from "@testing-library/react";
import { act } from "react";
import userEvent from "@testing-library/user-event";
import { renderApp } from "../test/app";
import { makeNote } from "../test/fixtures";
import { mockLayoutBox } from "../test/layout";
import { useRecordingStore } from "../lib/store";
import type { Note, NoteSession } from "../lib/ipc";

// #146. A take captured with "Transcribe manually" on holds its audio and no
// text. The action's *presence* is the only signal in v1 — there is no badge on
// the take carousel — so what it keys off, and what it does while running, is
// the whole contract of the feature at this layer.

beforeAll(() => mockLayoutBox());
beforeEach(() => {
  // Module-singleton stores: a note left mid-transcription by one test would
  // still be mid-transcription in the next.
  useRecordingStore.setState({ transcribing: {} });
});

function session(over: Partial<NoteSession>): NoteSession {
  return {
    id: "s1",
    index: 1,
    startedAt: "",
    durationMs: 1000,
    streams: ["mic"],
    hasPlayback: true,
    canTranscribe: false,
    ...over,
  };
}

function open(
  sessions: NoteSession[],
  handlers: Record<string, (args: unknown) => unknown> = {},
) {
  const note: Note = makeNote({ id: "n1", title: "Standup" });
  renderApp("/note/n1", {
    notes_list: () => [note],
    notes_get: () => note,
    note_timeline: () => [],
    note_sessions: () => sessions,
    ...handlers,
  });
  return note;
}

function transcribeButton() {
  return screen.queryByRole("button", { name: /^transcribe/i });
}

describe("the note's Transcribe action", () => {
  it("stays hidden for a note whose takes are all transcribed", async () => {
    open([session({})]);
    await screen.findByRole("button", { name: /summarize/i });
    expect(transcribeButton()).not.toBeInTheDocument();
  });

  it("appears when a take is still holding untranscribed audio", async () => {
    open([session({ canTranscribe: true })]);
    expect(await screen.findByRole("button", { name: /^transcribe$/i })).toBeInTheDocument();
  });

  // The backend answers both halves of "would this do anything" — untranscribed
  // AND audio still on disk. A take swept away by Settings → Delete stored
  // audio can never produce text, so offering the action would leave a button
  // that fails forever.
  it("stays hidden for an untranscribed take whose audio is gone", async () => {
    open([session({ canTranscribe: false })]);
    await screen.findByRole("button", { name: /summarize/i });
    expect(transcribeButton()).not.toBeInTheDocument();
  });

  it("invokes the deferred transcription and reports progress on the button", async () => {
    const transcribe = vi.fn(() => null);
    open([session({ canTranscribe: true })], {
      transcribe_note: transcribe,
    });
    const button = await screen.findByRole("button", { name: /^transcribe$/i });

    await userEvent.click(button);
    expect(transcribe).toHaveBeenCalledWith({ noteId: "n1" });

    // The backend brackets the replay with `transcribe_status`; the button
    // reads that rather than any local flag, so a replay started elsewhere
    // (the menu bar, another window) shows here too.
    act(() => useRecordingStore.getState().setTranscribing("n1", true));
    const busy = await screen.findByRole("button", { name: /transcribing/i });
    expect(busy).toBeDisabled();
  });

  // Per-note, not the shared recording slot: a replay on this note must not be
  // reported for another, and vice versa.
  it("ignores a transcription running on a different note", async () => {
    open([session({ canTranscribe: true })]);
    await screen.findByRole("button", { name: /^transcribe$/i });

    act(() => useRecordingStore.getState().setTranscribing("other", true));

    expect(
      await screen.findByRole("button", { name: /^transcribe$/i }),
    ).toBeEnabled();
  });

  it("surfaces a refusal as an error rather than a dead click", async () => {
    open([session({ canTranscribe: true })], {
      transcribe_note: () => {
        throw new Error("This note is recording — stop the recording first.");
      },
    });
    const button = await screen.findByRole("button", { name: /^transcribe$/i });

    await userEvent.click(button);

    await waitFor(() =>
      expect(
        useRecordingStore
          .getState()
          .errors.some((e) => /stop the recording first/.test(e.message)),
      ).toBe(true),
    );
  });
});
