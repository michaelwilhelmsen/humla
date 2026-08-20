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
    canRetranscribe: false,
    ...over,
  };
}

function open(
  sessions: NoteSession[],
  handlers: Record<string, (args: unknown) => unknown> = {},
  noteOver: Partial<Note> = {},
) {
  const note: Note = makeNote({ id: "n1", title: "Standup", ...noteOver });
  renderApp("/note/n1", {
    notes_list: () => [note],
    notes_get: () => note,
    note_timeline: () => [],
    note_sessions: () => sessions,
    ...handlers,
  });
  return note;
}

/** Open the note and switch the context panel to its Transcript tab. */
async function openTranscriptPanel(
  sessions: NoteSession[],
  handlers: Record<string, (args: unknown) => unknown> = {},
  noteOver: Partial<Note> = {},
) {
  const note = open(sessions, handlers, noteOver);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: /transcript/i }));
  return { note, user };
}

function retranscribeButton() {
  return screen.queryByRole("button", { name: /^re-transcribe$/i });
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
    // "pending" — the toolbar action finishes what a deferred capture left
    // waiting; it must not re-run takes that already have their text.
    expect(transcribe).toHaveBeenCalledWith({ noteId: "n1", scope: "pending" });

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

// The Transcript panel's re-transcribe, mirroring the Summary panel's
// regenerate: the take's audio is still on disk, so a recording that came back
// off the wrong language or the wrong model can be re-run in place.
describe("the transcript panel's re-transcribe control", () => {
  const TRANSCRIPT = "Speaker 1: so where did we land\nSpeaker 2: friday";

  it("re-runs every take that still has its audio, not just the pending ones", async () => {
    const transcribe = vi.fn(() => null);
    const { user } = await openTranscriptPanel(
      // Already transcribed — this is the whole point of the control.
      [session({ canTranscribe: false, canRetranscribe: true })],
      { transcribe_note: transcribe },
      { transcript: TRANSCRIPT },
    );

    await user.click(await screen.findByRole("button", { name: /^re-transcribe$/i }));

    expect(transcribe).toHaveBeenCalledWith({ noteId: "n1", scope: "all" });
  });

  it("stays hidden when no take has its raw streams left", async () => {
    await openTranscriptPanel(
      [session({ canRetranscribe: false })],
      {},
      { transcript: TRANSCRIPT },
    );
    await screen.findByRole("button", { name: /^copy transcript$/i });
    expect(retranscribeButton()).not.toBeInTheDocument();
  });

  // It replaces text the note already has, and a viewer's write would be
  // rejected by the server anyway.
  it("stays hidden on a read-only note", async () => {
    // `readOnly` comes off cloud status, not the note row: a workspace whose
    // plan isn't live locks its notes for everyone.
    const ws = { id: "w1", name: "Acme", role: "member" as const, plan_status: "none" as const };
    await openTranscriptPanel(
      [session({ canRetranscribe: true })],
      {
        cloud_status: () => ({
          configured: true,
          logged_in: true,
          base_url: "https://sync.humla.team",
          user: { id: "u1", email: "m@example.no", name: "Michael", verified: true },
          current_workspace: ws,
          workspaces: [ws],
          billing_enabled: true,
          seat_price_cents: 500,
          seat_currency: "usd",
        }),
        cloud_workspace_members: () => [
          { id: "u1", email: "m@example.no", name: "Michael", role: "member" },
        ],
      },
      { transcript: TRANSCRIPT, workspace_id: "w1" },
    );
    // Copy is still there — reading is what read-only allows.
    await screen.findByRole("button", { name: /^copy transcript$/i });
    expect(retranscribeButton()).not.toBeInTheDocument();
  });

  // It REPLACES a transcript, so on a note whose take was never transcribed it
  // would duplicate the toolbar's Transcribe under a tooltip that isn't true.
  // The toolbar owns that case; this control appears once there is text.
  it("stays hidden on a note that has never been transcribed", async () => {
    await openTranscriptPanel([session({ canTranscribe: true, canRetranscribe: true })]);
    // The panel's own pending copy proves it rendered and is in the state
    // under test — the control is absent by rule, not because nothing mounted.
    await screen.findByText(/hasn't been transcribed yet/i);
    expect(retranscribeButton()).not.toBeInTheDocument();
    // And the toolbar's Transcribe is the affordance for it.
    expect(screen.getByRole("button", { name: /^transcribe$/i })).toBeInTheDocument();
  });

  it("is disabled while a run is in flight on this note", async () => {
    await openTranscriptPanel(
      [session({ canRetranscribe: true })],
      {},
      { transcript: TRANSCRIPT },
    );
    await screen.findByRole("button", { name: /^re-transcribe$/i });

    act(() => useRecordingStore.getState().setTranscribing("n1", true));

    await waitFor(() => expect(retranscribeButton()).toBeDisabled());
  });
});
