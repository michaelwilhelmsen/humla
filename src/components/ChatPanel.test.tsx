import { describe, it, expect, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor, act } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { ChatPanel, type ChatSessionControls } from "./ChatPanel";
import { mockTauri } from "../test/tauri";
import type { ChatMessageDto, Folder, Note } from "../lib/ipc";
import { useCloudStore, DISCONNECTED, type CloudRole } from "../lib/cloud";
import { useNotesStore } from "../lib/store";

// Signed into a MANAGED (billing-enabled) workspace with the given role, so the
// BYOK activation pane (#76) applies. The roster's owner is "Ada" for
// member-facing "ask {owner}" copy.
function signInBillingWorkspace(role: CloudRole = "owner") {
  const ws = { id: "ws1", name: "Acme Team", role, plan_status: "active" as const };
  useCloudStore.setState({
    status: {
      ...DISCONNECTED,
      configured: true,
      logged_in: true,
      base_url: "https://sync.humla.team",
      current_workspace: ws,
      workspaces: [ws],
      billing_enabled: true,
    },
    members: { u1: { id: "u1", name: "Ada", email: "ada@acme.com", role: "owner" } },
  });
}

const UNCONFIGURED_KEY = { configured: false, last4: null, setBy: null, setAt: null, keyHealth: null };
const CONFIGURED_KEY = {
  configured: true,
  last4: "n3Kq",
  setBy: "u1",
  setAt: null,
  keyHealth: "ok",
};

// A minimal note carrying a folder, so the composer breadth picker's
// "Folder: {name}" option appears (#63). Only the fields ChatPanel reads matter.
function seedNoteWithFolder(noteId = "n1", folderName = "Roadmap") {
  const note = { id: noteId, folder_id: "f1" } as unknown as Note;
  const folder = { id: "f1", name: folderName } as unknown as Folder;
  useNotesStore.setState({ notes: [note], folders: [folder] });
}

