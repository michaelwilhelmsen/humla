import { describe, it, expect, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor, within } from "@testing-library/react";
import { MemoryRouter, Outlet, Route, Routes } from "react-router-dom";
import { Chat } from "./Chat";
import { mockTauri } from "../test/tauri";
import { useCloudStore, DISCONNECTED, type CloudRole } from "../lib/cloud";
import { useNotesStore } from "../lib/store";
import { useGlobalChatStore } from "../lib/globalChat";
import type { Note } from "../lib/ipc";

// The `/chat` page (issue #95). The page owns the shell, the library-wide target
// and the collapsed-sidebar fallback; the conversation list it publishes is
// rendered by the sidebar and tested in ChatConversations.test.tsx. Panel
// behaviour (streaming, citations, activation, a11y) is ChatPanel.test.tsx's.

function seedNotes(count: number) {
  const notes = Array.from({ length: count }, (_, i) => ({ id: `n${i}` }) as unknown as Note);
  useNotesStore.setState({ notes, loaded: true });
}

function signIntoWorkspace(role: CloudRole = "owner") {
  const ws = { id: "ws1", name: "Acme Team", role, plan_status: "active" as const };
  useCloudStore.setState({
    status: {
      ...DISCONNECTED,
      configured: true,
      logged_in: true,
      base_url: "https://sync.humla.team",
      current_workspace: ws,
      workspaces: [ws],
    },
  });
}

// `Chat` reads `sidebarCollapsed` from the Layout's outlet context, so it has to
// be rendered under a layout route rather than bare.
function renderChat(sidebarCollapsed = false) {
  return render(
    <MemoryRouter initialEntries={["/chat"]}>
      <Routes>
        <Route path="/" element={<Outlet context={{ sidebarCollapsed }} />}>
          <Route path="chat" element={<Chat />} />
        </Route>
      </Routes>
    </MemoryRouter>,
  );
}

beforeEach(() => {
  mockTauri();
  useCloudStore.setState({ status: DISCONNECTED, syncStatus: null });
  useGlobalChatStore.setState({ controls: null });
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
    // No greeting and no page heading at all: the title lives in the app bar,
    // and a second "Chat" beneath it would be the same word twice (#95).
    expect(screen.queryByRole("heading")).toBeNull();
    expect(screen.getByTitle("Chat")).toBeInTheDocument();
    // An ABSENT note id is what the backend reads as the global scope; an empty
    // string is rejected (#93), so this assertion is the wire contract. The window
    // is the first page — the list is uncapped and pages in as it scrolls.
    expect(asked[0]).toMatchObject({ noteId: null, offset: 0 });
    expect((asked[0] as { limit: number }).limit).toBeGreaterThan(0);
  });

  it("publishes its session projection for the sidebar to render", async () => {
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => ({ conversationId: null, messages: [] }),
      chat_list_conversations: () => [],
    });
    renderChat();

    // The list lives in the sidebar, which is not in this tree — the page's job is
    // to get the projection into the store.
    await waitFor(() => expect(useGlobalChatStore.getState().controls).not.toBeNull());
    expect(useGlobalChatStore.getState().controls?.targetKey).toBe("global");
  });

  it("clears the projection on unmount, so the sidebar can't outlive the pane", async () => {
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => ({ conversationId: null, messages: [] }),
    });
    const { unmount } = renderChat();
    await waitFor(() => expect(useGlobalChatStore.getState().controls).not.toBeNull());

    unmount();
    expect(useGlobalChatStore.getState().controls).toBeNull();
  });

  it("shows the library prompts as cards on a new chat", async () => {
    // A blank page tells a first-time user nothing about what this can do, and on
    // a library-wide surface the useful questions are the least guessable ones.
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => ({ conversationId: null, messages: [] }),
    });
    renderChat();

    await screen.findByPlaceholderText(/Ask about your notes/);
    expect(await screen.findByRole("button", { name: /Needs my attention/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Client status/ })).toBeInTheDocument();
    // "What I missed" only makes sense with a note on screen to have missed.
    expect(screen.queryByText("What I missed")).toBeNull();
  });

  it("says how to reach the prompts again once the cards are gone", async () => {
    // The "/" menu shipped in #80 and was mentioned nowhere, so it was
    // discoverable only by accident.
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => ({ conversationId: null, messages: [] }),
    });
    renderChat();

    expect(await screen.findByText(/in the composer for these any time/)).toBeInTheDocument();
  });

  it("fills the composer from a card rather than spending a turn", async () => {
    // These are starting points. A card that sent immediately — a metered turn, in
    // a workspace — would commit the user to a question they hadn't finished
    // thinking about.
    let sent = false;
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => ({ conversationId: null, messages: [] }),
      chat_send: () => {
        sent = true;
        return { conversationId: "c1", truncated: false };
      },
    });
    renderChat();

    const input = await screen.findByPlaceholderText(/Ask about your notes/);
    fireEvent.click(await screen.findByRole("button", { name: /Weekly recap/ }));

    expect(input).toHaveValue("Recap this week across my meetings.");
    expect(sent).toBe(false);
  });

  it("offers the same set from the '/' menu", async () => {
    // One list, two surfaces — the cards are not a second set that could drift.
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => ({ conversationId: null, messages: [] }),
    });
    renderChat();

    const input = await screen.findByPlaceholderText(/Ask about your notes/);
    fireEvent.keyDown(input, { key: "/" });

    const menu = await screen.findByRole("listbox", { name: "Prompts" });
    for (const label of ["Needs my attention", "Weekly recap", "Client status", "Decisions log"]) {
      expect(within(menu).getByText(label)).toBeInTheDocument();
    }
  });

  it("shows no scope picker — its only option would be 'All notes'", async () => {
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => ({ conversationId: null, messages: [] }),
    });
    renderChat();

    await screen.findByPlaceholderText(/Ask about your notes/);
    expect(screen.queryByLabelText("Chat scope")).toBeNull();
    expect(screen.queryByText("All notes")).toBeNull();
  });
});

