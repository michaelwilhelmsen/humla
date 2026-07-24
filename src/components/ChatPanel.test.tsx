import { describe, it, expect, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor, act } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { ChatPanel, type ChatSessionControls } from "./ChatPanel";
import { mockTauri } from "../test/tauri";
import type { ChatMessageDto, Folder, Note } from "../lib/ipc";
import { useCloudStore, DISCONNECTED } from "../lib/cloud";
import { useNotesStore } from "../lib/store";

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
      <ChatPanel noteId={noteId} />
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
        noteId={noteId}
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