// Populate the cloud store as if signed into a workspace, so the Teams tenant
// row appears (issue #50).
function signIntoWorkspace(name = "Acme Team") {
  const ws = { id: "ws1", name, role: "owner" as const, plan_status: "active" as const };
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

function userMsg(text: string): ChatMessageDto {
  return { id: "u1", role: "user", seq: 0, parts: [{ type: "text", id: "b0", text }], createdAt: 1 };
}
function assistantMsg(text: string): ChatMessageDto {
  return { id: "a1", role: "assistant", seq: 1, parts: [{ type: "text", id: "b1", text }], createdAt: 2 };
}
// An assistant turn that searched a note and cited it (issue #47).
function assistantWithCitation(text: string): ChatMessageDto {
  return {
    id: "a2",
    role: "assistant",
    seq: 1,
    parts: [
      {
        type: "tool",
        id: "t1",
        name: "search_notes",
        result: "Found 1 excerpt",
        citations: [{ noteId: "cited-note", title: "Kickoff notes", createdAt: 1700000000000 }],
      },
      { type: "text", id: "b1", text },
    ],
    createdAt: 2,
  };
}

// An assistant turn that ran one or more tools (#63), for the persistent
// tool-use receipt above the answer.
function assistantWithTools(text: string, tools: string[]): ChatMessageDto {
  return {
    id: "a3",
    role: "assistant",
    seq: 1,
    parts: [
      ...tools.map((name, i) => ({ type: "tool" as const, id: `t${i}`, name, result: "ok" })),
      { type: "text" as const, id: "b1", text },
    ],
    createdAt: 3,
  };
}

// `chat_history` returns { conversationId, messages } since #61; a non-empty
// history implies a resolved session id.
function history(messages: ChatMessageDto[] = []) {
  return { conversationId: messages.length ? "c1" : null, messages };
}

function renderPanel(noteId = "n1") {
  return render(
    <MemoryRouter>
      <ChatPanel target={{ kind: "note", noteId }} />
    </MemoryRouter>,
  );
}

// Render capturing the header controls ChatPanel publishes (#62). The buttons
// live in the Note header; testing through the controls contract keeps
// ChatPanel's session behavior self-contained.
function renderWithControls(noteId = "n1") {
  const captured: { current: ChatSessionControls | null } = { current: null };
  render(
    <MemoryRouter>
      <ChatPanel
        target={{ kind: "note", noteId }}
        onControls={(c) => {
          captured.current = c;
        }}
      />
    </MemoryRouter>,
  );
  return captured;
}

/** The same, on the library-wide target — which since #120 opens on a draft
 *  rather than resuming, so its behaviour has to be asserted separately. */
function renderGlobalWithControls() {
  const captured: { current: ChatSessionControls | null } = { current: null };
  render(
    <MemoryRouter>
      <ChatPanel
        target={{ kind: "global" }}
        onControls={(c) => {
          captured.current = c;
        }}
      />
    </MemoryRouter>,
  );
  return captured;
}

beforeEach(() => {
  mockTauri();
  // Default: signed out, so Personal is the only tenant unless a test opts in.
  useCloudStore.setState({ status: DISCONNECTED });
  // No note/folder seeded by default (breadth picker shows no folder option).
  useNotesStore.setState({ notes: [], folders: [] });
});

describe("ChatPanel readiness", () => {
  it("shows the setup prompt when OpenAI has no key", async () => {
    renderPanel();
    await waitFor(() => expect(screen.getByText(/Add your OpenAI key/)).toBeInTheDocument());
    expect(screen.queryByPlaceholderText(/Ask about your notes/)).toBeNull();
  });

  it("shows the input + empty state once a key is present", async () => {
    mockTauri({ provider_key_get: () => "sk-test", chat_history: () => history() });
    renderPanel();
    await waitFor(() =>
      expect(screen.getByPlaceholderText(/Ask about your notes/)).toBeInTheDocument(),
    );
    expect(screen.getByText(/Ask anything about your notes/)).toBeInTheDocument();
  });
});

describe("ChatPanel send", () => {
  it("sends the message and renders the reloaded conversation", async () => {
    let sent = false;
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_send: () => {
        sent = true;
        return { conversationId: "c1", truncated: false };
      },
      chat_history: () =>
        history(sent ? [userMsg("What happened?"), assistantMsg("A summary.")] : []),
    });
    renderPanel();

    const input = await screen.findByPlaceholderText(/Ask about your notes/);
    fireEvent.change(input, { target: { value: "What happened?" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    await waitFor(() => expect(screen.getByText("A summary.")).toBeInTheDocument());
    expect(screen.getByText("What happened?")).toBeInTheDocument();
  });

  it("surfaces a truncation notice when the backend truncated the note", async () => {
    let sent = false;
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_send: () => {
        sent = true;
        return { conversationId: "c1", truncated: true };
      },
      chat_history: () => history(sent ? [userMsg("hi"), assistantMsg("hello")] : []),
    });
    renderPanel();
    const input = await screen.findByPlaceholderText(/Ask about your notes/);
    fireEvent.change(input, { target: { value: "hi" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await waitFor(() =>
      expect(screen.getByText(/truncated to fit the context budget/)).toBeInTheDocument(),
    );
  });
});

describe("ChatPanel retrieval UI (#47)", () => {
  it("renders a citation chip for a cited note", async () => {
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () =>
        history([userMsg("what happened?"), assistantWithCitation("Here's what I found.")]),
    });
    renderPanel();
    await waitFor(() => expect(screen.getByText("Here's what I found.")).toBeInTheDocument());
    // The cited note surfaces as a clickable chip by title.
    expect(screen.getByRole("button", { name: /Kickoff notes/ })).toBeInTheDocument();
  });

  it("offers the scope breadths in the Scope popover", async () => {
    mockTauri({ provider_key_get: () => "sk-test", chat_history: () => history() });
    renderPanel();
    const trigger = await screen.findByRole("button", { name: "Chat scope" });
    fireEvent.click(trigger);
    await waitFor(() => expect(screen.getByText("All notes")).toBeInTheDocument());
    // No folder on the anchor note → no "this folder" option.
    expect(screen.queryByText(/^Folder:/)).toBeNull();
  });
});

describe("ChatPanel context pinning (#58)", () => {
  it("renders no tenant picker — chat is pinned to the loaded context", async () => {
    mockTauri({ provider_key_get: () => "sk-test", chat_history: () => history() });
    signIntoWorkspace("Acme Team");
    renderPanel();
    await screen.findByPlaceholderText(/Ask about your notes/);
    // The old Personal/Workspace selector is gone entirely.
    expect(screen.queryByRole("button", { name: "Chat tenant" })).toBeNull();
  });

  it("shows the workspace context line in a workspace (#63)", async () => {
    mockTauri({ provider_key_get: () => "sk-test", chat_history: () => history() });
    signIntoWorkspace("Acme Team");
    renderPanel();
    // A muted line above the thread, exact copy per the ticket.
    const line = await screen.findByText("Chatting in Acme Team · visible to members");
    expect(line).toBeInTheDocument();
    // It owns its vertical space with a bottom hairline so scrolled message
    // content passes beneath a proper boundary rather than colliding with it.
    expect(line.className).toContain("border-b");
  });

  it("renders no context line in personal (signed out) (#63)", async () => {
    mockTauri({ provider_key_get: () => "sk-test", chat_history: () => history() });
    renderPanel();
    await screen.findByPlaceholderText(/Ask about your notes/);
    // Personal context shows nothing — no indicator at all.
    expect(screen.queryByText(/visible to members/)).toBeNull();
    expect(screen.queryByText(/Chatting in/)).toBeNull();
  });
});

describe("ChatPanel sessions (#62)", () => {
  it("hides the history affordance for a lone empty conversation", async () => {
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => ({ conversationId: "c1", messages: [] }),
      chat_list_conversations: () => [
        { id: "c1", title: "", breadth: "note", updatedAt: 1, messageCount: 0 },
      ],
    });
    const controls = renderWithControls();
    await screen.findByPlaceholderText(/Ask about your notes/);
    await waitFor(() => expect(controls.current).not.toBeNull());
    expect(controls.current?.canBrowseHistory).toBe(false);
  });

  it("shows history once the note has another conversation", async () => {
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => ({ conversationId: "c1", messages: [] }),
      chat_list_conversations: () => [
        { id: "c1", title: "", breadth: "note", updatedAt: 20, messageCount: 0 },
        { id: "c2", title: "Earlier chat", breadth: "note", updatedAt: 10, messageCount: 4 },
      ],
    });
    const controls = renderWithControls();
    await screen.findByPlaceholderText(/Ask about your notes/);
    await waitFor(() => expect(controls.current?.canBrowseHistory).toBe(true));
    expect(controls.current?.conversations).toHaveLength(2);
    expect(controls.current?.activeConversationId).toBe("c1");
  });

  // Delete + rename (#109). The panel owns the list, so it owns what happens to
  // the pane afterwards — which is the part a UI test can't see.
  describe("delete + rename (#109)", () => {
    const twoConversations = {
      provider_key_get: () => "sk-test",
      chat_history: () => ({ conversationId: "c1", messages: [] }),
      chat_list_conversations: () => [
        { id: "c1", title: "Open thread", breadth: "note", updatedAt: 20, messageCount: 2 },
        { id: "c2", title: "Older thread", breadth: "note", updatedAt: 10, messageCount: 4 },
      ],
    };

    it("drops a deleted row from the list", async () => {
      const deleted: string[] = [];
      mockTauri({
        ...twoConversations,
        chat_delete_conversation: (args) => {
          const { conversationId } = args as { conversationId: string };
          deleted.push(conversationId);
          return null;
        },
      });
      const controls = renderWithControls();
      await waitFor(() => expect(controls.current?.conversations).toHaveLength(2));

      await controls.current!.deleteConversation("c2");

      expect(deleted).toEqual(["c2"]);
      await waitFor(() => expect(controls.current?.conversations).toHaveLength(1));
      expect(controls.current?.conversations[0].id).toBe("c1");
      // The open conversation is untouched — deleting another row is not a switch.
      expect(controls.current?.activeConversationId).toBe("c1");
    });

    it("lands on a fresh chat when the OPEN conversation is deleted", async () => {
      // The acceptance criterion: never leave the pane pointing at a dead id, or
      // the next send would resurrect it.
      //
      // The backing list is STATEFUL here, because `newChat` re-lists from the
      // backend — a fake that kept returning the deleted row would assert that we
      // tolerate a list contradicting the delete, which is the opposite of the
      // guarantee. This shape also proves the row doesn't come back on the reload.
      const rows = [
        { id: "c1", title: "Open thread", breadth: "note", updatedAt: 20, messageCount: 2 },
        { id: "c2", title: "Older thread", breadth: "note", updatedAt: 10, messageCount: 4 },
      ];
      mockTauri({
        provider_key_get: () => "sk-test",
        chat_history: () => ({ conversationId: "c1", messages: [] }),
        chat_list_conversations: () => [...rows],
        chat_delete_conversation: (args) => {
          const { conversationId } = args as { conversationId: string };
          const i = rows.findIndex((r) => r.id === conversationId);
          if (i >= 0) rows.splice(i, 1);
          return null;
        },
        chat_new_conversation: () => {
          const fresh = {
            id: "c-fresh",
            title: "",
            breadth: "note",
            updatedAt: 30,
            messageCount: 0,
          };
          rows.unshift(fresh);
          return fresh;
        },
      });
      const controls = renderWithControls();
      await waitFor(() => expect(controls.current?.activeConversationId).toBe("c1"));

      await controls.current!.deleteConversation("c1");

      await waitFor(() => expect(controls.current?.activeConversationId).toBe("c-fresh"));
      expect(controls.current?.conversations.some((c) => c.id === "c1")).toBe(false);
    });

    it("keeps the row when the backend refuses the delete", async () => {
      // A failed delete that removed the row anyway would read as success and leave
      // the list disagreeing with the backend.
      mockTauri({
        ...twoConversations,
        chat_delete_conversation: () => {
          throw new Error("Only the conversation's creator can delete it.");
        },
      });
      const controls = renderWithControls();
      await waitFor(() => expect(controls.current?.conversations).toHaveLength(2));

      await controls.current!.deleteConversation("c2");

      expect(controls.current?.conversations).toHaveLength(2);
      expect(await screen.findByText(/creator can delete/)).toBeInTheDocument();
    });

    it("renames optimistically and adopts the row the backend returns", async () => {
      mockTauri({
        ...twoConversations,
        chat_rename_conversation: (args) => ({
          id: "c2",
          title: (args as { title: string }).title,
          breadth: "note",
          updatedAt: 99,
          messageCount: 4,
        }),
      });
      const controls = renderWithControls();
      await waitFor(() => expect(controls.current?.conversations).toHaveLength(2));

      await controls.current!.renameConversation("c2", "Q3 pricing");

      await waitFor(() => {
        const row = controls.current?.conversations.find((c) => c.id === "c2");
        expect(row?.title).toBe("Q3 pricing");
        // The bumped timestamp comes back with it, so ordering tracks the rename.
        expect(row?.updatedAt).toBe(99);
      });
    });

    it("does not store the placeholder label as a real title", async () => {
      // Both editors seed from the row's DISPLAYED label, so an untitled thread
      // offers "New chat". Accepting that unchanged must not write it: a stored
      // title is what stops the first turn from naming the thread, so this would
      // make the placeholder permanent.
      let renames = 0;
      mockTauri({
        provider_key_get: () => "sk-test",
        chat_history: () => ({ conversationId: "c1", messages: [] }),
        chat_list_conversations: () => [
          { id: "c1", title: "", breadth: "note", updatedAt: 20, messageCount: 0 },
          { id: "c2", title: "Older thread", breadth: "note", updatedAt: 10, messageCount: 4 },
        ],
        chat_rename_conversation: () => {
          renames += 1;
          return null;
        },
      });
      const controls = renderWithControls();
      await waitFor(() => expect(controls.current?.conversations).toHaveLength(2));

      await controls.current!.renameConversation("c1", "New chat");
      expect(renames).toBe(0);

      // A genuine name still goes through.
      await controls.current!.renameConversation("c1", "Kickoff questions");
      expect(renames).toBe(1);
    });

    it("puts the old title back when a rename is refused", async () => {
      mockTauri({
        ...twoConversations,
        chat_rename_conversation: () => {
          throw new Error("A conversation needs a name.");
        },
      });
      const controls = renderWithControls();
      await waitFor(() => expect(controls.current?.conversations).toHaveLength(2));

      await controls.current!.renameConversation("c2", "doomed rename");

      await waitFor(() =>
        expect(controls.current?.conversations.find((c) => c.id === "c2")?.title).toBe(
          "Older thread",
        ),
      );
    });
  });

  // Opening on a draft (#120). The backend decides what a bare `chat_history`
  // resolves to; what the PANE owns is persisting nothing until the first turn —
  // so these assert on IPC that must NOT happen, which is the whole point.
  describe("a library-wide pane opens on a draft (#120)", () => {
    const aFinishedThread = {
      provider_key_get: () => "sk-test",
      // What the backend now returns for a bare library-wide request: no
      // conversation, even though one exists in the list below.
      chat_history: () => ({ conversationId: null, messages: [] }),
      chat_get_breadth: () => "all",
      chat_list_conversations: () => [
        { id: "c-old", title: "Last week's question", breadth: "all", updatedAt: 5, messageCount: 6 },
      ],
    };

    it("shows no conversation, so the prompt cards stand where a thread would", async () => {
      mockTauri(aFinishedThread);
      const controls = renderGlobalWithControls();
      await screen.findByPlaceholderText(/Ask about your notes/);
      await waitFor(() => expect(controls.current).not.toBeNull());

      expect(controls.current?.activeConversationId).toBeNull();
      // The past thread is still reachable — drafting hides nothing from the list.
      await waitFor(() => expect(controls.current?.conversations).toHaveLength(1));
    });

    it("'+' costs no IPC and creates no row", async () => {
      // A drafting pane is already the empty state "+" would produce, so creating
      // anything would only leave a row behind if the user changed their mind.
      let created = 0;
      mockTauri({
        ...aFinishedThread,
        chat_new_conversation: () => {
          created += 1;
          return { id: "c-new", title: "", breadth: "all", updatedAt: 9, messageCount: 0 };
        },
      });
      const controls = renderGlobalWithControls();
      await screen.findByPlaceholderText(/Ask about your notes/);
      await waitFor(() => expect(controls.current).not.toBeNull());

      await act(async () => {
        await controls.current!.newChat();
      });

      expect(created).toBe(0);
      expect(controls.current?.activeConversationId).toBeNull();
    });

    it("does not persist a breadth or pin chosen before the first turn", async () => {
      // The trap this replaces: writing them lazily created a row the list hid but
      // the next send resolved to, so the chip could read "off" while the stored
      // row narrowed the turn anyway (#103).
      let breadthWrites = 0;
      let pinWrites = 0;
      mockTauri({
        ...aFinishedThread,
        chat_set_breadth: () => {
          breadthWrites += 1;
          return null;
        },
        chat_set_owner_filter: () => {
          pinWrites += 1;
          return null;
        },
      });
      renderGlobalWithControls();
      await screen.findByPlaceholderText(/Ask about your notes/);

      expect(breadthWrites).toBe(0);
      expect(pinWrites).toBe(0);
    });

    it("carries the draft's breadth into the turn that creates the conversation", async () => {
      let sent: Record<string, unknown> | null = null;
      mockTauri({
        ...aFinishedThread,
        chat_send: (args) => {
          sent = args as Record<string, unknown>;
          return { conversationId: "c-fresh", truncated: false };
        },
      });
      renderGlobalWithControls();
      const input = await screen.findByPlaceholderText(/Ask about your notes/);

      fireEvent.change(input, { target: { value: "what did we decide?" } });
      fireEvent.keyDown(input, { key: "Enter" });

      await waitFor(() => expect(sent).not.toBeNull());
      // No conversation to append to, so the settings ride along...
      expect(sent!.conversationId).toBeNull();
      expect(sent!.draftBreadth).toBe("all");
      // ...and an absent pin is null, not "" — "" would read as a pin on nobody.
      expect(sent!.draftOwnerFilter).toBeNull();
    });

    it("stops sending draft settings once a conversation exists", async () => {
      // With a row open it is the source of truth for both, so re-sending them
      // could only introduce a disagreement.
      const sends: Record<string, unknown>[] = [];
      mockTauri({
        ...aFinishedThread,
        chat_send: (args) => {
          sends.push(args as Record<string, unknown>);
          return { conversationId: "c-fresh", truncated: false };
        },
      });
      renderGlobalWithControls();
      const input = await screen.findByPlaceholderText(/Ask about your notes/);

      fireEvent.change(input, { target: { value: "first" } });
      fireEvent.keyDown(input, { key: "Enter" });
      await waitFor(() => expect(sends).toHaveLength(1));

      fireEvent.change(input, { target: { value: "second" } });
      fireEvent.keyDown(input, { key: "Enter" });
      await waitFor(() => expect(sends).toHaveLength(2));

      expect(sends[1].conversationId).toBe("c-fresh");
      expect(sends[1].draftBreadth).toBeNull();
    });

    it("keeps the question, and the thread, when a draft's first turn fails", async () => {
      // The backend persists the user message and then rolls back only the
      // assistant placeholder, so a failed first turn leaves a real conversation we
      // were never told the id of. Refetching with `null` resolves to nothing on a
      // drafting target, which would blank the question while a one-sided row
      // appeared in the sidebar — and a retry would open a second thread.
      const rows = [
        { id: "c-created", title: "why did the build break", breadth: "all", updatedAt: 20, messageCount: 1 },
      ];
      mockTauri({
        provider_key_get: () => "sk-test",
        chat_history: (args) => {
          const { conversationId } = args as { conversationId: string | null };
          // Mirrors the new backend: a bare library-wide request resolves to nothing.
          if (!conversationId) return { conversationId: null, messages: [] };
          return { conversationId, messages: [userMsg("why did the build break")] };
        },
        chat_get_breadth: () => "all",
        chat_list_conversations: () => [...rows],
        chat_send: () => {
          throw new Error("The model is unavailable.");
        },
      });
      const controls = renderGlobalWithControls();
      const input = await screen.findByPlaceholderText(/Ask about your notes/);

      fireEvent.change(input, { target: { value: "why did the build break" } });
      fireEvent.keyDown(input, { key: "Enter" });

      expect(await screen.findByText(/The model is unavailable/)).toBeInTheDocument();
      // The question survives...
      expect(await screen.findByText("why did the build break")).toBeInTheDocument();
      // ...and the pane has adopted the conversation the failed turn created, so a
      // retry continues it.
      await waitFor(() => expect(controls.current?.activeConversationId).toBe("c-created"));
    });

    it("a NOTE's pane still resumes its thread and still creates on '+'", async () => {
      // The deliberate divergence: a note is an anchor, so returning to it
      // continues the same line of thinking. Asserted so it stays a decision.
      let created = 0;
      mockTauri({
        provider_key_get: () => "sk-test",
        chat_history: () => ({
          conversationId: "c1",
          messages: [userMsg("Old question?"), assistantMsg("Old answer.")],
        }),
        chat_list_conversations: () => [
          { id: "c1", title: "First", breadth: "note", updatedAt: 5, messageCount: 2 },
        ],
        chat_new_conversation: () => {
          created += 1;
          return { id: "c2", title: "", breadth: "note", updatedAt: 9, messageCount: 0 };
        },
      });
      const controls = renderWithControls();
      await waitFor(() => expect(controls.current?.activeConversationId).toBe("c1"));
      expect(await screen.findByText("Old answer.")).toBeInTheDocument();

      await act(async () => {
        await controls.current!.newChat();
      });
      expect(created).toBe(1);
    });
  });

  // Paging (#95): `/chat` lists conversations uncapped in the sidebar, so the
  // panel fetches them a page at a time. It owns the list, so it owns the paging.
  describe("paging", () => {
    const page = (from: number, count: number) =>
      Array.from({ length: count }, (_, i) => ({
        id: `c${from + i}`,
        title: `Chat ${from + i}`,
        breadth: "all" as const,
        updatedAt: 1000 - (from + i),
        messageCount: 2,
      }));

    // The panel's page size, inferred from the first request rather than hardcoded
    // here — the constant is the panel's business, the tiling is what matters.
    async function firstPage() {
      let size = 0;
      const seen: number[] = [];
      mockTauri({
        provider_key_get: () => "sk-test",
        chat_history: () => history(),
        chat_list_conversations: (args) => {
          const { limit, offset } = args as { limit: number; offset: number };
          size = limit;
          seen.push(offset);
          // A full page while rows remain, then a short one: that's how the panel
          // learns where the end is, with no total count anywhere.
          if (offset === 0) return page(0, limit);
          if (offset === limit) return page(limit, 2);
          return [];
        },
      });
      const controls = renderWithControls();
      await screen.findByPlaceholderText(/Ask about your notes/);
      await waitFor(() => expect(controls.current?.conversations.length).toBe(size));
      return { controls, size, seen };
    }

    it("appends the next page and then reports the end", async () => {
      const { controls, size } = await firstPage();
      expect(controls.current?.hasMore).toBe(true);

      await act(async () => {
        await controls.current!.loadMore();
      });
      await waitFor(() => expect(controls.current?.conversations.length).toBe(size + 2));
      // A short page means the end; nothing further is requested.
      expect(controls.current?.hasMore).toBe(false);
      const before = controls.current!.conversations.length;
      await act(async () => {
        await controls.current!.loadMore();
      });
      expect(controls.current?.conversations.length).toBe(before);
    });

    it("asks for each page exactly once for one gesture", async () => {
      const { controls, seen, size } = await firstPage();
      // A scroll observer fires repeatedly; a second call while the first is in
      // flight must not fetch the same window twice.
      await act(async () => {
        await Promise.all([controls.current!.loadMore(), controls.current!.loadMore()]);
      });
      expect(seen.filter((o) => o === size)).toHaveLength(1);
    });

    it("does not list a conversation twice when one is bumped between pages", async () => {
      // A thread that gets a new message moves to page one, shifting the window —
      // so page two can hand back a row already on screen.
      mockTauri({
        provider_key_get: () => "sk-test",
        chat_history: () => history(),
        chat_list_conversations: (args) => {
          const { limit, offset } = args as { limit: number; offset: number };
          return offset === 0 ? page(0, limit) : [...page(limit - 1, 1), ...page(limit, 1)];
        },
      });
      const controls = renderWithControls();
      await screen.findByPlaceholderText(/Ask about your notes/);
      await waitFor(() => expect(controls.current!.conversations.length).toBeGreaterThan(0));

      await act(async () => {
        await controls.current!.loadMore();
      });
      const ids = controls.current!.conversations.map((c) => c.id);
      expect(new Set(ids).size).toBe(ids.length);
    });
  });

  it("'+' starts a fresh conversation and switches the pane to it", async () => {
    let created = false;
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => ({
        conversationId: "c1",
        messages: [userMsg("Old question?"), assistantMsg("Old answer.")],
      }),
      chat_list_conversations: () => [
        { id: "c1", title: "First", breadth: "note", updatedAt: 5, messageCount: 2 },
      ],
      chat_new_conversation: () => {
        created = true;
        // Inherits the prior breadth ("all") per #61.
        return { id: "c2", title: "", breadth: "all", updatedAt: 9, messageCount: 0 };
      },
    });
    const controls = renderWithControls();
    await screen.findByText("Old answer.");
    await waitFor(() => expect(controls.current).not.toBeNull());

    await act(async () => {
      await controls.current!.newChat();
    });

    expect(created).toBe(true);
    // The pane is now the fresh, empty conversation.
    expect(screen.queryByText("Old answer.")).toBeNull();
    expect(screen.getByText(/Ask anything about your notes/)).toBeInTheDocument();
    await waitFor(() => expect(controls.current?.activeConversationId).toBe("c2"));
    // Inherited breadth is reflected in the Scope chip.
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Chat scope" })).toHaveTextContent("All notes"),
    );
  });

  it("a slow initial history load can't clobber a conversation started with '+'", async () => {
    // Hold the mount history fetch open so it resolves AFTER newChat() lands.
    let releaseInitialHistory: (() => void) | null = null;
    let historyCalls = 0;
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: (args) => {
        const cid = (args as { conversationId?: string | null }).conversationId ?? null;
        historyCalls += 1;
        if (cid === null && historyCalls === 1) {
          return new Promise((resolve) => {
            releaseInitialHistory = () =>
              resolve({
                conversationId: "c1",
                messages: [userMsg("Old question?"), assistantMsg("Stale answer.")],
              });
          });
        }
        return { conversationId: cid ?? "c1", messages: [] };
      },
      chat_list_conversations: () => [
        { id: "c1", title: "First", breadth: "note", updatedAt: 5, messageCount: 2 },
      ],
      chat_new_conversation: () => ({
        id: "c2",
        title: "",
        breadth: "note",
        updatedAt: 9,
        messageCount: 0,
      }),
    });
    const controls = renderWithControls();
    await waitFor(() => expect(controls.current).not.toBeNull());

    // Switch to a fresh conversation while the initial history is still pending.
    await act(async () => {
      await controls.current!.newChat();
    });
    expect(controls.current?.activeConversationId).toBe("c2");

    // Now let the stale initial fetch resolve — it must be ignored.
    await act(async () => {
      releaseInitialHistory?.();
      await Promise.resolve();
    });
    expect(screen.queryByText("Stale answer.")).toBeNull();
    expect(controls.current?.activeConversationId).toBe("c2");
  });

  it("loads a chosen conversation's messages and its stored breadth", async () => {
    const historyArgs: (string | null)[] = [];
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: (args) => {
        const cid = (args as { conversationId?: string | null }).conversationId ?? null;
        historyArgs.push(cid);
        return cid === "c2"
          ? { conversationId: "c2", messages: [userMsg("Q2"), assistantMsg("Answer two.")] }
          : { conversationId: "c1", messages: [userMsg("Q1"), assistantMsg("Answer one.")] };
      },
      chat_get_breadth: (args) =>
        (args as { conversationId?: string | null }).conversationId === "c2" ? "all" : "note",
      chat_list_conversations: () => [
        { id: "c1", title: "First", breadth: "note", updatedAt: 20, messageCount: 2 },
        { id: "c2", title: "Second", breadth: "all", updatedAt: 10, messageCount: 2 },
      ],
    });
    const controls = renderWithControls();
    await screen.findByText("Answer one.");
    await waitFor(() => expect(controls.current).not.toBeNull());

    await act(async () => {
      await controls.current!.openConversation("c2");
    });

    await waitFor(() => expect(screen.getByText("Answer two.")).toBeInTheDocument());
    expect(screen.queryByText("Answer one.")).toBeNull();
    expect(historyArgs).toContain("c2");
    // The Scope chip tracks the loaded conversation's stored breadth.
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Chat scope" })).toHaveTextContent("All notes"),
    );
  });
});

