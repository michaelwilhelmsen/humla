import { describe, it, expect, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { RenameYouRow } from "./RenameYouRow";
import { mockTauri } from "../../../test/tauri";
import { useNotesStore } from "../../../lib/store";
import { useCloudStore } from "../../../lib/cloud";
import { makeNote } from "../../../test/fixtures";

const note = (id: string, transcript: string) => makeNote({ id, transcript });

// The `You` case of the cross-note rename (#116 part 2), as a count-gated row.
//
// It renders only when there is something to fix, so it never existed for a new
// install and disappears for good once used — the self-retiring shape #122's
// rebuild row uses. It sits in Transcription rather than Chat because the row is
// about what transcripts *say*, and that is where someone whose transcript reads
// "You:" would look.

function seed(notes: ReturnType<typeof makeNote>[]) {
  useNotesStore.setState({ notes });
}

beforeEach(() => {
  mockTauri({ speaker_default_name: () => "Michael Wilhelmsen" });
  seed([]);
});

describe("RenameYouRow gating", () => {
  it("renders nothing when no transcript says You", async () => {
    seed([note("a", "Michael: hi\nHege: hello")]);
    const { container } = render(<RenameYouRow />);
    // Never existed for a new install; gone for good once used.
    await waitFor(() => expect(container).toBeEmptyDOMElement());
  });

  it("renders nothing for an empty library", async () => {
    const { container } = render(<RenameYouRow />);
    await waitFor(() => expect(container).toBeEmptyDOMElement());
  });

  it("appears once a transcript carries the literal You label", async () => {
    seed([note("a", "You: hi\nSpeaker 2: hello")]);
    render(<RenameYouRow />);
    expect(await screen.findByText(/Speaker labelled "You"/i)).toBeTruthy();
  });
});

describe("RenameYouRow prefill", () => {
  it("prefills the resolved name, editably", async () => {
    seed([note("a", "You: hi")]);
    render(<RenameYouRow />);
    const field = await screen.findByRole("textbox", { name: /rename you to/i });
    expect(field).toHaveValue("Michael Wilhelmsen");

    // Editable on purpose: Personal falls back to the macOS name, which can be
    // "admin". An uneditable prefill would leave that user stuck.
    await userEvent.clear(field);
    await userEvent.type(field, "Michael W");
    expect(field).toHaveValue("Michael W");
  });

  it("states the exact string it will write, and the count", async () => {
    seed([note("a", "You: hi"), note("b", "You: there"), note("c", "Hege: no You here")]);
    render(<RenameYouRow />);
    expect(
      await screen.findByRole("button", { name: /Rename You → Michael Wilhelmsen in 2 notes/ }),
    ).toBeTruthy();
  });

  it("tracks the count as the note set changes", async () => {
    seed([note("a", "You: hi")]);
    render(<RenameYouRow />);
    expect(await screen.findByRole("button", { name: /in 1 note$/ })).toBeTruthy();
  });

  it("cannot be run with an empty name", async () => {
    seed([note("a", "You: hi")]);
    render(<RenameYouRow />);
    const field = await screen.findByRole("textbox", { name: /rename you to/i });
    await userEvent.clear(field);
    expect(screen.getByRole("button", { name: /^Rename You/ })).toBeDisabled();
  });

  it("falls back to an empty field when no name resolves", async () => {
    mockTauri({ speaker_default_name: () => null });
    seed([note("a", "You: hi")]);
    render(<RenameYouRow />);
    const field = await screen.findByRole("textbox", { name: /rename you to/i });
    expect(field).toHaveValue("");
    // Nothing to write yet, so the action stays out of reach rather than
    // offering to rename "You" to nothing.
    expect(screen.getByRole("button", { name: /^Rename You/ })).toBeDisabled();
  });
});

describe("RenameYouRow running", () => {
  it("rewrites every affected note and reports how many", async () => {
    const updated: Array<[string, unknown]> = [];
    mockTauri({
      speaker_default_name: () => "Michael Wilhelmsen",
      notes_update: (args) => {
        const a = args as { id: string; patch: unknown };
        updated.push([a.id, a.patch]);
        return undefined;
      },
    });
    seed([note("a", "You: hi"), note("b", "Speaker 1: x\nYou: there")]);
    render(<RenameYouRow />);

    await userEvent.click(
      await screen.findByRole("button", { name: /Rename You → Michael Wilhelmsen in 2 notes/ }),
    );

    await waitFor(() => expect(updated).toHaveLength(2));
    expect(updated).toEqual([
      ["a", { transcript: "Michael Wilhelmsen: hi" }],
      ["b", { transcript: "Speaker 1: x\nMichael Wilhelmsen: there" }],
    ]);
    expect(await screen.findByText(/Renamed in 2 notes/)).toBeTruthy();
  });

  it("writes the edited name, not the prefilled one", async () => {
    const updated: Array<[string, unknown]> = [];
    mockTauri({
      speaker_default_name: () => "admin",
      notes_update: (args) => {
        const a = args as { id: string; patch: unknown };
        updated.push([a.id, a.patch]);
        return undefined;
      },
    });
    seed([note("a", "You: hi")]);
    render(<RenameYouRow />);

    const field = await screen.findByRole("textbox", { name: /rename you to/i });
    await userEvent.clear(field);
    await userEvent.type(field, "Michael Wilhelmsen");
    await userEvent.click(screen.getByRole("button", { name: /^Rename You/ }));

    await waitFor(() => expect(updated).toHaveLength(1));
    expect(updated[0][1]).toEqual({ transcript: "Michael Wilhelmsen: hi" });
  });

  it("says how many failed rather than reporting success", async () => {
    mockTauri({
      speaker_default_name: () => "Michael",
      notes_update: (args) => {
        if ((args as { id: string }).id === "b") throw new Error("write failed");
        return undefined;
      },
    });
    seed([note("a", "You: hi"), note("b", "You: there")]);
    render(<RenameYouRow />);

    await userEvent.click(await screen.findByRole("button", { name: /^Rename You/ }));
    expect(await screen.findByText(/Renamed in 1 of 2 — 1 failed/)).toBeTruthy();
  });
});

describe("RenameYouRow failure recovery", () => {
  it("offers a retry covering only the notes that failed", async () => {
    const attempts: string[] = [];
    let failing = true;
    mockTauri({
      speaker_default_name: () => "Michael",
      notes_update: (args) => {
        const id = (args as { id: string }).id;
        attempts.push(id);
        if (failing && id === "b") throw new Error("write failed");
        return undefined;
      },
    });
    seed([note("a", "You: hi"), note("b", "You: there")]);
    render(<RenameYouRow />);

    await userEvent.click(await screen.findByRole("button", { name: /^Rename You/ }));
    const retry = await screen.findByRole("button", { name: /Retry 1/ });

    failing = false;
    attempts.length = 0;
    await userEvent.click(retry);

    // Only the failure is retried, not the notes that already succeeded.
    await waitFor(() => expect(attempts).toEqual(["b"]));
    expect(await screen.findByText(/Renamed in 1 note$/)).toBeTruthy();
  });

  it("does not dress a partial failure as success", async () => {
    mockTauri({
      speaker_default_name: () => "Michael",
      notes_update: () => {
        throw new Error("write failed");
      },
    });
    seed([note("a", "You: hi")]);
    render(<RenameYouRow />);

    await userEvent.click(await screen.findByRole("button", { name: /^Rename You/ }));
    const message = await screen.findByText(/Renamed in 0 of 1/);
    expect(message.className).toContain("warning-text");
  });
});

describe("RenameYouRow permissions", () => {
  it("is absent for a viewer, who cannot write notes at all", async () => {
    useCloudStore.setState({
      status: {
        ...useCloudStore.getState().status,
        current_workspace: { id: "ws", name: "K2", role: "viewer", plan_status: "active" },
      },
    });
    seed([note("a", "You: hi")]);
    const { container } = render(<RenameYouRow />);
    // Offering it would rewrite every pill optimistically, then fail every write.
    await waitFor(() => expect(container).toBeEmptyDOMElement());
    useCloudStore.setState({
      status: { ...useCloudStore.getState().status, current_workspace: null },
    });
  });
});
