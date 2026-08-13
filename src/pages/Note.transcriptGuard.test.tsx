import { beforeAll, describe, expect, it, vi } from "vitest";
import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { renderApp } from "../test/app";
import { makeNote } from "../test/fixtures";
import { mockLayoutBox } from "../test/layout";
import type { TimelineEntry } from "../lib/ipc";

// #169: the styled reader renders from the merged timelines, so transcript
// text no timeline carries is simply not drawn. Repair-on-open closes the
// shapes it can explain; these cover what happens on either side of that.

// Both readers virtualize their lines, and jsdom pins offsetWidth/Height to 0
// — a zero-height scroll window renders no rows at all.
beforeAll(() => mockLayoutBox());

const ORPHAN = "we kicked off by agreeing the deadline slips a week";
const COVERED = "so where did we land on the freeze";

function entry(p: Partial<TimelineEntry> = {}): TimelineEntry {
  return {
    start_ms: 0,
    end_ms: 1_000,
    label: "Speaker 1",
    text: COVERED,
    words: [],
    sessionId: "s1",
    sessionIndex: 1,
    chunkIdx: 0,
    ...p,
  };
}

async function openTranscriptTab(
  handlers: Record<string, (args: unknown) => unknown>,
) {
  const note = makeNote({
    id: "n1",
    title: "Weekly sync",
    transcript: `${ORPHAN}\nSpeaker 1: ${COVERED}`,
  });
  renderApp("/note/n1", {
    notes_list: () => [note],
    notes_get: () => note,
    note_timeline: () => [entry()],
    ...handlers,
  });
  await userEvent.click(await screen.findByRole("button", { name: /transcript/i }));
}

describe("transcript reader guard (#169)", () => {
  it("shows the whole transcript plainly when the timeline can't account for it", async () => {
    await openTranscriptTab({
      note_timeline_repair: () => ({ repaired: false, coversTranscript: false }),
    });
    // The orphaned line is the point: the turn list would omit it entirely.
    expect(await screen.findByText(new RegExp(ORPHAN))).toBeInTheDocument();
    expect(screen.getByText(new RegExp(COVERED))).toBeInTheDocument();
    // And the reader says why playback highlighting is gone, rather than
    // leaving the note looking like an ordinary one that lost its player.
    expect(screen.getByText(/no recording timeline behind it/i)).toBeInTheDocument();
  });

  it("keeps the turn list when the timeline covers the transcript", async () => {
    await openTranscriptTab({
      note_timeline_repair: () => ({ repaired: true, coversTranscript: true }),
    });
    await waitFor(() =>
      expect(screen.queryByText(/no recording timeline behind it/i)).toBeNull(),
    );
    expect(screen.getByText(new RegExp(COVERED))).toBeInTheDocument();
  });

  it("re-reads the timeline after a repair, so the synthesized turns render", async () => {
    const timeline = vi
      .fn()
      // Before repair: only the take that was diarized.
      .mockReturnValueOnce([entry()])
      // After repair: the synthesized session in front of it.
      .mockReturnValue([
        entry({ sessionId: "repair", sessionIndex: 0, label: "", text: ORPHAN }),
        entry({ chunkIdx: 0 }),
      ]);
    await openTranscriptTab({
      note_timeline: timeline,
      note_timeline_repair: () => ({ repaired: true, coversTranscript: true }),
    });
    expect(await screen.findByText(new RegExp(ORPHAN))).toBeInTheDocument();
    expect(timeline).toHaveBeenCalledTimes(2);
  });

  it("falls back to the plain reader when the repair call itself fails", async () => {
    await openTranscriptTab({
      note_timeline_repair: () => {
        throw new Error("disk went away");
      },
    });
    // A repair we couldn't run is exactly when the turn list is most likely to
    // be hiding something, so the unknown answer is "not covered".
    expect(await screen.findByText(new RegExp(ORPHAN))).toBeInTheDocument();
    expect(screen.getByText(/no recording timeline behind it/i)).toBeInTheDocument();
  });

  it("does not repair a note that has no timeline at all", async () => {
    const repair = vi.fn(() => ({ repaired: false, coversTranscript: true }));
    await openTranscriptTab({
      note_timeline: () => [],
      note_timeline_repair: repair,
    });
    // Nothing to orphan against, and synthesizing a session here would take
    // the note's free-text editing away.
    await waitFor(() => expect(screen.getByText(new RegExp(ORPHAN))).toBeInTheDocument());
    expect(repair).not.toHaveBeenCalled();
  });
});