describe("ChatPanel scope breadth (#58)", () => {
  it("initialises the scope chip from the persisted breadth", async () => {
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => history(),
      chat_get_breadth: () => "all",
    });
    renderPanel();
    // The chip reflects the backend's persisted breadth, not the "note" default.
    const trigger = await screen.findByRole("button", { name: "Chat scope" });
    await waitFor(() => expect(trigger).toHaveTextContent("All notes"));
  });

  it("persists a breadth change via chatSetBreadth", async () => {
    let setArgs: { noteId?: string; breadth?: string } | undefined;
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => history(),
      chat_get_breadth: () => "note",
      chat_set_breadth: (args) => {
        setArgs = args as { noteId?: string; breadth?: string };
        return undefined;
      },
    });
    renderPanel();
    fireEvent.click(await screen.findByRole("button", { name: "Chat scope" }));
    fireEvent.click(await screen.findByText("All notes"));
    await waitFor(() => expect(setArgs?.breadth).toBe("all"));
    expect(setArgs?.noteId).toBe("n1");
  });
});

// #94: the panel is parameterised by a target, so a library-wide pane renders
// from the same component. No route reaches one yet (#95), so it's exercised
// here — the refactor's whole claim is that this works without a second
// implementation.
describe("ChatPanel with a library-wide target (#94)", () => {
  function renderGlobal() {
    return render(
      <MemoryRouter>
        <ChatPanel target={{ kind: "global" }} />
      </MemoryRouter>,
    );
  }

  it("sends a null anchor to every chat command, never an empty string", async () => {
    // `null` is the wire value that means "the whole library"; the backend
    // REJECTS "" outright (#93), so an empty string here would be a hard error.
    const seen: Record<string, unknown> = {};
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: (args) => {
        seen.history = args;
        return history();
      },
      chat_get_breadth: (args) => {
        seen.breadth = args;
        return "all";
      },
      chat_list_conversations: (args) => {
        seen.list = args;
        return [];
      },
    });
    renderGlobal();

    await waitFor(() => expect(seen.history).toBeDefined());
    for (const [name, args] of Object.entries(seen)) {
      expect((args as { noteId?: unknown }).noteId, `${name} anchor`).toBeNull();
    }
  });

  it("shows no breadth chrome at all, so no anchor-dependent option can appear", async () => {
    // A library-wide conversation has no anchor to narrow to, so BOTH "This note"
    // and "Folder: …" are meaningless — offering either would let the chip show a
    // breadth the backend's `check_anchor` is guaranteed to reject, i.e. lie about
    // what the next turn will search. #94 suppressed those two options, which left
    // a one-option picker; #95 removed the picker itself, since a dropdown whose
    // only entry is "All notes" is noise. Seeding a note WITH a folder proves the
    // options are gone because there's no anchor, not because no folder exists.
    seedNoteWithFolder();
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => history(),
      // Unreadable stored breadth: the pane must still come up (falling back to
      // "all"), not error or stall.
      chat_get_breadth: () => {
        throw new Error("unavailable");
      },
    });
    renderGlobal();

    await screen.findByPlaceholderText(/Ask about your notes/);
    expect(screen.queryByRole("button", { name: "Chat scope" })).toBeNull();
    expect(screen.queryByText(/^Folder:/)).toBeNull();
    expect(screen.queryByText("This note")).toBeNull();
    expect(screen.queryByText("All notes")).toBeNull();
  });

  it("keeps the note pane's own scope options intact", async () => {
    // The other half of the same rule: an anchored pane still offers everything it
    // did before. This is the regression the suppression above could cause.
    seedNoteWithFolder();
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => history(),
      chat_get_breadth: () => "note",
    });
    renderPanel();

    fireEvent.click(await screen.findByRole("button", { name: "Chat scope" }));
    // "This note" appears twice (trigger + row); the other two once each.
    await waitFor(() => expect(screen.getAllByText("This note").length).toBeGreaterThan(0));
    expect(screen.getByText(/^Folder:/)).toBeInTheDocument();
    expect(screen.getByText("All notes")).toBeInTheDocument();
  });

  it("publishes controls under a target key distinct from any note's", async () => {
    // The header's stale-projection guard compares this key, so a library pane's
    // projection must never be mistaken for note "global"'s and vice versa.
    const captured: { current: ChatSessionControls | null } = { current: null };
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => history(),
      chat_get_breadth: () => "all",
      chat_list_conversations: () => [],
    });
    render(
      <MemoryRouter>
        <ChatPanel
          target={{ kind: "global" }}
          onControls={(c) => {
            captured.current = c;
          }}
        />
      </MemoryRouter>,
    );
    await waitFor(() => expect(captured.current).not.toBeNull());
    expect(captured.current?.targetKey).toBe("global");
    expect(captured.current?.targetKey).not.toBe("note:global");
  });
});

