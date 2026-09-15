import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router-dom";
import { AllNotes } from "./AllNotes";
import { LocationProbe } from "../test/app";
import { mockTauri } from "../test/tauri";
import { useNotesStore, useRecordingStore } from "../lib/store";
import { useCloudStore } from "../lib/cloud";
import type { Folder, Note } from "../lib/ipc";

// Notes all created "now" so they land in the same "Today" group and render in
// insertion order — which fixes the visual order that shift-range relies on.
function makeNote(id: string, title: string): Note {
  return {
    id,
    title,
    body: "",
    transcript: "",
    summary: "",
    audio_path: null,
    summary_preset: "meeting",
    folder_id: null,
    language: "en",
    summary_provider: "",
    expected_speakers: null,
    detected_language: null,
    created_at: Date.now(),
    updated_at: Date.now(),
    owner: "",
    workspace_id: "",
    private: false,
  };
}

function seed(notes: Note[], folders: Folder[] = []) {
  useNotesStore.setState({ notes, folders });
}

function renderAll() {
  return render(
    <MemoryRouter initialEntries={["/"]}>
      <AllNotes />
      <LocationProbe />
    </MemoryRouter>,
  );
}

const loc = () => screen.getByTestId("location").textContent;

beforeEach(() => {
  useNotesStore.setState({ notes: [], folders: [] });
  useRecordingStore.setState({ errors: [] });
});

describe("AllNotes selection", () => {
  it("cmd-click toggles a row's selection without navigating", async () => {
    mockTauri();
    seed([makeNote("n1", "Alpha"), makeNote("n2", "Beta")]);
    renderAll();

    const row = screen.getByRole("link", { name: /Alpha/ });
    fireEvent.click(row, { metaKey: true });
    expect(loc()).toBe("/"); // no navigation
    expect(row).toHaveAttribute("data-selected", "true");

    fireEvent.click(row, { metaKey: true }); // toggle off
    expect(row).not.toHaveAttribute("data-selected");
    expect(loc()).toBe("/");
  });

  it("exposes selection via aria-selected, not colour alone", async () => {
    mockTauri();
    seed([makeNote("n1", "Alpha"), makeNote("n2", "Beta")]);
    renderAll();

    const row = screen.getByRole("link", { name: /Alpha/ });
    // Programmatically determinable both before and after selecting.
    expect(row).toHaveAttribute("aria-selected", "false");
    fireEvent.click(row, { metaKey: true });
    expect(row).toHaveAttribute("aria-selected", "true");
  });

  it("Space toggles selection on a focused row without navigating", async () => {
    mockTauri();
    seed([makeNote("n1", "Alpha"), makeNote("n2", "Beta")]);
    renderAll();

    const row = screen.getByRole("link", { name: /Alpha/ });
    fireEvent.keyDown(row, { key: " " });
    expect(row).toHaveAttribute("aria-selected", "true");
    expect(loc()).toBe("/"); // Space selects, never navigates

    fireEvent.keyDown(row, { key: " " }); // toggle back off
    expect(row).toHaveAttribute("aria-selected", "false");
    expect(loc()).toBe("/");
  });

  it("Shift+Space extends a keyboard selection into a range", async () => {
    mockTauri();
    seed([makeNote("n1", "Alpha"), makeNote("n2", "Beta"), makeNote("n3", "Gamma")]);
    renderAll();

    fireEvent.keyDown(screen.getByRole("link", { name: /Alpha/ }), { key: " " });
    fireEvent.keyDown(screen.getByRole("link", { name: /Gamma/ }), {
      key: " ",
      shiftKey: true,
    });

    expect(screen.getByRole("link", { name: /Alpha/ })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByRole("link", { name: /Beta/ })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByRole("link", { name: /Gamma/ })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByText("3 selected")).toBeInTheDocument();
  });

  it("ctrl-click does NOT toggle selection (macOS context-menu click)", async () => {
    mockTauri();
    seed([makeNote("n1", "Alpha"), makeNote("n2", "Beta")]);
    renderAll();

    const row = screen.getByRole("link", { name: /Alpha/ });
    fireEvent.click(row, { ctrlKey: true });
    // Ctrl is not a selection modifier: the row is not selected and no
    // selection bar appears.
    expect(row).not.toHaveAttribute("data-selected");
    expect(screen.queryByText(/selected$/)).not.toBeInTheDocument();
  });

  it("plain click still navigates to the note", async () => {
    mockTauri();
    seed([makeNote("n1", "Alpha")]);
    renderAll();

    fireEvent.click(screen.getByRole("link", { name: /Alpha/ }));
    await waitFor(() => expect(loc()).toBe("/note/n1"));
  });

  it("shift-click selects a contiguous range and the bar shows the count", async () => {
    mockTauri();
    seed([makeNote("n1", "Alpha"), makeNote("n2", "Beta"), makeNote("n3", "Gamma")]);
    renderAll();

    fireEvent.click(screen.getByRole("link", { name: /Alpha/ }), { metaKey: true });
    fireEvent.click(screen.getByRole("link", { name: /Gamma/ }), { shiftKey: true });

    expect(screen.getByRole("link", { name: /Alpha/ })).toHaveAttribute("data-selected", "true");
    expect(screen.getByRole("link", { name: /Beta/ })).toHaveAttribute("data-selected", "true");
    expect(screen.getByRole("link", { name: /Gamma/ })).toHaveAttribute("data-selected", "true");
    expect(screen.getByText("3 selected")).toBeInTheDocument();
  });

  it("Cancel clears the selection and hides the bar", async () => {
    mockTauri();
    seed([makeNote("n1", "Alpha"), makeNote("n2", "Beta")]);
    renderAll();

    fireEvent.click(screen.getByRole("link", { name: /Alpha/ }), { metaKey: true });
    fireEvent.click(screen.getByRole("link", { name: /Beta/ }), { metaKey: true });
    expect(screen.getByText("2 selected")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.queryByText("2 selected")).not.toBeInTheDocument();
    expect(screen.getByRole("link", { name: /Alpha/ })).not.toHaveAttribute("data-selected");
  });

  it("Esc clears the selection", async () => {
    mockTauri();
    seed([makeNote("n1", "Alpha"), makeNote("n2", "Beta")]);
    renderAll();

    fireEvent.click(screen.getByRole("link", { name: /Alpha/ }), { metaKey: true });
    fireEvent.click(screen.getByRole("link", { name: /Beta/ }), { metaKey: true });
    expect(screen.getByText("2 selected")).toBeInTheDocument();

    fireEvent.keyDown(document.body, { key: "Escape" });
    expect(screen.queryByText("2 selected")).not.toBeInTheDocument();
  });
});