describe("/chat header", () => {
  it("states the tenant and who can read it, as separate pills", async () => {
    signIntoWorkspace();
    mockTauri({ chat_history: () => ({ conversationId: null, messages: [] }) });
    renderChat();

    // The visibility claim is the highest-stakes fact on the screen, so it's its
    // own pill rather than a clause in a sentence.
    expect(await screen.findByText("Acme Team")).toBeInTheDocument();
    expect(screen.getByText("All members")).toBeInTheDocument();
    // The old single-line phrasing is gone.
    expect(screen.queryByText(/visible to members/)).toBeNull();
  });

  it("says Personal and Private outside a workspace", async () => {
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => ({ conversationId: null, messages: [] }),
    });
    renderChat();

    expect(await screen.findByText("Personal")).toBeInTheDocument();
    expect(screen.getByText("Private")).toBeInTheDocument();
  });

  it("names the open conversation, and falls back to \"Chat\" for a fresh one", async () => {
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => ({ conversationId: "c1", messages: [] }),
      chat_list_conversations: () => [
        { id: "c1", title: "Budget questions", breadth: "all", updatedAt: 2, messageCount: 4 },
        // Untitled until the backend derives one from the first turn.
        { id: "c2", title: "", breadth: "all", updatedAt: 1, messageCount: 0 },
      ],
    });
    renderChat();

    expect(await screen.findByTitle("Budget questions")).toBeInTheDocument();
    expect(screen.queryByTitle("Chat")).toBeNull();
  });

  it("names the answering model in the header, not the composer row", async () => {
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => ({ conversationId: null, messages: [] }),
      settings_get: (args) => {
        const key = (args as { key?: string }).key;
        if (key === "chat_model") return "gpt-5.1";
        if (key === "onboarding_completed") return "true";
        return null;
      },
    });
    renderChat();

    expect(await screen.findByText("gpt-5.1")).toBeInTheDocument();
    // #80 put this chip in the composer row; on the page it moved to the header,
    // and showing the same fact twice would just be clutter.
    expect(screen.queryByTestId("chat-model-indicator")).toBeNull();
  });

  it("claims no model in a workspace", async () => {
    // The turn runs on the server's model there, so naming the local chat_model
    // would name something that isn't answering (#80).
    signIntoWorkspace();
    mockTauri({
      chat_history: () => ({ conversationId: null, messages: [] }),
      settings_get: (args) => {
        const key = (args as { key?: string }).key;
        if (key === "chat_model") return "gpt-5.1";
        if (key === "onboarding_completed") return "true";
        return null;
      },
    });
    renderChat();

    await screen.findByText("Acme Team");
    expect(screen.queryByText("gpt-5.1")).toBeNull();
  });
});