describe("ChatPanel accessibility + contrast (#64)", () => {
  it("exposes the message area as an aria-live log", async () => {
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => history([userMsg("q"), assistantMsg("a")]),
    });
    renderPanel();
    await screen.findByText("a");
    const log = screen.getByRole("log");
    expect(log).toHaveAttribute("aria-live", "polite");
  });

  it("renders messages as a list with visually-hidden author labels", async () => {
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => history([userMsg("my question"), assistantMsg("the answer")]),
    });
    renderPanel();
    await screen.findByText("the answer");
    expect(screen.getByRole("list")).toBeInTheDocument();
    expect(screen.getAllByRole("listitem")).toHaveLength(2);
    // Authorship never depends on colour/alignment alone.
    expect(screen.getByText("You:")).toBeInTheDocument();
    expect(screen.getByText("Assistant:")).toBeInTheDocument();
  });

  it("gives the composer textarea an accessible name", async () => {
    mockTauri({ provider_key_get: () => "sk-test", chat_history: () => history() });
    renderPanel();
    expect(
      await screen.findByRole("textbox", { name: /ask about your notes/i }),
    ).toBeInTheDocument();
  });

  it("focuses the composer when the pane is ready", async () => {
    mockTauri({ provider_key_get: () => "sk-test", chat_history: () => history() });
    renderPanel();
    const input = await screen.findByRole("textbox", { name: /ask about your notes/i });
    await waitFor(() => expect(document.activeElement).toBe(input));
  });

  it("returns focus to the composer after starting a new chat", async () => {
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => history([userMsg("q"), assistantMsg("a")]),
      chat_list_conversations: () => [
        { id: "c1", title: "First", breadth: "note", updatedAt: 5, messageCount: 2 },
      ],
      chat_new_conversation: () => ({
        id: "c2",
        title: "",
        breadth: "note",
        updatedAt: 9,
        messageCount: 0,
      }),
    });
    const controls = renderWithControls();
    await screen.findByText("a");
    await waitFor(() => expect(controls.current).not.toBeNull());
    await act(async () => {
      await controls.current!.newChat();
    });
    const input = screen.getByRole("textbox", { name: /ask about your notes/i });
    await waitFor(() => expect(document.activeElement).toBe(input));
  });

  it("uses the muted (not disabled) placeholder colour on the composer", async () => {
    mockTauri({ provider_key_get: () => "sk-test", chat_history: () => history() });
    renderPanel();
    const input = await screen.findByRole("textbox", { name: /ask about your notes/i });
    expect(input.className).toContain("placeholder:text-[var(--color-text-muted)]");
    expect(input.className).not.toContain("text-disabled");
  });

  it("marks the log busy during a bulk load and clears it once messages land", async () => {
    // Hold the initial history load open so the bulk-load window is observable.
    let releaseHistory: (() => void) | null = null;
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () =>
        new Promise((resolve) => {
          releaseHistory = () => resolve({ conversationId: null, messages: [] });
        }),
    });
    renderPanel();
    const log = await screen.findByRole("log");
    // Busy while the wholesale list load is pending — SR defers announcing it.
    expect(log).toHaveAttribute("aria-busy", "true");
    await act(async () => {
      releaseHistory?.();
      await Promise.resolve();
    });
    await waitFor(() => expect(log).toHaveAttribute("aria-busy", "false"));
  });

  it("gives the composer a visible focus-within treatment", async () => {
    mockTauri({ provider_key_get: () => "sk-test", chat_history: () => history() });
    renderPanel();
    const input = await screen.findByRole("textbox", { name: /ask about your notes/i });
    // The textarea has no native outline, so its wrapper shows focus.
    expect(input.parentElement?.className).toContain(
      "focus-within:border-[var(--color-text-muted)]",
    );
  });
});

