import { describe, it, expect, beforeEach } from "vitest";
import { render, screen } from "@testing-library/react";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { Folder } from "./Folder";
import { mockTauri } from "../test/tauri";
import { useNotesStore } from "../lib/store";
import { makeNote } from "../test/fixtures";

beforeEach(() => {
  useNotesStore.setState({ notes: [], folders: [], clients: [], recordedNoteIds: new Set() });
});

function renderFolder() {
  return render(
    <MemoryRouter initialEntries={["/folder/f1"]}>
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
