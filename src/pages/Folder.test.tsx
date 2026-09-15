import { describe, it, expect, beforeEach } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter, Route, Routes, useNavigate } from "react-router-dom";
import { Folder } from "./Folder";
import { mockTauri } from "../test/tauri";
import { useNotesStore } from "../lib/store";
import { makeNote } from "../test/fixtures";

beforeEach(() => {
  useNotesStore.setState({ notes: [], folders: [], clients: [], recordedNoteIds: new Set() });
});

function renderFolder(entry = "/folder/f1") {
  return render(
    <MemoryRouter initialEntries={[entry]}>
      <Routes>
        <Route path="/folder/:id" element={<Folder />} />
      </Routes>
    </MemoryRouter>,
  );
}

// The state dot is shared with All notes, and a note that reads Recorded on one
// view must not read Empty on the other.
describe("Folder view", () => {
  it("draws a recorded note's state the same way All notes does", () => {
    mockTauri();
    useNotesStore.setState({
      folders: [{ id: "f1", name: "Work", created_at: 0, updated_at: 0 }],
      notes: [makeNote({ id: "n1", title: "Alpha", folder_id: "f1" })],
      recordedNoteIds: new Set(["n1"]),
    });
    renderFolder();

    expect(screen.getByRole("link", { name: /Alpha/ }).textContent).toContain("Recorded");
  });

  it("calls a note with nothing in it empty", () => {
    mockTauri();
    useNotesStore.setState({
      folders: [{ id: "f1", name: "Work", created_at: 0, updated_at: 0 }],
      notes: [makeNote({ id: "n1", title: "Alpha", folder_id: "f1" })],
    });
    renderFolder();

    expect(screen.getByRole("link", { name: /Alpha/ }).textContent).toContain("Empty");
  });
});

describe("Folder filters", () => {
  const FOLDERS = [
    { id: "f1", name: "Work", created_at: 0, updated_at: 0 },
    { id: "f2", name: "Home", created_at: 0, updated_at: 0 },
  ];

  async function pick(control: string, option: string) {
    await userEvent.click(screen.getByRole("button", { name: control }));
    await userEvent.click(screen.getByRole("menuitemradio", { name: option }));
  }

  const titles = () => screen.getAllByRole("link").map((a) => a.textContent ?? "").join(" ");

  it("narrows the folder's own notes, and the count says how much is hidden", async () => {
    mockTauri();
    useNotesStore.setState({
      folders: FOLDERS,
      notes: [
        makeNote({ id: "n1", title: "Alpha", folder_id: "f1", summary: "s" }),
        makeNote({ id: "n2", title: "Beta", folder_id: "f1" }),
        makeNote({ id: "n3", title: "Gamma", folder_id: "f2", summary: "s" }),
      ],
    });
    renderFolder();

    await pick("Status", "Summarized");
    expect(titles()).toContain("Alpha");
    expect(titles()).not.toContain("Beta");
    // The other folder's note was never in reach to begin with.
    expect(titles()).not.toContain("Gamma");
    expect(screen.getByText("1 of 2")).toBeTruthy();
  });

  // Every note here is filed, in this folder.
  it("offers no unfiled toggle", () => {
    mockTauri();
    useNotesStore.setState({
      folders: FOLDERS,
      notes: [makeNote({ id: "n1", title: "Alpha", folder_id: "f1" })],
    });
    renderFolder();

    expect(screen.queryByRole("button", { name: /No folder/ })).toBeNull();
  });

  it("says the filter emptied the view, not that the folder is empty", async () => {
    mockTauri();
    useNotesStore.setState({
      folders: FOLDERS,
      notes: [makeNote({ id: "n1", title: "Alpha", folder_id: "f1" })],
    });
    renderFolder();

    await pick("Status", "Summarized");
    expect(screen.getByText("No notes match these filters.")).toBeTruthy();
    expect(screen.queryByText("No notes in this folder yet.")).toBeNull();
  });
});

// Switching folders is not a remount — the route's params change under the same
// component — so the filter has to be cleared deliberately or it silently
// narrows the next folder.
describe("Folder filter reset", () => {
  function Switcher() {
    const navigate = useNavigate();
    return <button onClick={() => navigate("/folder/f2")}>Go to Home</button>;
  }

  it("clears the filter when the folder changes", async () => {
    mockTauri();
    useNotesStore.setState({
      folders: [
        { id: "f1", name: "Work", created_at: 0, updated_at: 0 },
        { id: "f2", name: "Home", created_at: 0, updated_at: 0 },
      ],
      notes: [
        makeNote({ id: "n1", title: "Alpha", folder_id: "f1", summary: "s" }),
        makeNote({ id: "n2", title: "Beta", folder_id: "f2" }),
      ],
    });
    render(
      <MemoryRouter initialEntries={["/folder/f1"]}>
        <Switcher />
        <Routes>
          <Route path="/folder/:id" element={<Folder />} />
        </Routes>
      </MemoryRouter>,
    );

    await userEvent.click(screen.getByRole("button", { name: "Status" }));
    await userEvent.click(screen.getByRole("menuitemradio", { name: "Summarized" }));
    expect(screen.getByRole("button", { name: "Status" }).textContent).toContain("Summarized");

    await userEvent.click(screen.getByRole("button", { name: "Go to Home" }));
    expect(screen.getByRole("link", { name: /Beta/ })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Status" }).textContent).toContain("Any status");
  });
});