describe("AllNotes hover checkbox (discoverability)", () => {
  it("renders a per-row checkbox that toggles selection without navigating", async () => {
    mockTauri();
    seed([makeNote("n1", "Alpha"), makeNote("n2", "Beta")]);
    renderAll();

    const cb = screen.getByRole("checkbox", { name: /Select Alpha/ });
    expect(cb).toBeInTheDocument();
    expect(cb).not.toBeChecked();

    fireEvent.click(cb);
    expect(loc()).toBe("/"); // checkbox never navigates
    expect(cb).toBeChecked();
    expect(screen.getByRole("link", { name: /Alpha/ })).toHaveAttribute(
      "aria-selected",
      "true",
    );

    fireEvent.click(cb); // toggle back off
    expect(cb).not.toBeChecked();
    expect(loc()).toBe("/");
  });

  it("clicking a second row's checkbox adds it — selection mode persists", async () => {
    mockTauri();
    seed([makeNote("n1", "Alpha"), makeNote("n2", "Beta")]);
    renderAll();

    fireEvent.click(screen.getByRole("checkbox", { name: /Select Alpha/ }));
    fireEvent.click(screen.getByRole("checkbox", { name: /Select Beta/ }));

    expect(screen.getByRole("checkbox", { name: /Select Alpha/ })).toBeChecked();
    expect(screen.getByRole("checkbox", { name: /Select Beta/ })).toBeChecked();
    expect(screen.getByText("2 selected")).toBeInTheDocument();
  });

  it("shift+checkbox extends selection into a contiguous range", async () => {
    mockTauri();
    seed([makeNote("n1", "Alpha"), makeNote("n2", "Beta"), makeNote("n3", "Gamma")]);
    renderAll();

    fireEvent.click(screen.getByRole("checkbox", { name: /Select Alpha/ }));
    fireEvent.click(screen.getByRole("checkbox", { name: /Select Gamma/ }), {
      shiftKey: true,
    });

    expect(screen.getByRole("checkbox", { name: /Select Alpha/ })).toBeChecked();
    expect(screen.getByRole("checkbox", { name: /Select Beta/ })).toBeChecked();
    expect(screen.getByRole("checkbox", { name: /Select Gamma/ })).toBeChecked();
    expect(screen.getByText("3 selected")).toBeInTheDocument();
  });

  it("shows every row's checkbox while a selection is active, hides them once cleared", async () => {
    mockTauri();
    seed([makeNote("n1", "Alpha"), makeNote("n2", "Beta")]);
    renderAll();

    const beta = screen.getByRole("checkbox", { name: /Select Beta/ });
    // At rest, an unselected row's checkbox is hover-only (not force-shown).
    expect(beta).toHaveAttribute("data-shown", "false");

    // Selecting Alpha enters selection mode → all rows force their checkbox.
    fireEvent.click(screen.getByRole("checkbox", { name: /Select Alpha/ }));
    expect(beta).toHaveAttribute("data-shown", "true");

    // Clearing selection returns unselected rows to hover-only.
    fireEvent.keyDown(document.body, { key: "Escape" });
    expect(screen.getByRole("checkbox", { name: /Select Beta/ })).toHaveAttribute(
      "data-shown",
      "false",
    );
  });

  it("shows the action bar at a single checkbox selection", async () => {
    mockTauri();
    seed([makeNote("n1", "Alpha"), makeNote("n2", "Beta")]);
    renderAll();

    fireEvent.click(screen.getByRole("checkbox", { name: /Select Alpha/ }));
    expect(screen.getByText("1 selected")).toBeInTheDocument();
  });
});

