import { describe, it, expect, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import Chat from "./Chat";
import { mockTauri } from "../test/tauri";
import { useCloudStore, DISCONNECTED } from "../lib/cloud";
import { useNotesStore } from "../lib/store";
import type { ConversationMeta, Note } from "../lib/ipc";

// The `/chat` surface (issue #95). These tests are about what this page owns —
// the Recents rail, the library prompt set, and the empty-library state. The
// panel's own behaviour (streaming, citations, activation, a11y) is covered by
// ChatPanel.test.tsx and arrives here unchanged by construction (#94).

const HOUR = 3_600_000;

function conversation(over: Partial<ConversationMeta> & { id: string }): ConversationMeta {
  return { title: "Untitled", breadth: "all", updatedAt: 1, messageCount: 2, ...over };
}

function seedNotes(count: number) {
  const notes = Array.from({ length: count }, (_, i) => ({ id: `n${i}` }) as unknown as Note);
  useNotesStore.setState({ notes, loaded: true });
}

function renderChat() {
  return render(
    <MemoryRouter>
      <Chat />
    </MemoryRouter>,
  );
}

beforeEach(() => {
  mockTauri();
  useCloudStore.setState({ status: DISCONNECTED });
  // Default: a library with notes, already loaded — the ordinary case.
  seedNotes(3);
});

describe("/chat page", () => {
  it("opens on the composer, with the library-wide target", async () => {
    const asked: unknown[] = [];
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => ({ conversationId: null, messages: [] }),
      chat_list_conversations: (args) => {
        asked.push(args);
        return [];
      },
    });
    renderChat();

    await waitFor(() =>
      expect(screen.getByPlaceholderText(/Ask about your notes/)).toBeInTheDocument(),
    );
    // No greeting and no display heading — just the page title (#95).
    expect(screen.getByRole("heading", { name: "Chat" })).toBeInTheDocument();
    // An ABSENT note id is what the backend reads as the global scope; an empty
    // string is rejected (#93), so this assertion is the wire contract.
    expect(asked).toContainEqual({ noteId: null });
  });

  it("lists recent conversations most-recent first and marks the active one", async () => {
    const now = Date.now();
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => ({ conversationId: "c-mid", messages: [] }),
      chat_list_conversations: () => [
        conversation({ id: "c-old", title: "Budget questions", updatedAt: now - 50 * HOUR }),
        conversation({ id: "c-new", title: "This week", updatedAt: now - 1 * HOUR }),
        conversation({ id: "c-mid", title: "Client status", updatedAt: now - 5 * HOUR }),
      ],
    });
    renderChat();

    const rows = await waitFor(() => {
      const found = screen.getAllByRole("listitem");
      expect(found.length).toBe(3);
      return found;
    });
    expect(rows.map((r) => r.textContent)).toEqual([
      "This week1h ago",
      "Client status5h ago",
      "Budget questions2d ago",
    ]);
    // The loaded conversation is marked, and only it.
    const current = screen.getAllByRole("button", { current: true });
    expect(current).toHaveLength(1);
    expect(current[0].textContent).toContain("Client status");
  });

  it("caps Recents at ten rows", async () => {
    const now = Date.now();
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => ({ conversationId: null, messages: [] }),
      chat_list_conversations: () =>
        Array.from({ length: 14 }, (_, i) =>
          conversation({ id: `c${i}`, title: `Chat ${i}`, updatedAt: now - i * HOUR }),
        ),
    });
    renderChat();

    await waitFor(() => expect(screen.getAllByRole("listitem")).toHaveLength(10));
    // The ten kept are the most recent ten, not the first ten returned.
    expect(screen.getByText("Chat 0")).toBeInTheDocument();
    expect(screen.queryByText("Chat 13")).toBeNull();
  });

  it("says so when there is no history to browse", async () => {
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => ({ conversationId: null, messages: [] }),
      // A lone untouched conversation isn't history worth listing (#62).
      chat_list_conversations: () => [conversation({ id: "c1", title: "", messageCount: 0 })],
    });
    renderChat();

    await waitFor(() => expect(screen.getByText("No conversations yet")).toBeInTheDocument());
    expect(screen.queryByRole("listitem")).toBeNull();
  });

  it("loads the conversation whose row is clicked", async () => {
    const requested: unknown[] = [];
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: (args) => {
        requested.push(args);
        return { conversationId: null, messages: [] };
      },
      chat_list_conversations: () => [
        conversation({ id: "c-a", title: "Alpha", updatedAt: 2 }),
        conversation({ id: "c-b", title: "Beta", updatedAt: 1 }),
      ],
    });
    renderChat();

    fireEvent.click(await screen.findByText("Beta"));
    await waitFor(() =>
      expect(requested).toContainEqual({ noteId: null, conversationId: "c-b" }),
    );
  });

  it("offers the library prompt set, not the note-scoped one", async () => {
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => ({ conversationId: null, messages: [] }),
    });
    renderChat();

    const input = await screen.findByPlaceholderText(/Ask about your notes/);
    fireEvent.keyDown(input, { key: "/" });

    expect(await screen.findByText("Outstanding actions")).toBeInTheDocument();
    expect(screen.getByText("Client status")).toBeInTheDocument();
    // "What I missed" only makes sense with a note on screen to have missed.
    expect(screen.queryByText("What I missed")).toBeNull();
  });
});

describe("/chat with an empty library", () => {
  it("disables the composer and says there is nothing to search", async () => {
    useNotesStore.setState({ notes: [], loaded: true });
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => ({ conversationId: null, messages: [] }),
    });
    renderChat();

    const input = await screen.findByLabelText("Ask about your notes");
    expect(input).toBeDisabled();
    expect(screen.getByPlaceholderText("Nothing to search yet")).toBeInTheDocument();
    // The standing invitation is replaced, not merely supplemented — it would
    // otherwise invite a question whose only answer is "I couldn't find anything".
    expect(screen.getByText(/No notes yet\./)).toBeInTheDocument();
    expect(screen.queryByText(/Ask anything about your notes/)).toBeNull();
  });

  it("keeps the composer enabled while the first load is still in flight", async () => {
    // notes: [] with loaded: false is "we don't know yet", not "empty" — claiming
    // an empty library here would fire on every launch for one frame.
    useNotesStore.setState({ notes: [], loaded: false });
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => ({ conversationId: null, messages: [] }),
    });
    renderChat();

    const input = await screen.findByPlaceholderText(/Ask about your notes/);
    expect(input).not.toBeDisabled();
  });
});
