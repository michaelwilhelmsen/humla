import { afterEach, describe, expect, it } from "vitest";
import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { renderApp } from "../test/app";
import { makeNote } from "../test/fixtures";
import { useRecordingStore } from "../lib/store";
import type { RecordingPhase } from "../lib/ipc";

// #182. Both controls stay locked well past the end of the capture — the stop
// chain holds the slot through the drain and the diarize pass, and owns
// `note.transcript` for all of it — so each state has to name what it waits on.

const OTHER = makeNote({ id: "n1", title: "Weekly sync", transcript: "Speaker 1: hei" });

function openNote(phase: RecordingPhase, noteId: string | null) {
  useRecordingStore.setState({ status: { noteId, phase } });
  return renderApp("/note/n1", { notes_list: () => [OTHER], notes_get: () => OTHER });
}

afterEach(() => useRecordingStore.setState({ status: { noteId: null, phase: "idle" } }));

describe("Record's refusal names the capture it is waiting on (#182)", () => {
  it("says the previous recording is finishing while one drains elsewhere", async () => {
    openNote("stopping", "other");
    const record = await screen.findByRole("button", { name: "Record" });
    expect(record).toBeDisabled();
    expect(record).toHaveAttribute("title", "Finishing the previous recording");
  });

  it("says the same through the diarize pass, which is the longer half", async () => {
    openNote("diarizing", "other");
    expect(await screen.findByRole("button", { name: "Record" })).toHaveAttribute(
      "title",
      "Finishing the previous recording",
    );
  });

  it("names a live capture as one rather than as a stop", async () => {
    openNote("recording", "other");
    expect(await screen.findByRole("button", { name: "Record" })).toHaveAttribute(
      "title",
      "Another note is recording",
    );
  });

  it("keeps the shortcut in the tooltip when nothing is in the way", async () => {
    openNote("idle", null);
    const record = await screen.findByRole("button", { name: "Record" });
    expect(record).toBeEnabled();
    expect(record).toHaveAttribute("title", "Record (⌘R)");
  });
});

describe("the transcript lock names the state it is in (#182)", () => {
  async function lockCopy(phase: RecordingPhase) {
    openNote(phase, "n1");
    await userEvent.click(await screen.findByRole("button", { name: /^transcript$/i }));
    const locked = await screen.findByTitle(/^Editing is paused/);
    return locked.getAttribute("title");
  }

  it("reports the tail still landing, not a recording", async () => {
    expect(await lockCopy("stopping")).toBe("Editing is paused while the transcript finishes");
  });

  it("reports the diarize pass by name", async () => {
    expect(await lockCopy("diarizing")).toBe("Editing is paused while speakers are identified");
  });

  it("still says 'while recording' while one is actually running", async () => {
    expect(await lockCopy("recording")).toBe("Editing is paused while recording");
  });

  it("leaves the body editable throughout", async () => {
    const { container } = openNote("diarizing", "n1");
    // The body is the one surface the post-stop chain never rewrites, so it is
    // never gated — Tiptap stays editable while the chain runs.
    await waitFor(() => expect(container.querySelector(".ProseMirror")).not.toBeNull());
    expect(container.querySelector(".ProseMirror")).toHaveAttribute("contenteditable", "true");
  });
});
