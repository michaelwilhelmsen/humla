import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { mockTauri } from "../test/tauri";
import { useCloudStore, DISCONNECTED } from "../lib/cloud";
import { useNotesStore } from "../lib/store";

// Readiness is mocked here so the test can flip `ready` at will — the Ollama
// probe drives it in production, polling every 2s (#64). Isolated in its own
// file so the mock doesn't leak into ChatPanel.test.tsx (which exercises the
// real readiness hook).
const readyState = vi.hoisted(() => ({ value: true }));
vi.mock("./provider/useChatReadiness", () => ({
  useChatReadiness: () => ({
    loading: false,
    ready: readyState.value,
    hint: "",
    provider: "openai",
    model: "",
  }),
}));

import { ChatPanel } from "./ChatPanel";

function panel() {
  return (
    <MemoryRouter>
      <ChatPanel target={{ kind: "note", noteId: "n1" }} />
    </MemoryRouter>
  );
}

beforeEach(() => {
  mockTauri({ chat_history: () => ({ conversationId: null, messages: [] }) });
  useCloudStore.setState({ status: DISCONNECTED });
  useNotesStore.setState({ notes: [], folders: [] });
  readyState.value = true;
});

describe("ChatPanel composer auto-focus (#64)", () => {
  it("auto-focuses once on open and does NOT re-focus when readiness re-flips", async () => {
    const { rerender } = render(panel());
    const input = await screen.findByRole("textbox", { name: /ask about your notes/i });
    await waitFor(() => expect(document.activeElement).toBe(input));

    // The user moves focus away (e.g. clicks into the always-mounted note body).
    (document.activeElement as HTMLElement).blur();
    const away = document.createElement("button");
    document.body.appendChild(away);
    away.focus();
    expect(document.activeElement).toBe(away);

    // Readiness drops then recovers (a transient Ollama probe failure). The
    // composer must NOT snatch focus back mid-typing.
    readyState.value = false;
    rerender(panel());
    readyState.value = true;
    rerender(panel());

    expect(document.activeElement).toBe(away);
    away.remove();
  });
});
