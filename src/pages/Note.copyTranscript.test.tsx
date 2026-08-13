import { beforeAll, describe, expect, it } from "vitest";
import { act, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { renderApp } from "../test/app";
import { makeNote } from "../test/fixtures";
import { useNotesStore } from "../lib/store";
import { mockLayoutBox } from "../test/layout";

// #166: the summary panel has a copy control; the transcript panel had none,
// so getting the transcript out meant dragging a selection across a long
// scrolling region.

// Both readers virtualize their lines and jsdom pins offsetWidth/Height to 0.
beforeAll(() => mockLayoutBox());

const TRANSCRIPT = "Speaker 1: so where did we land on the freeze\nSpeaker 2: friday";

// jsdom ships no `navigator.clipboard`; `userEvent.setup()` installs the stub
// this reads back, so the copy assertions below depend on calling it rather
// than on the shared `src/test/setup.ts`.

async function openTranscriptTab(transcript: string) {
  const note = makeNote({ id: "n1", title: "Weekly sync", transcript });
  renderApp("/note/n1", {
    notes_list: () => [note],
    notes_get: () => note,
    note_timeline: () => [],
  });
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: /transcript/i }));
  return user;
}

describe("transcript copy control (#166)", () => {
  it("copies the raw transcript, speaker labels and all", async () => {
    const user = await openTranscriptTab(TRANSCRIPT);

    await user.click(await screen.findByRole("button", { name: /^copy transcript$/i }));

    // The stored string is the payload deliberately: view mode drops the
    // label text in favour of a coloured dot, but a pasted transcript is
    // only useful elsewhere if it says who spoke.
    await waitFor(async () =>
      expect(await navigator.clipboard.readText()).toBe(TRANSCRIPT),
    );
    expect(await screen.findByRole("button", { name: /transcript copied/i })).toBeInTheDocument();
  });

  it("copies the rebuilt transcript after a per-turn edit, not the pre-edit one", async () => {
    // A #170 turn edit rebuilds the string in the backend and it arrives on
    // `transcript_replaced`. The draft deliberately refuses to adopt store
    // transcript updates while idle, so copying `draft.transcript` would hand
    // over the text the reader stopped showing.
    const user = await openTranscriptTab(TRANSCRIPT);
    await screen.findByRole("button", { name: /^copy transcript$/i });

    const rebuilt = "Speaker 1: so where did we land on the freeze\nSpeaker 2: friday, confirmed";
    act(() => useNotesStore.getState().replaceTranscript("n1", rebuilt));

    await user.click(screen.getByRole("button", { name: /^copy transcript$/i }));
    await waitFor(async () =>
      expect(await navigator.clipboard.readText()).toBe(rebuilt),
    );
  });

  it("hides the control when there is no transcript to copy", async () => {
    await openTranscriptTab("");
    // Wait on the panel's own empty state, so the assertion below can't pass
    // merely because the tab hadn't rendered yet.
    expect(await screen.findByText(/no transcript yet/i)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /copy transcript/i })).toBeNull();
  });
});