describe("ChatPanel composer breadth + chrome (#63)", () => {
  it("shows the current breadth on the composer trigger (not just inside the popover)", async () => {
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => history(),
      chat_get_breadth: () => "all",
    });
    renderPanel();
    const trigger = await screen.findByRole("button", { name: "Chat scope" });
    // The active breadth is always visible on the trigger itself.
    await waitFor(() => expect(trigger).toHaveTextContent("All notes"));
  });

  it("offers 'Folder: {name}' only when the note has a folder", async () => {
    seedNoteWithFolder("n1", "Roadmap");
    mockTauri({ provider_key_get: () => "sk-test", chat_history: () => history() });
    renderPanel();
    fireEvent.click(await screen.findByRole("button", { name: "Chat scope" }));
    expect(await screen.findByText("Folder: Roadmap")).toBeInTheDocument();
  });

  it("renders the citation chip without the uppercase-mono nd-chip class", async () => {
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () =>
        history([userMsg("what happened?"), assistantWithCitation("Here's what I found.")]),
    });
    renderPanel();
    const chip = await screen.findByRole("button", { name: /Kickoff notes/ });
    expect(chip.className).not.toContain("nd-chip");
  });

  it("shows a persistent, aggregated tool-use line above a tool-using answer", async () => {
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () =>
        history([
          userMsg("what happened?"),
          assistantWithTools("Here's the answer.", ["search_notes", "get_note", "get_note"]),
        ]),
    });
    renderPanel();
    await screen.findByText("Here's the answer.");
    // Aggregated, past-tense, one line — search then a pluralised read count.
    const line = screen.getByText("Searched your notes · Read 2 notes");
    expect(line).toBeInTheDocument();
    // Reads like body text: same size as the answer (text-sm, not text-xs) and
    // flush-left with the answer block (no px inset), with paragraph margins.
    expect(line.className).toContain("text-sm");
    expect(line.className).not.toContain("text-xs");
    expect(line.className).not.toContain("px-1");
  });

  it("shows no tool-use line for an answer that used no tools", async () => {
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => history([userMsg("hi"), assistantMsg("Plain answer.")]),
    });
    renderPanel();
    await screen.findByText("Plain answer.");
    expect(
      screen.queryByText(/Searched your notes|Read a note|Read \d+ notes|Browsed your notes|Used a tool/),
    ).toBeNull();
  });

  it("drops the assistant bubble and puts user messages in the gray bubble", async () => {
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => history([userMsg("my question"), assistantMsg("the answer")]),
    });
    renderPanel();
    // User turn: right-aligned gray bubble (not the old amber accent pair).
    const userBubble = (await screen.findByText("my question")).parentElement!;
    expect(userBubble.className).toContain("bg-[var(--color-pill-hover)]");
    expect(userBubble.className).not.toContain("accent-soft");
    // Assistant turn: plain block, no bubble background / rounding.
    const answerBlock = screen.getByText("the answer").parentElement!;
    expect(answerBlock.className).toContain("prose-summary");
    expect(answerBlock.className).not.toContain("rounded-[var(--radius-card)]");
    expect(answerBlock.className).not.toContain("bg-[var(--color-pill-hover)]");
  });

  it("animates the thinking indicator with a reduced-motion guard", async () => {
    let releaseSend: (() => void) | null = null;
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => history(),
      chat_send: () =>
        new Promise((resolve) => {
          releaseSend = () => resolve({ conversationId: "c1", truncated: false });
        }),
    });
    renderPanel();
    const input = await screen.findByPlaceholderText(/Ask about your notes/);
    fireEvent.change(input, { target: { value: "hi" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    // While the send is in flight the thinking line shows, animated but gated
    // behind prefers-reduced-motion. The activity line shares these classes.
    const thinking = await screen.findByText("Thinking…");
    expect(thinking.className).toContain("animate-pulse");
    expect(thinking.className).toContain("motion-reduce:animate-none");
    await act(async () => {
      releaseSend?.();
      await Promise.resolve();
    });
  });
});

