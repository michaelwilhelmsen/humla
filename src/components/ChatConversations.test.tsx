import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { ChatConversations } from "./ChatConversations";
import { useGlobalChatStore } from "../lib/globalChat";
import type { ChatSessionControls } from "./ChatPanel";
import type { ConversationMeta } from "../lib/ipc";

// The `/chat` conversation list, as rendered in the sidebar (issue #95). It reads
// only what ChatPanel publishes, so these tests seed the projection directly —
// no IPC, no panel, no route.

const HOUR = 3_600_000;

function conversation(over: Partial<ConversationMeta> & { id: string }): ConversationMeta {
  return { title: "Untitled", breadth: "all", updatedAt: 1, messageCount: 2, ...over };
}

function publish(over: Partial<ChatSessionControls> = {}) {
  const controls: ChatSessionControls = {
    targetKey: "global",
    conversations: [],
    activeConversationId: null,
    canBrowseHistory: true,
    hasMore: false,
    loadingMore: false,
    newChat: vi.fn(async () => {}),
    openConversation: vi.fn(async () => {}),
    loadMore: vi.fn(async () => {}),
    status: null,
    ...over,
  };
  useGlobalChatStore.setState({ controls });
  return controls;
}

// jsdom has no IntersectionObserver. Stub it and keep the callback so a test can
// fire the intersection itself rather than faking a scroll.
let fireIntersection: (() => void) | null = null;
function stubIntersectionObserver() {
  class Stub {
    constructor(private cb: IntersectionObserverCallback) {
      fireIntersection = () =>
        this.cb([{ isIntersecting: true } as IntersectionObserverEntry], this as never);
    }
    observe() {}
    disconnect() {
      fireIntersection = null;
    }
    unobserve() {}
    takeRecords() {
      return [];
    }
    root = null;
    rootMargin = "";
    thresholds = [];
  }
  vi.stubGlobal("IntersectionObserver", Stub);
}

beforeEach(() => {
  useGlobalChatStore.setState({ controls: null });
  fireIntersection = null;
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("ChatConversations", () => {
  it("renders nothing but the header until a projection arrives", () => {
    render(<ChatConversations />);

    expect(screen.getByText("Conversations")).toBeInTheDocument();
    // "No conversations yet" would be a claim we can't make yet — the pane may
    // still be mounting, or chat may not be configured.
    expect(screen.queryByText("No conversations yet")).toBeNull();
  });

  it("offers no actions of its own — the app bar owns those", () => {
    // The section is the list. "New chat" lives in the title-bar row with the
    // other actions, so it isn't rendered twice on one screen.
    publish({ conversations: [conversation({ id: "c-a", title: "Alpha" })] });
    render(<ChatConversations />);

    expect(screen.queryByLabelText("New chat")).toBeNull();
  });

  // The clock is frozen for this one: "50 hours ago" is 2 calendar days back or 3
  // depending on the time of day it runs, so the row's relative label flipped
  // between "2d ago" and "3d ago" and the suite failed after ~22:00 local. A
  // relative-time assertion has to pin the instant it is relative TO.
  it("lists conversations most-recent first and marks the active one", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-07-26T12:00:00Z"));
    const now = Date.now();
    publish({
      activeConversationId: "c-mid",
      conversations: [
        conversation({ id: "c-old", title: "Budget questions", updatedAt: now - 50 * HOUR }),
        conversation({ id: "c-new", title: "This week", updatedAt: now - 1 * HOUR }),
        conversation({ id: "c-mid", title: "Client status", updatedAt: now - 5 * HOUR }),
      ],
    });
    render(<ChatConversations />);

    const rows = screen.getAllByRole("listitem");
    expect(rows.map((r) => r.textContent)).toEqual([
      "This week1h ago",
      "Client status5h ago",
      "Budget questions2d ago",
    ]);
    const current = screen.getAllByRole("button", { current: true });
    expect(current).toHaveLength(1);
    expect(current[0].textContent).toContain("Client status");
    vi.useRealTimers();
  });

  it("does not cap the list", () => {
    // A workspace could accumulate hundreds; the list is uncapped and pages in
    // instead. 40 rows would have been truncated to 10 by the earlier design.
    const now = Date.now();
    publish({
      conversations: Array.from({ length: 40 }, (_, i) =>
        conversation({ id: `c${i}`, title: `Chat ${i}`, updatedAt: now - i * HOUR }),
      ),
    });
    render(<ChatConversations />);

    expect(screen.getAllByRole("listitem")).toHaveLength(40);
  });

  it("says so when there is no history worth browsing", () => {
    // The lone-empty-conversation rule (#62), reused rather than reimplemented.
    publish({ canBrowseHistory: false, conversations: [conversation({ id: "c1", title: "" })] });
    render(<ChatConversations />);

    expect(screen.getByText("No conversations yet")).toBeInTheDocument();
    expect(screen.queryByRole("listitem")).toBeNull();
  });

  it("loads the conversation whose row is clicked", () => {
    const controls = publish({
      conversations: [conversation({ id: "c-a", title: "Alpha", updatedAt: 2 })],
    });
    render(<ChatConversations />);

    fireEvent.click(screen.getByText("Alpha"));
    expect(controls.openConversation).toHaveBeenCalledWith("c-a");
  });

  describe("paging", () => {
    it("asks for the next page when the end of the list comes into view", async () => {
      stubIntersectionObserver();
      const controls = publish({
        hasMore: true,
        conversations: [conversation({ id: "c1", title: "One" })],
      });
      render(<ChatConversations />);

      expect(fireIntersection).not.toBeNull();
      fireIntersection!();
      await waitFor(() => expect(controls.loadMore).toHaveBeenCalled());
    });

    it("stops watching once the list is exhausted", () => {
      stubIntersectionObserver();
      publish({ hasMore: false, conversations: [conversation({ id: "c1", title: "One" })] });
      render(<ChatConversations />);

      // No sentinel, so no observer: nothing left to ask for.
      expect(fireIntersection).toBeNull();
      expect(screen.queryByText("Loading…")).toBeNull();
    });

    it("says a page is on the way while one is in flight", () => {
      stubIntersectionObserver();
      publish({ hasMore: true, loadingMore: true, conversations: [] });
      render(<ChatConversations />);

      expect(screen.getByText("Loading…")).toBeInTheDocument();
    });

    it("renders without an IntersectionObserver at all", () => {
      // Absent in jsdom and in old webviews. The list keeps the pages it has
      // rather than throwing; the popover fallback still reaches them.
      vi.stubGlobal("IntersectionObserver", undefined);
      publish({ hasMore: true, conversations: [conversation({ id: "c1", title: "One" })] });

      expect(() => render(<ChatConversations />)).not.toThrow();
      expect(screen.getByText("One")).toBeInTheDocument();
    });
  });
});
