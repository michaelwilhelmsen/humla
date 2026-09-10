import { afterEach, describe, expect, it } from "vitest";
import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { renderApp } from "../test/app";
import { makeNote } from "../test/fixtures";
import { useRecordingStore, type ReplayRun } from "../lib/store";
import type { RecordingStatus } from "../lib/ipc";

// #182. The stop chain runs for minutes on a local-Whisper meeting, and the
// note view's own bar is off screen the moment the user navigates away — so the
// compact indicator is the one thing every other screen can say it with.

const NOTE = makeNote({ id: "n1", title: "Weekly sync" });

function open(
  path: string,
  status: RecordingStatus,
  transcribing: Record<string, ReplayRun> = {},
) {
  useRecordingStore.setState({ status, transcribing });
  return renderApp(path, { notes_list: () => [NOTE], notes_get: () => NOTE });
}

afterEach(() =>
  useRecordingStore.setState({ status: { noteId: null, phase: "idle" }, transcribing: {} }),
);

describe("the capture indicator off the note (#182)", () => {
  it("reports the drain from a screen that is not the recording's note", async () => {
    open("/all-notes", { noteId: "n1", phase: "stopping", pending: 4, done: 3 });
    const bar = await screen.findByRole("progressbar", { name: "Finishing transcript…" });
    expect(bar).toHaveAttribute("aria-valuenow", "75");
  });

  it("carries the diarize step too, which is all the retired toast ever said", async () => {
    open("/all-notes", { noteId: "n1", phase: "diarizing" });
    const bar = await screen.findByRole("progressbar", { name: "Identifying speakers…" });
    expect(bar).not.toHaveAttribute("aria-valuenow");
  });

  it("shows the elapsed timer while the capture is still running", async () => {
    open("/all-notes", { noteId: "n1", phase: "recording" });
    expect(await screen.findByRole("button", { name: /open the recording/i })).toBeInTheDocument();
    expect(screen.queryByRole("progressbar")).toBeNull();
  });

  it("navigates to the note it belongs to", async () => {
    open("/all-notes", { noteId: "n1", phase: "stopping", pending: 4, done: 3 });
    await userEvent.click(await screen.findByRole("button", { name: /open the recording/i }));
    await waitFor(() => expect(screen.getByTestId("location").textContent).toBe("/note/n1"));
  });

  it("stays out of the way on the recording's own note, where the full bar is", async () => {
    open("/note/n1", { noteId: "n1", phase: "stopping", pending: 4, done: 3 });
    await screen.findByRole("progressbar", { name: "Finishing transcript…" });
    // One bar, not two: the note view's own is the one that shows.
    expect(screen.getAllByRole("progressbar")).toHaveLength(1);
    expect(screen.queryByRole("button", { name: /open the recording/i })).toBeNull();
  });

  it("shows nothing at all when no capture is in flight", async () => {
    open("/all-notes", { noteId: null, phase: "idle" });
    await screen.findByText(/weekly sync/i);
    expect(screen.queryByRole("button", { name: /open the recording/i })).toBeNull();
    expect(screen.queryByRole("progressbar")).toBeNull();
  });
});

// #146. Pressing Transcribe replays retained audio for minutes on local
// Whisper — the longest thing the app does, and the one most likely to be
// running while the user is somewhere else entirely.
describe("the compact indicator during a replay (#146)", () => {
  const RUN: ReplayRun = { startedAt: 1, doneMs: 45_000, totalMs: 180_000 };

  it("follows the user off the note the replay belongs to", async () => {
    open("/all-notes", { noteId: null, phase: "idle" }, { n1: RUN });
    const bar = await screen.findByRole("progressbar", { name: "Transcribing…" });
    expect(bar).toHaveAttribute("aria-valuenow", "25");
  });

  it("names the take when the run has more than one", async () => {
    open("/all-notes", { noteId: null, phase: "idle" }, { n1: { ...RUN, take: 2, takes: 3 } });
    await screen.findByRole("progressbar", { name: "Transcribing take 2 of 3…" });
  });

  // The second half of every take, reported on the replay's own channel.
  it("goes indeterminate for the diarize half, the way a stop's does", async () => {
    open(
      "/all-notes",
      { noteId: null, phase: "idle" },
      { n1: { ...RUN, step: "diarizing", doneMs: 180_000 } },
    );
    const bar = await screen.findByRole("progressbar", { name: "Identifying speakers…" });
    expect(bar).not.toHaveAttribute("aria-valuenow");
    expect(bar).toHaveAttribute("aria-busy", "true");
  });

  it("names the take in the diarize half too", async () => {
    open(
      "/all-notes",
      { noteId: null, phase: "idle" },
      { n1: { ...RUN, step: "diarizing", take: 2, takes: 3 } },
    );
    await screen.findByRole("progressbar", { name: "Identifying speakers in take 2 of 3…" });
  });

  it("is determinate again when the next take starts replaying", async () => {
    open(
      "/all-notes",
      { noteId: null, phase: "idle" },
      { n1: { ...RUN, step: "transcribing", doneMs: 120_000, take: 3, takes: 3 } },
    );
    const bar = await screen.findByRole("progressbar", { name: "Transcribing take 3 of 3…" });
    expect(bar).toHaveAttribute("aria-valuenow", "67");
  });

  it("navigates to the note being transcribed, not to a recording", async () => {
    open("/all-notes", { noteId: null, phase: "idle" }, { n1: RUN });
    await userEvent.click(await screen.findByRole("button", { name: /open the recording/i }));
    await waitFor(() => expect(screen.getByTestId("location").textContent).toBe("/note/n1"));
  });

  it("shows one replay, the most recently started, rather than a stack of pills", async () => {
    // `state.transcribing` is a set server-side: two notes can legitimately
    // replay at once, and the compact indicator has one slot.
    open(
      "/all-notes",
      { noteId: null, phase: "idle" },
      { n1: { ...RUN, take: 1, takes: 2 }, n2: { ...RUN, startedAt: 9, take: 1, takes: 5 } },
    );
    await screen.findByRole("progressbar", { name: "Transcribing take 1 of 5…" });
    expect(screen.getAllByRole("progressbar")).toHaveLength(1);
  });

  it("yields the slot to a live capture, which is the thing that can be lost", async () => {
    open("/all-notes", { noteId: "n1", phase: "stopping", pending: 4, done: 3 }, { n2: RUN });
    await screen.findByRole("progressbar", { name: "Finishing transcript…" });
    expect(screen.getAllByRole("progressbar")).toHaveLength(1);
  });

  it("takes the slot back from a stop that draws nothing", async () => {
    // A deferred stop on one note (#146) and a replay on another can overlap,
    // and the stop has nothing to say — so it must not blank the row.
    open("/all-notes", { noteId: "n1", phase: "stopping", deferred: true }, { n2: RUN });
    await screen.findByRole("progressbar", { name: "Transcribing…" });
  });

  it("draws nothing at all for a deferred stop", async () => {
    open("/all-notes", { noteId: "n1", phase: "stopping", deferred: true });
    await screen.findByText(/weekly sync/i);
    expect(screen.queryByRole("progressbar")).toBeNull();
    expect(screen.queryByRole("button", { name: /open the recording/i })).toBeNull();
  });
});