describe("AllNotes bulk delete", () => {
  it("deletes every selected note behind one confirm, one invoke per id", async () => {
    const deleteSpy = vi.fn();
    mockTauri({
      notes_delete: (args) => {
        deleteSpy(args);
        return undefined;
      },
    });
    seed([makeNote("n1", "Alpha"), makeNote("n2", "Beta")]);
    renderAll();

    fireEvent.click(screen.getByRole("link", { name: /Alpha/ }), { metaKey: true });
    fireEvent.click(screen.getByRole("link", { name: /Beta/ }), { metaKey: true });

    // First Delete opens the confirm; the actual deletion is behind it.
    fireEvent.click(screen.getByRole("button", { name: "Delete" }));
    expect(deleteSpy).not.toHaveBeenCalled();

    const dialog = screen.getByRole("dialog", { name: /Delete notes/ });
    fireEvent.click(within(dialog).getByRole("button", { name: "Delete" }));

    await waitFor(() => expect(deleteSpy).toHaveBeenCalledTimes(2));
    expect(deleteSpy).toHaveBeenCalledWith({ id: "n1" });
    expect(deleteSpy).toHaveBeenCalledWith({ id: "n2" });
    // Both notes are gone from the store, so the bar clears.
    await waitFor(() => expect(useNotesStore.getState().notes).toHaveLength(0));
  });

  it("stops and reports when a delete fails, leaving unprocessed notes selected", async () => {
    mockTauri({
      notes_delete: (args) => {
        if ((args as { id: string }).id === "n2") throw new Error("boom");
        return undefined;
      },
    });
    seed([makeNote("n1", "Alpha"), makeNote("n2", "Beta"), makeNote("n3", "Gamma")]);
    renderAll();

    fireEvent.click(screen.getByRole("link", { name: /Alpha/ }), { metaKey: true });
    fireEvent.click(screen.getByRole("link", { name: /Beta/ }), { metaKey: true });
    fireEvent.click(screen.getByRole("link", { name: /Gamma/ }), { metaKey: true });

    fireEvent.click(screen.getByRole("button", { name: "Delete" }));
    const dialog = screen.getByRole("dialog", { name: /Delete notes/ });
    fireEvent.click(within(dialog).getByRole("button", { name: "Delete" }));

    // n1 deleted, n2 failed -> stop. An error is surfaced; n2/n3 remain.
    await waitFor(() => expect(useRecordingStore.getState().errors.length).toBeGreaterThan(0));
    const notes = useNotesStore.getState().notes.map((n) => n.id);
    expect(notes).toContain("n2");
    expect(notes).toContain("n3");
    expect(notes).not.toContain("n1");
  });
});

