import { describe, it, expect, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor, act } from "@testing-library/react";
import { ChatKeyPanel } from "./ChatKeyPanel";
import { mockTauri } from "../../../test/tauri";
import { useCloudStore, DISCONNECTED, type ChatKeyMeta, type CloudRole } from "../../../lib/cloud";

const UNCONFIGURED: ChatKeyMeta = {
  configured: false,
  last4: null,
  setBy: null,
  setAt: null,
  keyHealth: null,
};
const configured = (over: Partial<ChatKeyMeta> = {}): ChatKeyMeta => ({
  configured: true,
  last4: "n3Kq",
  setBy: "u1",
  setAt: "2026-07-24T10:00:00Z",
  keyHealth: "ok",
  ...over,
});

function ws(role: CloudRole, id = "ws1") {
  return { id, name: "Acme", role, plan_status: "active" as const };
}

function renderPanel(role: CloudRole = "owner") {
  return render(<ChatKeyPanel ws={ws(role)} />);
}

beforeEach(() => {
  mockTauri();
  // Owner "Ada" in the roster so set-by / ask-owner names resolve.
  useCloudStore.setState({
    status: DISCONNECTED,
    members: { u1: { id: "u1", name: "Ada", email: "ada@acme.com", role: "owner" } },
  });
});

describe("ChatKeyPanel — owner (#75)", () => {
  it("shows the key entry when chat isn't activated", async () => {
    mockTauri({ chat_key_meta: () => UNCONFIGURED, chat_usage: () => null });
    renderPanel("owner");
    expect(await screen.findByLabelText("OpenAI API key")).toBeInTheDocument();
    expect(screen.getByText(/isn't activated/)).toBeInTheDocument();
  });

  it("saves a key, shows it verified, and clears the input", async () => {
    let savedKey: string | undefined;
    mockTauri({
      chat_key_meta: () => UNCONFIGURED,
      chat_usage: () => null,
      chat_key_set: (args) => {
        savedKey = (args as { apiKey?: string }).apiKey;
        return configured();
      },
    });
    renderPanel("owner");
    const input = await screen.findByLabelText("OpenAI API key");
    fireEvent.change(input, { target: { value: "sk-secret-123" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    expect(await screen.findByText(/Key ending n3Kq/)).toBeInTheDocument();
    expect(savedKey).toBe("sk-secret-123");
    expect(screen.getByText(/verified/)).toBeInTheDocument();
    // Configured state hides the entry — the key is gone from the UI.
    expect(screen.queryByLabelText("OpenAI API key")).toBeNull();
  });

  it("shows the mapped error on a rejected key and keeps the draft", async () => {
    mockTauri({
      chat_key_meta: () => UNCONFIGURED,
      chat_usage: () => null,
      chat_key_set: () => {
        throw "OpenAI rejected this key.";
      },
    });
    renderPanel("owner");
    const input = await screen.findByLabelText("OpenAI API key");
    fireEvent.change(input, { target: { value: "sk-bad" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    expect(await screen.findByText("OpenAI rejected this key.")).toBeInTheDocument();
    // Draft retained so the owner can correct it.
    expect(screen.getByLabelText("OpenAI API key")).toHaveValue("sk-bad");
  });

  it("removes the key via inline confirm", async () => {
    let removed = false;
    mockTauri({
      chat_key_meta: () => configured(),
      chat_usage: () => null,
      chat_key_delete: () => {
        removed = true;
        return UNCONFIGURED;
      },
    });
    renderPanel("owner");
    await screen.findByText(/Key ending n3Kq/);
    fireEvent.click(screen.getByRole("button", { name: "Remove" }));
    fireEvent.click(await screen.findByRole("button", { name: "Remove key" }));
    await waitFor(() => expect(removed).toBe(true));
    expect(await screen.findByText(/isn't activated/)).toBeInTheDocument();
  });

  it("rotates the key: reveals the entry, re-runs test-on-save, shows the new last4", async () => {
    let savedKey: string | undefined;
    mockTauri({
      chat_key_meta: () => configured({ last4: "old1" }),
      chat_usage: () => null,
      chat_key_set: (args) => {
        savedKey = (args as { apiKey?: string }).apiKey;
        return configured({ last4: "new9" });
      },
    });
    renderPanel("owner");
    await screen.findByText(/Key ending old1/);
    fireEvent.click(screen.getByRole("button", { name: "Rotate key" }));
    // Masked entry reappears.
    const input = await screen.findByLabelText("OpenAI API key");
    fireEvent.change(input, { target: { value: "sk-rotated" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    expect(await screen.findByText(/Key ending new9/)).toBeInTheDocument();
    expect(savedKey).toBe("sk-rotated");
    expect(screen.queryByLabelText("OpenAI API key")).toBeNull();
  });

  it("ignores a stale workspace's metadata after a fast switch", async () => {
    let releaseA: (() => void) | null = null;
    let phase: "A" | "B" = "A";
    mockTauri({
      chat_usage: () => null,
      chat_key_meta: () =>
        phase === "A"
          ? new Promise((resolve) => {
              releaseA = () => resolve(configured({ last4: "AAAA" }));
            })
          : configured({ last4: "BBBB" }),
    });
    const { rerender } = render(<ChatKeyPanel ws={ws("owner", "wsA")} />);
    // A's meta is still pending; switch to workspace B (fresh, immediate).
    phase = "B";
    rerender(<ChatKeyPanel ws={ws("owner", "wsB")} />);
    expect(await screen.findByText(/Key ending BBBB/)).toBeInTheDocument();

    // A's stale fetch resolves last — it must not clobber B's panel.
    await act(async () => {
      releaseA?.();
      await Promise.resolve();
    });
    expect(screen.getByText(/Key ending BBBB/)).toBeInTheDocument();
    expect(screen.queryByText(/Key ending AAAA/)).toBeNull();
  });

  it("surfaces the key-health degradation warning", async () => {
    mockTauri({
      chat_key_meta: () => configured({ keyHealth: "failing" }),
      chat_usage: () => null,
    });
    renderPanel("owner");
    expect(await screen.findByText(/semantic search degraded/)).toBeInTheDocument();
  });

  it("shows the managed add-on state with turn numbers", async () => {
    mockTauri({
      chat_key_meta: () => UNCONFIGURED,
      chat_usage: () => ({ used: 5, cap: 100, periodEnd: 0 }),
    });
    renderPanel("owner");
    expect(await screen.findByText(/Humla's managed key/)).toBeInTheDocument();
    expect(screen.getByText(/5\/100 turns this period/)).toBeInTheDocument();
  });
});

describe("ChatKeyPanel — member (#75)", () => {
  it("shows read-only configured state with no entry or actions", async () => {
    mockTauri({ chat_key_meta: () => configured(), chat_usage: () => null });
    renderPanel("member");
    expect(await screen.findByText(/Chat runs on Ada's workspace key/)).toBeInTheDocument();
    expect(screen.getByText(/Key ending n3Kq/)).toBeInTheDocument();
    expect(screen.queryByLabelText("OpenAI API key")).toBeNull();
    expect(screen.queryByRole("button", { name: "Rotate key" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Remove" })).toBeNull();
  });

  it("shows an ask-owner message when not activated, with no entry", async () => {
    mockTauri({ chat_key_meta: () => UNCONFIGURED, chat_usage: () => null });
    renderPanel("member");
    expect(await screen.findByText(/Ask Ada to turn it on/)).toBeInTheDocument();
    expect(screen.queryByLabelText("OpenAI API key")).toBeNull();
  });
});
