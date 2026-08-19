import { beforeAll, describe, expect, it, vi } from "vitest";
import { screen, waitFor } from "@testing-library/react";
import { act } from "react";
import userEvent from "@testing-library/user-event";
import { renderApp } from "../test/app";
import { makeNote } from "../test/fixtures";
import { mockLayoutBox } from "../test/layout";
import { useNotesStore } from "../lib/store";
import type { Note } from "../lib/ipc";

// #90. The automatic titler is a BACKEND write: it lands in the notes store via
// `notes_changed`, with nothing in the Note view having asked for it. Without
// the guarded adopt, the new title reaches the sidebar while the open note's
// title box still reads "Recording 19 Aug 14:32" — the one place the user is
// most likely to be looking.

beforeAll(() => mockLayoutBox());

const TIMESTAMP = "Recording 19 Aug 14:32";

function open(note: Note, handlers: Record<string, (args: unknown) => unknown> = {}) {
  renderApp("/note/n1", {
    notes_list: () => [note],
    notes_get: () => note,
    note_timeline: () => [],
    ...handlers,
  });
}

/** Stand in for the backend write reaching the store, which is what
 * `notes_changed` → `refresh()` produces. */
function backendWroteTitle(note: Note, title: string) {
  act(() => {
    useNotesStore.getState().upsertLocal({ ...note, title });
  });
}

function titleBox() {
  return screen.getByPlaceholderText("New note") as HTMLTextAreaElement;
}

describe("a title generated while its note is open", () => {
  it("appears in the title box without a reload", async () => {
    const note = makeNote({ id: "n1", title: TIMESTAMP });
    open(note);
    await waitFor(() => expect(titleBox()).toHaveValue(TIMESTAMP));

    backendWroteTitle(note, "Kickoff with Hege");

    await waitFor(() => expect(titleBox()).toHaveValue("Kickoff with Hege"));
  });

  it("does not clobber a title the user has typed", async () => {
    const note = makeNote({ id: "n1", title: TIMESTAMP });
    open(note);
    await waitFor(() => expect(titleBox()).toHaveValue(TIMESTAMP));

    await userEvent.clear(titleBox());
    await userEvent.type(titleBox(), "Mine");

    // A generated title racing the user's rename must lose. The backend refuses
    // this write too, but the open view has to hold the line on its own draft.
    backendWroteTitle(note, "Kickoff with Hege");

    await new Promise((r) => setTimeout(r, 20));
    expect(titleBox()).toHaveValue("Mine");
  });
});

describe("⋯ → Regenerate title", () => {
  it("replaces a title the user typed, because they asked", async () => {
    const note = makeNote({ id: "n1", title: "Mine" });
    const generate = vi.fn(() => "Kickoff with Hege");
    open(note, { note_generate_title: generate });

    await waitFor(() => expect(titleBox()).toHaveValue("Mine"));
    await userEvent.click(screen.getByRole("button", { name: /more/i }));
    await userEvent.click(await screen.findByText("Regenerate title"));

    await waitFor(() => expect(titleBox()).toHaveValue("Kickoff with Hege"));
    // `force` is the whole difference between this and the automatic path.
    expect(generate).toHaveBeenCalledWith({ noteId: "n1", force: true });
  });

  it("leaves the title alone, and says so, when the model gives back nothing", async () => {
    const note = makeNote({ id: "n1", title: TIMESTAMP });
    open(note, { note_generate_title: () => null });

    await waitFor(() => expect(titleBox()).toHaveValue(TIMESTAMP));
    await userEvent.click(screen.getByRole("button", { name: /more/i }));
    await userEvent.click(await screen.findByText("Regenerate title"));

    // The user pressed a button, so they are owed an answer — unlike the
    // automatic path, which is silent by design.
    expect(await screen.findByText("Couldn’t write a title")).toBeInTheDocument();
    expect(screen.getByText(/nothing usable, so the title is unchanged/i)).toBeInTheDocument();
    expect(titleBox()).toHaveValue(TIMESTAMP);
  });
});
