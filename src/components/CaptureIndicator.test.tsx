import { afterEach, describe, expect, it } from "vitest";
import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { renderApp } from "../test/app";
import { makeNote } from "../test/fixtures";
import { useRecordingStore } from "../lib/store";
import type { RecordingStatus } from "../lib/ipc";

// #182. The stop chain is minutes long on a local-Whisper meeting, and the only
// thing that ever said so lived inside the recording note's own view — leaving
// the note meant leaving every indicator behind, which is what made a stop feel
// like it held the whole app. The compact indicator is the same bar in the one
// place every screen can show it, and it replaces the diarize-only toast.

const NOTE = makeNote({ id: "n1", title: "Weekly sync" });

function open(path: string, status: RecordingStatus) {
  useRecordingStore.setState({ status });
  return renderApp(path, { notes_list: () => [NOTE], notes_get: () => NOTE });
}

afterEach(() => useRecordingStore.setState({ status: { noteId: null, phase: "idle" } }));

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