describe("ChatPanel turn allowance (#69)", () => {
  it("shows the workspace turn allowance bottom-right of the composer", async () => {
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => history(),
      chat_usage: () => ({ used: 2, cap: 3, periodEnd: 0 }),
    });
    signIntoWorkspace("Acme Team");
    renderPanel();
    expect(await screen.findByText("2/3 turns")).toBeInTheDocument();
  });

  it("renders no allowance in personal context", async () => {
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => history(),
      // Even if the backend returned a reading, personal must never show it.
      chat_usage: () => ({ used: 2, cap: 3, periodEnd: 0 }),
    });
    renderPanel();
    await screen.findByPlaceholderText(/ask about your notes/i);
    expect(screen.queryByText(/turns/)).toBeNull();
  });

  it("hides the allowance when the backend returns null (unmetered / unavailable)", async () => {
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => history(),
      chat_usage: () => null,
    });
    signIntoWorkspace("Acme Team");
    renderPanel();
    await screen.findByPlaceholderText(/ask about your notes/i);
    expect(screen.queryByText(/turns/)).toBeNull();
  });

  it("refreshes the allowance after a completed send", async () => {
    let sent = false;
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_send: () => {
        sent = true;
        return { conversationId: "c1", truncated: false };
      },
      chat_history: () => history(sent ? [userMsg("q"), assistantMsg("a")] : []),
      // Allowance consumed by the completed turn: 2/3 before, 3/3 after.
      chat_usage: () => ({ used: sent ? 3 : 2, cap: 3, periodEnd: 0 }),
    });
    signIntoWorkspace("Acme Team");
    renderPanel();
    expect(await screen.findByText("2/3 turns")).toBeInTheDocument();
    const input = await screen.findByRole("textbox", { name: /ask about your notes/i });
    fireEvent.change(input, { target: { value: "q" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await waitFor(() => expect(screen.getByText("3/3 turns")).toBeInTheDocument());
  });

  it("clears the old workspace's meter on a workspace switch until the new fetch resolves", async () => {
    let releaseB: (() => void) | null = null;
    let phase: "A" | "B" = "A";
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => history(),
      chat_usage: () =>
        phase === "A"
          ? { used: 2, cap: 3, periodEnd: 0 }
          : new Promise((resolve) => {
              releaseB = () => resolve({ used: 1, cap: 9, periodEnd: 0 });
            }),
    });
    signIntoWorkspace("Acme Team");
    renderPanel();
    expect(await screen.findByText("2/3 turns")).toBeInTheDocument();

    // Switch to a different workspace whose usage fetch is still pending.
    phase = "B";
    act(() => {
      const ws = { id: "ws2", name: "Beta Team", role: "owner" as const, plan_status: "active" as const };
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
    });
    // The old workspace's numbers must not linger while B's fetch is pending.
    await waitFor(() => expect(screen.queryByText("2/3 turns")).toBeNull());
    expect(screen.queryByText(/turns/)).toBeNull();

    // Once B resolves, its own numbers appear.
    await act(async () => {
      releaseB?.();
      await Promise.resolve();
    });
    expect(await screen.findByText("1/9 turns")).toBeInTheDocument();
  });

  it("hides the meter when the usage fetch throws", async () => {
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => history(),
      chat_usage: () => {
        throw new Error("usage boom");
      },
    });
    signIntoWorkspace("Acme Team");
    renderPanel();
    await screen.findByPlaceholderText(/ask about your notes/i);
    expect(screen.queryByText(/turns/)).toBeNull();
  });

  it("keeps the meter value after a failed send (a failed turn consumes nothing)", async () => {
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => history(),
      chat_send: () => {
        throw new Error("send failed");
      },
      // The server value is unchanged by a failed turn; #76 re-detects activation
      // in the catch (which reads usage), but the displayed number stays 2/3.
      chat_usage: () => ({ used: 2, cap: 3, periodEnd: 0 }),
    });
    signIntoWorkspace("Acme Team");
    renderPanel();
    expect(await screen.findByText("2/3 turns")).toBeInTheDocument();
    const input = await screen.findByRole("textbox", { name: /ask about your notes/i });
    fireEvent.change(input, { target: { value: "q" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await screen.findByText(/send failed/);
    expect(screen.getByText("2/3 turns")).toBeInTheDocument();
  });

  it("colours the meter with the danger tone at the cap (#69)", async () => {
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => history(),
      chat_usage: () => ({ used: 3, cap: 3, periodEnd: 0 }),
    });
    signIntoWorkspace("Acme Team");
    renderPanel();
    const meter = await screen.findByText("3/3 turns");
    expect(meter.className).toContain("text-[var(--color-status-danger)]");
  });
});

describe("ChatPanel breadth icons (#69)", () => {
  it("shows the active scope's icon on the breadth trigger", async () => {
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => history(),
      chat_get_breadth: () => "all",
    });
    renderPanel();
    const trigger = await screen.findByRole("button", { name: "Chat scope" });
    await waitFor(() => expect(trigger).toHaveTextContent("All notes"));
    // "All notes" → the Files glyph (distinct from FileText / ChevronDown).
    expect(trigger.querySelector(".lucide-files")).not.toBeNull();
  });
});

