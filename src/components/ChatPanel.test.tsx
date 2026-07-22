import { describe, it, expect, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { ChatPanel } from "./ChatPanel";
import { mockTauri } from "../test/tauri";
import type { ChatMessageDto } from "../lib/ipc";

function userMsg(text: string): ChatMessageDto {
  return { id: "u1", role: "user", seq: 0, parts: [{ type: "text", id: "b0", text }], createdAt: 1 };
}
function assistantMsg(text: string): ChatMessageDto {
  return { id: "a1", role: "assistant", seq: 1, parts: [{ type: "text", id: "b1", text }], createdAt: 2 };
}

beforeEach(() => {
  mockTauri();
});

describe("ChatPanel readiness", () => {
  it("shows the setup prompt when OpenAI has no key", async () => {
    // Default provider is OpenAI; default provider_key_get returns null.
    render(<ChatPanel noteId="n1" />);
    await waitFor(() =>
      expect(screen.getByText(/Add your OpenAI key/)).toBeInTheDocument(),
    );
    // No input while unconfigured.
    expect(screen.queryByPlaceholderText(/Ask about this note/)).toBeNull();
  });

  it("shows the input + empty state once a key is present", async () => {
    mockTauri({ provider_key_get: () => "sk-test", chat_history: () => [] });
    render(<ChatPanel noteId="n1" />);
    await waitFor(() =>
      expect(screen.getByPlaceholderText(/Ask about this note/)).toBeInTheDocument(),
    );
    expect(screen.getByText(/Ask anything about this note/)).toBeInTheDocument();
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
      // Empty until the send lands, then the persisted turn pair.
      chat_history: () => (sent ? [userMsg("What happened?"), assistantMsg("A summary.")] : []),
    });
    render(<ChatPanel noteId="n1" />);

    const input = await screen.findByPlaceholderText(/Ask about this note/);
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
    render(<ChatPanel noteId="n1" />);
    const input = await screen.findByPlaceholderText(/Ask about this note/);
    fireEvent.change(input, { target: { value: "hi" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await waitFor(() =>
      expect(screen.getByText(/truncated to fit the context budget/)).toBeInTheDocument(),
    );
  });
});