describe("/chat session chrome placement", () => {
  // Actions live in the app bar, as they do in the Note view, so "new chat" has
  // one home whatever the sidebar is doing. History is the exception: the sidebar
  // list IS the history while it's open, and the popover beside it would be the
  // same thing twice.
  it("keeps new chat in the bar whether or not the sidebar is open", async () => {
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => ({ conversationId: null, messages: [] }),
      chat_list_conversations: () => [
        { id: "c1", title: "Earlier", breadth: "all", updatedAt: 2, messageCount: 4 },
        { id: "c2", title: "Later", breadth: "all", updatedAt: 3, messageCount: 2 },
      ],
    });
    renderChat(false);

    expect(await screen.findByLabelText("New chat")).toBeInTheDocument();
    // Two conversations, so history WOULD be offerable — it's the visible sidebar
    // list that suppresses the popover, not a lack of history.
    expect(screen.queryByTitle("Chat history")).toBeNull();
  });

  it("adds the history popover once the sidebar is collapsed", async () => {
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => ({ conversationId: null, messages: [] }),
      chat_list_conversations: () => [
        { id: "c1", title: "Earlier", breadth: "all", updatedAt: 2, messageCount: 4 },
        { id: "c2", title: "Later", breadth: "all", updatedAt: 3, messageCount: 2 },
      ],
    });
    renderChat(true);

    expect(await screen.findByLabelText("New chat")).toBeInTheDocument();
    // Collapsed, the popover is the only way back to a past thread.
    expect(await screen.findByTitle("Chat history")).toBeInTheDocument();
  });
});

describe("/chat with nothing to retrieve", () => {
  it("holds the composer and says there is nothing to search", async () => {
    useNotesStore.setState({ notes: [], loaded: true });
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => ({ conversationId: null, messages: [] }),
    });
    renderChat();

    const input = await screen.findByLabelText("Ask about your notes");
    // readOnly, not disabled: `disabled` drops the control out of the tab order,
    // so a keyboard or screen-reader user would never reach the placeholder that
    // explains why they can't type.
    expect(input).toHaveAttribute("readonly");
    expect(input).toHaveAttribute("aria-disabled", "true");
    expect(screen.getByPlaceholderText("Nothing to search yet")).toBeInTheDocument();
    // The standing invitation is replaced, not merely supplemented — it would
    // otherwise invite a question whose only answer is "I couldn't find anything".
    expect(screen.getByText(/No notes yet\./)).toBeInTheDocument();
    expect(screen.queryByText(/Ask anything about your notes/)).toBeNull();
  });

  it("holds it in a workspace too, where a wasted turn is metered", async () => {
    signIntoWorkspace();
    useNotesStore.setState({ notes: [], loaded: true });
    mockTauri({ chat_history: () => ({ conversationId: null, messages: [] }) });
    renderChat();

    const input = await screen.findByLabelText("Ask about your notes");
    expect(input).toHaveAttribute("aria-disabled", "true");
  });

  it("says notes are still arriving, not that there are none, mid-sync", async () => {
    // An empty local mirror during a workspace pull is not evidence of an empty
    // workspace — claiming "No notes yet" there would be a false statement about
    // someone's library.
    signIntoWorkspace();
    useNotesStore.setState({ notes: [], loaded: true });
    useCloudStore.setState({ syncStatus: "syncing" });
    mockTauri({ chat_history: () => ({ conversationId: null, messages: [] }) });
    renderChat();

    expect(await screen.findByText(/Still syncing your notes/)).toBeInTheDocument();
    expect(screen.queryByText(/No notes yet\./)).toBeNull();
    expect(screen.getByLabelText("Ask about your notes")).toHaveAttribute("aria-disabled", "true");
  });

  it("leaves the composer open while the first load is still in flight", async () => {
    // notes: [] with loaded: false is "we don't know yet", not "empty" — claiming
    // an empty library here would fire on every launch for one frame.
    useNotesStore.setState({ notes: [], loaded: false });
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => ({ conversationId: null, messages: [] }),
    });
    renderChat();

    const input = await screen.findByPlaceholderText(/Ask about your notes/);
    expect(input).not.toHaveAttribute("readonly");
    expect(input).not.toHaveAttribute("aria-disabled");
  });
});