describe("ChatPanel workspace activation (#76)", () => {
  it("owner sees the activation pane with key entry when not activated", async () => {
    mockTauri({
      chat_history: () => history(),
      chat_key_meta: () => UNCONFIGURED_KEY,
      chat_usage: () => null,
    });
    signInBillingWorkspace("owner");
    renderPanel();
    // Values-first line respecting the claim boundary ("your OpenAI relationship").
    expect(await screen.findByText(/your OpenAI relationship/)).toBeInTheDocument();
    expect(screen.getByLabelText("OpenAI API key")).toBeInTheDocument();
    // The composer is replaced by the activation state.
    expect(screen.queryByPlaceholderText(/Ask about your notes/)).toBeNull();
  });

  it("member sees an ask-owner message with nothing actionable", async () => {
    mockTauri({
      chat_history: () => history(),
      chat_key_meta: () => UNCONFIGURED_KEY,
      chat_usage: () => null,
    });
    signInBillingWorkspace("member");
    renderPanel();
    expect(await screen.findByText(/ask Ada/)).toBeInTheDocument();
    expect(screen.queryByLabelText("OpenAI API key")).toBeNull();
    expect(screen.queryByPlaceholderText(/Ask about your notes/)).toBeNull();
  });

  it("flips to the composer after the owner activates, without a reload", async () => {
    mockTauri({
      chat_history: () => history(),
      chat_key_meta: () => UNCONFIGURED_KEY,
      chat_usage: () => null,
      chat_key_set: () => CONFIGURED_KEY,
    });
    signInBillingWorkspace("owner");
    renderPanel();
    const input = await screen.findByLabelText("OpenAI API key");
    fireEvent.change(input, { target: { value: "sk-abc" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    // Composer appears in place (same instance — no reload); entry gone.
    expect(await screen.findByPlaceholderText(/Ask about your notes/)).toBeInTheDocument();
    expect(screen.queryByLabelText("OpenAI API key")).toBeNull();
  });

  it("keeps history readable while unactivated", async () => {
    mockTauri({
      chat_history: () => history([userMsg("earlier q"), assistantMsg("earlier a")]),
      chat_key_meta: () => UNCONFIGURED_KEY,
      chat_usage: () => null,
    });
    signInBillingWorkspace("member");
    renderPanel();
    // Past conversation renders even though chat isn't activated…
    expect(await screen.findByText("earlier a")).toBeInTheDocument();
    expect(screen.getByText("earlier q")).toBeInTheDocument();
    // …and the activation state shows instead of the composer.
    expect(screen.getByText(/ask Ada/)).toBeInTheDocument();
  });

  it("shows the composer (not the pane) when the workspace key is configured", async () => {
    mockTauri({
      chat_history: () => history(),
      chat_key_meta: () => CONFIGURED_KEY,
      chat_usage: () => null,
    });
    signInBillingWorkspace("owner");
    renderPanel();
    expect(await screen.findByPlaceholderText(/Ask about your notes/)).toBeInTheDocument();
    expect(screen.queryByLabelText("OpenAI API key")).toBeNull();
  });

  it("never shows the activation pane on a self-host server (billing disabled)", async () => {
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => history(),
      chat_key_meta: () => UNCONFIGURED_KEY,
      chat_usage: () => null,
    });
    signIntoWorkspace("Acme Team"); // billing_enabled false → self-host posture
    renderPanel();
    expect(await screen.findByPlaceholderText(/Ask about your notes/)).toBeInTheDocument();
    expect(screen.queryByText(/your OpenAI relationship/)).toBeNull();
  });

  it("lets a member without a personal key send in an activated workspace", async () => {
    let sent = false;
    mockTauri({
      provider_key_get: () => null, // no personal key → personal readiness is false
      chat_history: () => history(sent ? [userMsg("q"), assistantMsg("a")] : []),
      chat_key_meta: () => CONFIGURED_KEY, // workspace is activated
      chat_usage: () => null,
      chat_send: () => {
        sent = true;
        return { conversationId: "c1", truncated: false };
      },
    });
    signInBillingWorkspace("member");
    renderPanel();
    const input = await screen.findByPlaceholderText(/Ask about your notes/);
    fireEvent.change(input, { target: { value: "q" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    // The send must reach the backend — the workspace runs on the workspace key,
    // not the member's (absent) personal key.
    await waitFor(() => expect(sent).toBe(true));
  });

  it("lets a workspace member without a personal key reach the pane (no personal setup prompt)", async () => {
    mockTauri({
      provider_key_get: () => null,
      chat_history: () => history(),
      chat_key_meta: () => UNCONFIGURED_KEY,
      chat_usage: () => null,
    });
    signInBillingWorkspace("member");
    renderPanel();
    expect(await screen.findByText(/ask Ada/)).toBeInTheDocument();
    // NOT the personal "Add your OpenAI key" setup prompt.
    expect(screen.queryByText(/Add your OpenAI key/)).toBeNull();
  });
});

// A note whose reference block is comfortably over GROUNDING_CHAR_BUDGET, so the
// composer's pre-emptive truncation hint applies (#80).
function seedLongNote(noteId = "n1") {
  const note = { id: noteId, transcript: "x".repeat(30_000) } as unknown as Note;
  useNotesStore.setState({ notes: [note], folders: [] });
}

describe("ChatPanel composer chrome (#80)", () => {
  it("shows which model is about to answer", async () => {
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => history(),
      settings_get: (args) => {
        const key = (args as { key?: string }).key;
        if (key === "chat_model") return "gpt-5.1";
        if (key === "onboarding_completed") return "true";
        return null;
      },
    });
    renderPanel();
    await waitFor(() =>
      expect(screen.getByTestId("chat-model-indicator")).toHaveTextContent("gpt-5.1"),
    );
  });

  it("hides the model in a workspace, where the server picks it", async () => {
    // `model` is the LOCAL chat_model setting; a workspace turn runs on the
    // server's model, so naming the local one would be actively misleading.
    signIntoWorkspace();
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => history(),
      settings_get: (args) => {
        const key = (args as { key?: string }).key;
        if (key === "chat_model") return "gpt-5.1";
        if (key === "onboarding_completed") return "true";
        return null;
      },
    });
    renderPanel();
    await screen.findByPlaceholderText(/Ask about your notes/);
    expect(screen.queryByTestId("chat-model-indicator")).toBeNull();
  });

  it("warns that a long note may be trimmed BEFORE a turn is spent", async () => {
    seedLongNote();
    mockTauri({ provider_key_get: () => "sk-test", chat_history: () => history() });
    renderPanel();
    await waitFor(() =>
      expect(screen.getByTestId("chat-truncation-hint")).toBeInTheDocument(),
    );
  });

  it("does not warn for a note that fits", async () => {
    seedNoteWithFolder();
    mockTauri({ provider_key_get: () => "sk-test", chat_history: () => history() });
    renderPanel();
    await screen.findByPlaceholderText(/Ask about your notes/);
    expect(screen.queryByTestId("chat-truncation-hint")).toBeNull();
  });

  it("keeps the confirmed truncation banner alongside the pre-turn hint", async () => {
    // The hint is an estimate and a warning ("may omit"); the banner is the
    // confirmation it actually happened. Dropping either leaves the user unsure
    // which of the two they're looking at.
    seedLongNote();
    let sent = false;
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_send: () => {
        sent = true;
        return { conversationId: "c1", truncated: true };
      },
      chat_history: () => history(sent ? [userMsg("hi"), assistantMsg("hello")] : []),
    });
    renderPanel();
    const input = await screen.findByPlaceholderText(/Ask about your notes/);
    fireEvent.change(input, { target: { value: "hi" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    await waitFor(() => expect(screen.getByText("hello")).toBeInTheDocument());
    expect(screen.getByTestId("chat-truncation-hint")).toBeInTheDocument();
    expect(screen.getByText(/truncated to fit the context budget/)).toBeInTheDocument();
  });
});

describe("ChatPanel prompt picker (#80)", () => {
  async function openPicker() {
    mockTauri({ provider_key_get: () => "sk-test", chat_history: () => history() });
    renderPanel();
    const input = await screen.findByPlaceholderText(/Ask about your notes/);
    fireEvent.keyDown(input, { key: "/" });
    return input;
  }

  it("opens on '/' typed into an empty composer", async () => {
    await openPicker();
    expect(await screen.findByRole("listbox", { name: "Prompts" })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: /Key decisions/ })).toBeInTheDocument();
  });

  it("ignores '/' typed mid-sentence so 'and/or' still works", async () => {
    mockTauri({ provider_key_get: () => "sk-test", chat_history: () => history() });
    renderPanel();
    const input = await screen.findByPlaceholderText(/Ask about your notes/);
    fireEvent.change(input, { target: { value: "and" } });
    fireEvent.keyDown(input, { key: "/" });
    expect(screen.queryByRole("listbox", { name: "Prompts" })).toBeNull();
  });

  it("fills the composer with the picked prompt instead of sending it blind", async () => {
    let sends = 0;
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => history(),
      chat_send: () => {
        sends += 1;
        return { conversationId: "c1", truncated: false };
      },
    });
    renderPanel();
    const input = await screen.findByPlaceholderText(/Ask about your notes/);
    fireEvent.keyDown(input, { key: "/" });
    const option = await screen.findByRole("option", { name: /Action items/ });
    fireEvent.mouseDown(option);

    await waitFor(() =>
      expect((input as HTMLTextAreaElement).value).toMatch(/action items/i),
    );
    expect(sends).toBe(0);
    expect(screen.queryByRole("listbox", { name: "Prompts" })).toBeNull();
  });

  it("moves the selection with the arrow keys", async () => {
    await openPicker();
    const list = await screen.findByRole("listbox", { name: "Prompts" });
    const first = screen.getByRole("option", { name: /Key decisions/ });
    const second = screen.getByRole("option", { name: /Action items/ });
    expect(first).toHaveAttribute("aria-selected", "true");
    fireEvent.keyDown(list, { key: "ArrowDown" });
    expect(second).toHaveAttribute("aria-selected", "true");
    fireEvent.keyDown(list, { key: "ArrowUp" });
    expect(first).toHaveAttribute("aria-selected", "true");
  });
});

