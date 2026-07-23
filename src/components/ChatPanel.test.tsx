import { describe, it, expect, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { ChatPanel } from "./ChatPanel";
import { mockTauri } from "../test/tauri";
import type { ChatMessageDto } from "../lib/ipc";
import { useCloudStore, DISCONNECTED } from "../lib/cloud";

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

function renderPanel(noteId = "n1") {
  return render(
    <MemoryRouter>
      <ChatPanel noteId={noteId} />
    </MemoryRouter>,
  );
}

beforeEach(() => {
  mockTauri();
  // Default: signed out, so Personal is the only tenant unless a test opts in.
  useCloudStore.setState({ status: DISCONNECTED });
});

describe("ChatPanel readiness", () => {
  it("shows the setup prompt when OpenAI has no key", async () => {
    renderPanel();
    await waitFor(() => expect(screen.getByText(/Add your OpenAI key/)).toBeInTheDocument());
    expect(screen.queryByPlaceholderText(/Ask about your notes/)).toBeNull();
  });

  it("shows the input + empty state once a key is present", async () => {
    mockTauri({ provider_key_get: () => "sk-test", chat_history: () => [] });
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
      chat_history: () => (sent ? [userMsg("What happened?"), assistantMsg("A summary.")] : []),
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
      chat_history: () => (sent ? [userMsg("hi"), assistantMsg("hello")] : []),
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
      chat_history: () => [userMsg("what happened?"), assistantWithCitation("Here's what I found.")],
    });
    renderPanel();
    await waitFor(() => expect(screen.getByText("Here's what I found.")).toBeInTheDocument());
    // The cited note surfaces as a clickable chip by title.
    expect(screen.getByRole("button", { name: /Kickoff notes/ })).toBeInTheDocument();
  });

  it("offers the scope breadths in the Scope popover", async () => {
    mockTauri({ provider_key_get: () => "sk-test", chat_history: () => [] });
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
    mockTauri({ provider_key_get: () => "sk-test", chat_history: () => [] });
    signIntoWorkspace("Acme Team");
    renderPanel();
    await screen.findByPlaceholderText(/Ask about your notes/);
    // The old Personal/Workspace selector is gone entirely.
    expect(screen.queryByRole("button", { name: "Chat tenant" })).toBeNull();
  });

  it("shows the workspace name as a non-interactive context indicator", async () => {
    mockTauri({ provider_key_get: () => "sk-test", chat_history: () => [] });
    signIntoWorkspace("Acme Team");
    renderPanel();
    // The indicator shows where chat goes; it is not a button (no popover).
    const indicator = await screen.findByLabelText("Chat context");
    expect(indicator).toHaveTextContent("Acme Team");
    expect(indicator.tagName).not.toBe("BUTTON");
  });

  it("shows Personal as the context indicator when signed out", async () => {
    mockTauri({ provider_key_get: () => "sk-test", chat_history: () => [] });
    renderPanel();
    const indicator = await screen.findByLabelText("Chat context");
    expect(indicator).toHaveTextContent("Personal");
  });
});

describe("ChatPanel scope breadth (#58)", () => {
  it("initialises the scope chip from the persisted breadth", async () => {
    mockTauri({
      provider_key_get: () => "sk-test",
      chat_history: () => [],
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
      chat_history: () => [],
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
