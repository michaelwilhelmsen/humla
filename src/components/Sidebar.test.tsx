import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { Sidebar } from "./Sidebar";
import { LocationProbe } from "../test/app";
import { mockTauri } from "../test/tauri";
import { useNotesStore, useRecordingStore } from "../lib/store";
import type { Note } from "../lib/ipc";

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
    created_at: Date.now(),
    updated_at: Date.now(),
    owner: "",
    workspace_id: "",
  };
}

function renderSidebar() {
  return render(
    <MemoryRouter initialEntries={["/all-notes"]}>
      <Sidebar onCollapse={() => {}} />
      <LocationProbe />
    </MemoryRouter>,
  );
}

const loc = () => screen.getByTestId("location").textContent;

beforeEach(() => {
  useNotesStore.setState({ notes: [], folders: [] });
  useRecordingStore.setState({ errors: [] });
});

describe("Sidebar import audio", () => {
  it("picks a file, imports it into a new note, and navigates there", async () => {
    const importSpy = vi.fn();
    mockTauri({
      // The native open panel returns the chosen path.
      "plugin:dialog|open": () => "/Users/me/meeting.m4a",
      import_audio: (args) => {
        importSpy(args);
        return makeNote("imp1", "meeting");
      },
    });
    renderSidebar();

    fireEvent.click(screen.getByRole("button", { name: /Import audio/ }));

    await waitFor(() => expect(importSpy).toHaveBeenCalledTimes(1));
    expect(importSpy).toHaveBeenCalledWith({ path: "/Users/me/meeting.m4a" });
    // The created note lands in the store and we navigate to it.
    await waitFor(() => expect(loc()).toBe("/note/imp1"));
    expect(useNotesStore.getState().notes.map((n) => n.id)).toContain("imp1");
  });

  it("does nothing when the file picker is cancelled", async () => {
    const importSpy = vi.fn();
    mockTauri({
      "plugin:dialog|open": () => null, // cancelled
      import_audio: (args) => {
        importSpy(args);
        return makeNote("imp1", "meeting");
      },
    });
    renderSidebar();

    fireEvent.click(screen.getByRole("button", { name: /Import audio/ }));

    // No import, no navigation away from the starting route.
    await waitFor(() => expect(loc()).toBe("/all-notes"));
    expect(importSpy).not.toHaveBeenCalled();
  });
});