describe("ChatPanel stop (#80)", () => {
  // A send that stays in flight until released, so the turn is observably
  // streaming.
  function pendingSend() {
    let release: (() => void) | null = null;
    const cancels: unknown[] = [];
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => history(),
      chat_send: () =>
        new Promise((resolve) => {
          release = () => resolve({ conversationId: "c1", truncated: false });
        }),
      chat_cancel: (args) => {
        cancels.push(args);
        return null;
      },
    });
    return { cancels, release: () => release?.() };
  }

  it("swaps Send for Stop while a turn streams", async () => {
    const { release } = pendingSend();
    renderPanel();
    const input = await screen.findByPlaceholderText(/Ask about your notes/);
    fireEvent.change(input, { target: { value: "hi" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Stop generating" })).toBeInTheDocument(),
    );
    expect(screen.queryByRole("button", { name: "Send" })).toBeNull();
    await act(async () => release());
  });

  it("asks the backend to stop the turn for this note", async () => {
    const { cancels, release } = pendingSend();
    renderPanel();
    const input = await screen.findByPlaceholderText(/Ask about your notes/);
    fireEvent.change(input, { target: { value: "hi" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    const stop = await screen.findByRole("button", { name: "Stop generating" });
    fireEvent.click(stop);

    await waitFor(() => expect(cancels).toHaveLength(1));
    expect(cancels[0]).toEqual({ noteId: "n1" });
    await act(async () => release());
  });

  it("stops on Enter mid-turn instead of queueing a second one", async () => {
    const { cancels, release } = pendingSend();
    renderPanel();
    const input = await screen.findByPlaceholderText(/Ask about your notes/);
    fireEvent.change(input, { target: { value: "hi" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await screen.findByRole("button", { name: "Stop generating" });

    fireEvent.keyDown(screen.getByPlaceholderText(/press Enter to stop/), { key: "Enter" });
    await waitFor(() => expect(cancels).toHaveLength(1));
    await act(async () => release());
  });
});

// The conversation's pinned authorship filter (#103) — "Created by me" beside the
// breadth picker. Breadth says WHAT is in reach; this says WHOSE.
describe("ChatPanel authorship pin", () => {
  const PIN = { name: /Created by/ };

  // Signed into a workspace WITHOUT billing (so the BYOK activation pane doesn't
  // gate the composer), as a user with an id — the chip resolves the pin against
  // that id directly, not against a display name.
  function signInAs(id = "u-me", name = "Michael") {
    const ws = { id: "ws1", name: "Acme Team", role: "owner" as const, plan_status: "active" as const };
    useCloudStore.setState({
      status: {
        ...DISCONNECTED,
        configured: true,
        logged_in: true,
        base_url: "https://sync.humla.team",
        user: { id, name, email: `${id}@acme.com`, verified: true },
        current_workspace: ws,
        workspaces: [ws],
      },
      members: {
        [id]: { id, name, email: `${id}@acme.com`, role: "owner" },
        "u-anna": { id: "u-anna", name: "Anna", email: "anna@acme.com", role: "member" },
      },
    });
  }

  function renderGlobal() {
    return render(
      <MemoryRouter>
        <ChatPanel target={{ kind: "global" }} />
      </MemoryRouter>,
    );
  }

  it("is absent in Personal, where every note is already yours", async () => {
    mockTauri({ provider_key_get: () => "sk-test", chat_history: () => history(), chat_get_breadth: () => "all" });
    renderGlobal();
    await screen.findByPlaceholderText(/Ask about your notes/);
    expect(screen.queryByRole("button", PIN)).toBeNull();
  });

  // Presence tracks the SCOPE, not the note. Under `note` breadth the filter is
  // either a no-op (your own note) or empties the pane's own anchor (a
  // teammate's) — the same reason the date window is dropped there.
  it("is absent under note breadth, and appears once the scope widens", async () => {
    signInAs();
    seedNoteWithFolder();
    mockTauri({ provider_key_get: () => "sk-test", chat_history: () => history(), chat_get_breadth: () => "note" });
    renderPanel();

    await screen.findByPlaceholderText(/Ask about your notes/);
    expect(screen.queryByRole("button", PIN)).toBeNull();

    fireEvent.click(await screen.findByRole("button", { name: "Chat scope" }));
    fireEvent.click(await screen.findByText("All notes"));
    await waitFor(() => expect(screen.getByRole("button", PIN)).toBeInTheDocument());
  });

  // Pinned on a DRAFT (the library-wide default since #120): nothing is written
  // through, because there is no conversation to write to — the chip holds the pin
  // and the first turn carries it in. The #103 guarantee is unchanged, it just
  // binds one step later; `chat_send` carrying `draftOwnerFilter` is asserted in
  // the draft suite, and Rust covers the row being created with it.
  it("pins to the signed-in user's id, not to their name", async () => {
    signInAs("u-me");
    const sends: Record<string, unknown>[] = [];
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => history(),
      chat_get_breadth: () => "all",
      chat_send: (a) => {
        sends.push(a as Record<string, unknown>);
        return { conversationId: "c-fresh", truncated: false };
      },
    });
    renderGlobal();

    fireEvent.click(await screen.findByRole("button", PIN));
    await waitFor(() =>
      expect(screen.getByRole("button", PIN)).toHaveAttribute("aria-pressed", "true"),
    );

    const input = screen.getByPlaceholderText(/Ask about your notes/);
    fireEvent.change(input, { target: { value: "what did we agree?" } });
    fireEvent.keyDown(input, { key: "Enter" });

    await waitFor(() => expect(sends).toHaveLength(1));
    // The ID, never the display name — a name is prompt wording only.
    expect(sends[0].draftOwnerFilter).toBe("u-me");
  });

  // Clearing a pin on an OPEN conversation still writes through as null — an
  // existing row is the source of truth, so the clear has to reach it. (The
  // drafting case has no row and is covered above.) `history` returns a message so
  // the pane resolves a conversation, which is what makes this the write path.
  it("clears the pin by sending null, not an empty string", async () => {
    signInAs("u-me");
    const writes: { owner?: string | null }[] = [];
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => history([userMsg("earlier question")]),
      chat_get_breadth: () => "all",
      chat_get_owner_filter: () => "u-me",
      chat_set_owner_filter: (a) => {
        writes.push(a as { owner?: string | null });
        return null;
      },
    });
    renderGlobal();

    await waitFor(() => expect(screen.getByRole("button", PIN)).toHaveAttribute("aria-pressed", "true"));
    fireEvent.click(screen.getByRole("button", PIN));
    await waitFor(() => expect(writes).toHaveLength(1));
    expect(writes[0]!.owner).toBeNull();
  });

  // Anna's way out of a thread pinned to Michael is her own thread — the
  // alternative is her quietly rewriting what his scrollback means.
  it("names someone else's pin and refuses to let you change it", async () => {
    signInAs("u-me");
    const writes: unknown[] = [];
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => history(),
      chat_get_breadth: () => "all",
      chat_get_owner_filter: () => "u-anna",
      chat_set_owner_filter: (a) => {
        writes.push(a);
        return null;
      },
    });
    renderGlobal();

    const chip = await screen.findByRole("button", { name: /Created by Anna/ });
    expect(chip).toBeDisabled();
    fireEvent.click(chip);
    expect(writes).toHaveLength(0);
  });

  // The id filters; the name is only the model's disclosure wording. An
  // unresolvable person must not read as "me" — that would misattribute a
  // teammate's filter to whoever is looking at the thread.
  it("stays neutral about a pin it cannot resolve, and still shows it as on", async () => {
    signInAs("u-me");
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => history(),
      chat_get_breadth: () => "all",
      chat_get_owner_filter: () => "u-departed",
    });
    renderGlobal();

    const chip = await screen.findByRole("button", { name: /Created by someone else/ });
    expect(chip).toHaveAttribute("aria-pressed", "true");
    expect(chip).toBeDisabled();
  });

  it("sends the pinned person's name with the turn, for the model's disclosure", async () => {
    signInAs("u-me");
    const sends: { ownerName?: string | null }[] = [];
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => history(),
      chat_get_breadth: () => "all",
      chat_get_owner_filter: () => "u-anna",
      chat_send: (a) => {
        sends.push(a as { ownerName?: string | null });
        return { conversationId: "c1", truncated: false };
      },
    });
    renderGlobal();

    const input = await screen.findByPlaceholderText(/Ask about your notes/);
    fireEvent.change(input, { target: { value: "what did we decide?" } });
    fireEvent.keyDown(input, { key: "Enter" });
    await waitFor(() => expect(sends).toHaveLength(1));
    expect(sends[0]!.ownerName).toBe("Anna");
  });
});
