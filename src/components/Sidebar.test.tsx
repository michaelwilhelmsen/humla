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
    detected_language: null,
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
  it("labels the nav entry 'Import audio' with no trailing ellipsis", () => {
    mockTauri();
    renderSidebar();
    expect(
      screen.getByRole("button", { name: "Import audio" }),
    ).toBeInTheDocument();
    expect(screen.queryByText("Import audio…")).not.toBeInTheDocument();
  });

  it("picks a file, opens the config dialog, then imports and navigates", async () => {
    const importSpy = vi.fn();
    mockTauri({
      // The native open panel returns the chosen path.
      "plugin:dialog|open": () => "/Users/me/meeting.m4a",
      // Dialog preseeds its language chip from the global default.
      settings_get: () => "en",
      import_audio: (args) => {
        importSpy(args);
        return makeNote("imp1", "meeting");
      },
    });
    renderSidebar();

    fireEvent.click(screen.getByRole("button", { name: /Import audio/ }));

    // A config dialog opens first — import must NOT start immediately (this is
    // the whole point: language/speakers are chosen before the one-shot
    // transcription runs). Wait for the language chip to preseed from the
    // global default ("en" → "English") before confirming.
    const importBtn = await screen.findByRole("button", { name: /^Import$/ });
    expect(importSpy).not.toHaveBeenCalled();
    await screen.findByText("English");

    fireEvent.click(importBtn);

    await waitFor(() => expect(importSpy).toHaveBeenCalledTimes(1));
    expect(importSpy).toHaveBeenCalledWith(
      expect.objectContaining({
        path: "/Users/me/meeting.m4a",
        language: "en",
        expectedSpeakers: null,
      }),
    );
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

// #110: folders are the natural thing to scope a question to, and until now the
// only way to reach a folder-scoped conversation was from a test. The folder row
// is the entry point.
describe("Sidebar folder chat (#110)", () => {
  function seedFolder(id = "f1", name = "K2 pilot") {
    useNotesStore.setState({
      notes: [],
      folders: [{ id, name, created_at: 0, updated_at: 0 }],
    });
  }

  it("offers 'Chat about this folder' and navigates to that folder's chat", async () => {
    mockTauri();
    seedFolder();
    renderSidebar();

    fireEvent.contextMenu(await screen.findByText("K2 pilot"));
    fireEvent.click(await screen.findByText("Chat about this folder"));
    // The route says what the conversation can reach — it is not `/chat` with a
    // hidden filter, which is what made the folder scope unreachable before.
    await waitFor(() => expect(loc()).toBe("/folder/f1/chat"));
  });

  it("deletes a folder with no conversations immediately, as it always did", async () => {
    const deleted: string[] = [];
    mockTauri({
      chat_list_conversations: () => [],
      folders_delete: (args) => {
        deleted.push((args as { id: string }).id);
        return null;
      },
    });
    seedFolder();
    renderSidebar();

    fireEvent.contextMenu(await screen.findByText("K2 pilot"));
    fireEvent.click(await screen.findByText("Delete"));
    // Notes only move out of the folder, so there is nothing unrecoverable to
    // warn about — a confirm here would be a prompt about a non-loss.
    await waitFor(() => expect(deleted).toEqual(["f1"]));
    expect(screen.queryByText(/deleted for good/)).toBeNull();
  });

  it("confirms first when conversations would be destroyed, and names the cost", async () => {
    const deleted: string[] = [];
    const deletedConvs: string[] = [];
    mockTauri({
      chat_list_conversations: () => [{ id: "c1" }, { id: "c2" }],
      chat_delete_conversation: (args) => {
        deletedConvs.push((args as { conversationId: string }).conversationId);
        return null;
      },
      folders_delete: (args) => {
        deleted.push((args as { id: string }).id);
        return null;
      },
    });
    seedFolder();
    renderSidebar();

    fireEvent.contextMenu(await screen.findByText("K2 pilot"));
    fireEvent.click(await screen.findByText("Delete"));

    // A folder thread's whole reach was that folder, so it goes with it — hard,
    // with no Trash behind it. The prompt has to say so rather than ask a vague
    // "are you sure?", and nothing may be destroyed before the user answers.
    expect(await screen.findByText(/2 chat conversations/)).toBeInTheDocument();
    expect(screen.getByText(/deleted for good/)).toBeInTheDocument();
    expect(deleted).toEqual([]);

    fireEvent.click(screen.getByRole("button", { name: "Delete" }));
    await waitFor(() => expect(deleted).toEqual(["f1"]));
    // Through the per-conversation command, not by dropping local rows: in a
    // workspace the thread is server-authoritative and the local row is only a
    // handle, so a local-only cascade would leave the real thread alive on the
    // server pointing at a folder that no longer exists.
    expect(deletedConvs).toEqual(["c1", "c2"]);
  });

  it("confirms rather than assuming zero when the conversations can't be listed", async () => {
    const deleted: string[] = [];
    mockTauri({
      chat_list_conversations: () => {
        throw new Error("unavailable");
      },
      folders_delete: (args) => {
        deleted.push((args as { id: string }).id);
        return null;
      },
    });
    seedFolder();
    renderSidebar();

    fireEvent.contextMenu(await screen.findByText("K2 pilot"));
    fireEvent.click(await screen.findByText("Delete"));

    // Treating "couldn't ask" as "nothing there" is the one wrong way to be wrong
    // here — it turns an unknown number of unrecoverable threads into a silent
    // hard delete. The prompt appears without a number.
    expect(await screen.findByText(/any chat conversations about it are/)).toBeInTheDocument();
    expect(deleted).toEqual([]);
  });

  it("leaves the folder alone when a conversation refuses to delete", async () => {
    const deleted: string[] = [];
    mockTauri({
      chat_list_conversations: () => [{ id: "c1" }],
      chat_delete_conversation: () => {
        // The server's own rule: only the thread's creator, or a workspace
        // owner/admin, may delete one.
        throw new Error("Only the author can delete this conversation");
      },
      folders_delete: (args) => {
        deleted.push((args as { id: string }).id);
        return null;
      },
    });
    seedFolder();
    renderSidebar();

    fireEvent.contextMenu(await screen.findByText("K2 pilot"));
    fireEvent.click(await screen.findByText("Delete"));
    fireEvent.click(await screen.findByRole("button", { name: "Delete" }));

    // A folder that half-deleted — gone locally, its threads still on the server —
    // is worse than one that didn't delete, because nothing would ever offer to
    // finish it. So the refusal aborts and says so.
    expect(await screen.findByText(/the folder was left alone/)).toBeInTheDocument();
    expect(deleted).toEqual([]);
    expect(useNotesStore.getState().folders).toHaveLength(1);
  });
});