describe("AllNotes bulk move", () => {
  it("moves every selected note into a folder, one invoke per id", async () => {
    const moveSpy = vi.fn();
    mockTauri({
      notes_move: (args) => {
        moveSpy(args);
        return undefined;
      },
    });
    seed(
      [makeNote("n1", "Alpha"), makeNote("n2", "Beta")],
      [{ id: "f1", name: "Work", created_at: 0, updated_at: 0 }],
    );
    renderAll();

    fireEvent.click(screen.getByRole("link", { name: /Alpha/ }), { metaKey: true });
    fireEvent.click(screen.getByRole("link", { name: /Beta/ }), { metaKey: true });

    // The picker is the shared Menu now (#114): rows are menu items, and the
    // trigger opens on pointerdown, so drive it through userEvent.
    await userEvent.click(screen.getByRole("button", { name: /Move to folder/ }));
    await userEvent.click(screen.getByRole("menuitem", { name: "Work" }));

    await waitFor(() => expect(moveSpy).toHaveBeenCalledTimes(2));
    expect(moveSpy).toHaveBeenCalledWith({ id: "n1", folderId: "f1" });
    expect(moveSpy).toHaveBeenCalledWith({ id: "n2", folderId: "f1" });
  });

  it("moves selected notes to no folder (null)", async () => {
    const moveSpy = vi.fn();
    mockTauri({
      notes_move: (args) => {
        moveSpy(args);
        return undefined;
      },
    });
    seed(
      [makeNote("n1", "Alpha"), makeNote("n2", "Beta")],
      [{ id: "f1", name: "Work", created_at: 0, updated_at: 0 }],
    );
    renderAll();

    fireEvent.click(screen.getByRole("link", { name: /Alpha/ }), { metaKey: true });
    fireEvent.click(screen.getByRole("link", { name: /Beta/ }), { metaKey: true });

    await userEvent.click(screen.getByRole("button", { name: /Move to folder/ }));
    // "No folder" also names a filter chip up top, but that one is a button —
    // the picker entry is the only menu item with that name.
    await userEvent.click(screen.getByRole("menuitem", { name: "No folder" }));

    await waitFor(() => expect(moveSpy).toHaveBeenCalledTimes(2));
    expect(moveSpy).toHaveBeenCalledWith({ id: "n1", folderId: null });
    expect(moveSpy).toHaveBeenCalledWith({ id: "n2", folderId: null });
  });
});

describe("AllNotes activity", () => {
  it("shows the note's activity in place of its static state, and only that note's", () => {
    mockTauri();
    seed([makeNote("n1", "Alpha"), makeNote("n2", "Beta")]);
    useRecordingStore.setState({ summarizing: { n1: true } });
    renderAll();

    const alpha = screen.getByRole("link", { name: /Alpha/ });
    expect(alpha).toHaveTextContent("Summarizing…");
    expect(alpha).not.toHaveTextContent("Empty");
    // The dot is aria-hidden, so it is reached through the DOM rather than a role.
    expect(alpha.querySelector(".rec-dot")).not.toBeNull();

    const beta = screen.getByRole("link", { name: /Beta/ });
    expect(beta).toHaveTextContent("Empty");
    expect(beta.querySelector(".rec-dot")).toBeNull();

    useRecordingStore.setState({ summarizing: {} });
  });

  it("puts the capture's clock beside a recording note's label, and only there", () => {
    mockTauri();
    seed([makeNote("n1", "Alpha"), makeNote("n2", "Beta")]);
    useRecordingStore.setState({
      status: { noteId: "n1", phase: "recording" },
      activeSince: Date.now() - 65_000,
      activeAccumMs: 0,
      summarizing: { n2: true },
    });
    renderAll();

    expect(screen.getByRole("link", { name: /Alpha/ })).toHaveTextContent(/Recording1:0[56]/);
    // The date line carries a clock of its own, so the check is on the label.
    expect(screen.getByRole("link", { name: /Beta/ })).toHaveTextContent(/Summarizing…$/);

    useRecordingStore.setState({
      status: { noteId: null, phase: "idle" },
      activeSince: null,
      activeAccumMs: 0,
      summarizing: {},
    });
  });
});