// Every step of both chains has a name, and a step that counts discrete units
// puts the count in the LABEL and leaves the track indeterminate.
describe("the compact indicator on a named step", () => {
  const RUN: ReplayRun = { startedAt: 1, doneMs: 180_000, totalMs: 180_000 };

  it("names the audio copy and counts the streams in its label", async () => {
    open("/all-notes", {
      noteId: "n1",
      phase: "diarizing",
      step: "saving_audio",
      index: 1,
      count: 2,
    });
    const bar = await screen.findByRole("progressbar", { name: "Saving audio 1/2…" });
    expect(bar).not.toHaveAttribute("aria-valuenow");
  });

  it("leaves the track indeterminate on a counted step, at either end of it", async () => {
    // The counter is emitted BEFORE its unit's work, so a fraction off it
    // would read 100% with the second stream not started, and would retreat to
    // 50% at the next step's boundary.
    open("/all-notes", {
      noteId: "n1",
      phase: "diarizing",
      step: "diarizing",
      index: 1,
      count: 2,
    });
    const first = await screen.findByRole("progressbar", { name: "Identifying speakers 1/2…" });
    expect(first).not.toHaveAttribute("aria-valuenow");
    useRecordingStore.setState({
      status: { noteId: "n1", phase: "diarizing", step: "diarizing", index: 2, count: 2 },
    });
    const last = await screen.findByRole("progressbar", { name: "Identifying speakers 2/2…" });
    expect(last).not.toHaveAttribute("aria-valuenow");
  });

  it("stays indeterminate for a single pass, which counts nothing", async () => {
    open("/all-notes", { noteId: "n1", phase: "diarizing", step: "diarizing" });
    const bar = await screen.findByRole("progressbar", { name: "Identifying speakers…" });
    expect(bar).not.toHaveAttribute("aria-valuenow");
  });

  it("names the playback write, which has nothing to count", async () => {
    open("/all-notes", { noteId: "n1", phase: "diarizing", step: "writing_playback" });
    const bar = await screen.findByRole("progressbar", { name: "Writing playback…" });
    expect(bar).not.toHaveAttribute("aria-valuenow");
  });

  it("names the unify pass, which the diarize step used to hide", async () => {
    open("/all-notes", {
      noteId: "n1",
      phase: "diarizing",
      step: "matching_speakers",
      index: 1,
      count: 2,
    });
    const bar = await screen.findByRole("progressbar", { name: "Matching speakers 1/2…" });
    expect(bar).not.toHaveAttribute("aria-valuenow");
  });

  it("names the unify pass of a replay too, on the replay's own channel", async () => {
    open("/all-notes", { noteId: null, phase: "idle" }, {
      n1: { ...RUN, step: "matching_speakers", take: 3, takes: 3 },
    });
    // The take clause wins over a stream counter: which take of the run the
    // user pressed Transcribe on is the coarser fact.
    await screen.findByRole("progressbar", { name: "Matching speakers in take 3 of 3…" });
  });

  it("shows a single-take replay's stream counter, having no take clause", async () => {
    open("/all-notes", { noteId: null, phase: "idle" }, {
      n1: { ...RUN, step: "diarizing", index: 1, count: 2, take: 1, takes: 1 },
    });
    await screen.findByRole("progressbar", { name: "Identifying speakers 1/2…" });
  });

  it("keeps a deferred stop silent however many steps it would have named", async () => {
    open("/all-notes", {
      noteId: "n1",
      phase: "diarizing",
      deferred: true,
      step: "saving_audio",
      index: 1,
      count: 2,
    });
    await screen.findByText(/weekly sync/i);
    expect(screen.queryByRole("progressbar")).toBeNull();
  });
});