// #191 — a private note says so on its card, and only inside a workspace: every
// Personal note is private, so the glyph there would appear on every card and
// distinguish nothing.
describe("a private note's card", () => {
  it("carries a lock in a workspace and nothing on Personal", async () => {
    mockTauri();
    seed([
      { ...makeNote("n1", "Alpha"), workspace_id: "ws1", private: true },
      { ...makeNote("n2", "Beta"), workspace_id: "ws1" },
      { ...makeNote("n3", "Gamma"), private: true },
    ]);
    renderAll();

    const card = (title: string) => screen.getByRole("link", { name: new RegExp(title) });
    expect(card("Alpha").textContent).toContain("Private");
    expect(card("Beta").textContent).not.toContain("Private");
    expect(card("Gamma").textContent).not.toContain("Private");
  });
});

describe("AllNotes filters", () => {
  // Each picker is a menu: open the trigger by its accessible name, pick a row.
  async function pick(control: string, option: string) {
    await userEvent.click(screen.getByRole("button", { name: control }));
    await userEvent.click(screen.getByRole("menuitemradio", { name: option }));
  }

  const titles = () =>
    screen.getAllByRole("link").map((a) => a.textContent ?? "");

  it("narrows by status, and the count says how much is hidden", async () => {
    mockTauri();
    seed([
      { ...makeNote("n1", "Alpha"), summary: "s" },
      { ...makeNote("n2", "Beta"), transcript: "t" },
      makeNote("n3", "Gamma"),
    ]);
    renderAll();

    await pick("Status", "Summarized");
    expect(titles().join(" ")).toContain("Alpha");
    expect(titles().join(" ")).not.toContain("Beta");
    expect(screen.getByText("1 of 3")).toBeTruthy();

    await pick("Status", "Transcribed");
    expect(titles().join(" ")).toContain("Beta");
    expect(titles().join(" ")).not.toContain("Alpha");
  });

  // The one status the note row can't answer on its own (#146).
  it("treats a note holding untranscribed audio as recorded", async () => {
    mockTauri({ notes_awaiting_transcription: () => ["n2"] });
    seed([makeNote("n1", "Alpha"), makeNote("n2", "Beta")]);
    renderAll();

    await waitFor(() => expect(screen.getByRole("link", { name: /Beta/ }).textContent).toContain("Recorded"));
    await pick("Status", "Recorded");
    expect(titles().join(" ")).toContain("Beta");
    expect(titles().join(" ")).not.toContain("Alpha");
  });

  it("narrows by client, including the untagged notes", async () => {
    mockTauri();
    useNotesStore.setState({
      clients: [{ id: "c1", name: "Acme", created_at: 0, updated_at: 0 }],
    });
    seed([{ ...makeNote("n1", "Alpha"), client_id: "c1" }, makeNote("n2", "Beta")]);
    renderAll();

    await pick("Client", "Acme");
    expect(titles().join(" ")).toContain("Alpha");
    expect(titles().join(" ")).not.toContain("Beta");

    await pick("Client", "No client");
    expect(titles().join(" ")).toContain("Beta");
    expect(titles().join(" ")).not.toContain("Alpha");
  });

  // Personal has one author, so an owner picker there offers a single name
  // that changes nothing.
  it("offers the owner axis only in a workspace with more than one member", async () => {
    mockTauri();
    seed([makeNote("n1", "Alpha")]);
    renderAll();
    expect(screen.queryByRole("button", { name: "Created by" })).toBeNull();
  });

  it("counts an unsynced note as the signed-in user's own", async () => {
    mockTauri();
    useCloudStore.setState({
      members: {
        "u-me": { id: "u-me", email: "me@x", name: "Me", role: "owner" },
        "u-anna": { id: "u-anna", email: "anna@x", name: "Anna", role: "member" },
      },
      status: { ...useCloudStore.getState().status, user: { id: "u-me", email: "me@x", name: "Me", verified: true } },
    });
    seed([
      { ...makeNote("n1", "Alpha"), owner: "" },
      { ...makeNote("n2", "Beta"), owner: "u-anna" },
    ]);
    renderAll();

    await pick("Created by", "Me");
    expect(titles().join(" ")).toContain("Alpha");
    expect(titles().join(" ")).not.toContain("Beta");
  });

  it("clears every axis at once", async () => {
    mockTauri();
    seed([{ ...makeNote("n1", "Alpha"), summary: "s" }, makeNote("n2", "Beta")]);
    renderAll();

    await pick("Status", "Summarized");
    expect(titles()).toHaveLength(1);
    await userEvent.click(screen.getByRole("button", { name: /Clear/ }));
    expect(titles()).toHaveLength(2);
  });
});
