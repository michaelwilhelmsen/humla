// The conversation list for `/chat`, rendered in the SIDEBAR (issue #95).
//
// It lives in the left nav rather than a right-hand rail because it is
// navigation — a list of things you switch between, the same role the note list
// plays for the notes routes. The right panel in this app means "context about
// the thing on screen" (this note's summary, this note's transcript), which a
// conversation list is not.
//
// It replaces the Folders section rather than displacing the nav items, so the
// route swaps a *section* and not the whole sidebar: the "Chat" item you clicked
// stays where it is and no back affordance is needed.
//
// No cap — a workspace could accumulate hundreds — so pages arrive as the list
// scrolls. `ChatPanel` owns the list and the paging; this renders what it
// publishes and asks for more when the bottom comes into view.

import { useEffect, useRef } from "react";
import { Plus } from "lucide-react";
import { useGlobalChatStore } from "../lib/globalChat";
import { conversationRows } from "../lib/chatSessions";
import { cn } from "../lib/cn";

export function ChatConversations() {
  const controls = useGlobalChatStore((s) => s.controls);
  const sentinelRef = useRef<HTMLDivElement | null>(null);
  // Read through a ref so the observer below can be set up once, rather than torn
  // down and rebuilt every time a page lands (which is itself a state change).
  const loadMoreRef = useRef<(() => Promise<void>) | null>(null);
  loadMoreRef.current = controls?.loadMore ?? null;

  const hasMore = controls?.hasMore ?? false;

  useEffect(() => {
    const el = sentinelRef.current;
    // `IntersectionObserver` is absent in jsdom and in very old webviews. Nothing
    // to fall back to and nothing to break: the list keeps the pages it has, and
    // the popover fallback still reaches them.
    if (!el || !hasMore || typeof IntersectionObserver === "undefined") return;
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) void loadMoreRef.current?.();
      },
      // A little early, so the next page is usually already in by the time the
      // user reaches the end.
      { rootMargin: "120px" },
    );
    observer.observe(el);
    return () => observer.disconnect();
  }, [hasMore]);

  // Nothing published yet (the pane is still mounting, or chat isn't configured).
  // Render the header only: "No conversations yet" would be a claim we can't make.
  const rows = controls?.canBrowseHistory
    ? conversationRows(controls.conversations, controls.activeConversationId)
    : [];

  return (
    <>
      <div className="nd-label flex items-center justify-between px-2 pb-1.5">
        <span>Conversations</span>
        <button
          type="button"
          onClick={() => void controls?.newChat()}
          disabled={!controls}
          aria-label="New chat"
          title="New chat"
          className={cn(
            "grid place-items-center w-5 h-5 rounded-full text-[var(--color-text-muted)] hover:text-[var(--color-text)] hover:bg-[var(--color-pill-hover)] transition-colors",
            !controls && "opacity-40 pointer-events-none",
          )}
        >
          <Plus size={14} strokeWidth={2} />
        </button>
      </div>

      {controls && rows.length === 0 && (
        <div className="px-2 py-3 text-xs text-[var(--color-text-disabled)]">
          No conversations yet
        </div>
      )}

      {rows.length > 0 && (
        // Read-only rows: selecting one loads it, and "+" is the only other move.
        // No rename or delete — in a workspace, who may remove a conversation
        // every member can see is still an open question (#60).
        <ul className="list-none">
          {rows.map((row) => (
            <li key={row.id}>
              <button
                type="button"
                onClick={() => void controls?.openConversation(row.id)}
                aria-current={row.active ? "true" : undefined}
                title={row.label}
                className={cn(
                  "no-drag w-full flex flex-col gap-0.5 px-2.5 py-1.5 rounded-[var(--radius)] text-left transition-colors",
                  row.active
                    ? "bg-[var(--color-sidebar-active)] text-[var(--color-text)] font-medium shadow-[0_1px_2px_rgba(0,0,0,0.05)]"
                    : "text-[var(--color-text-muted)] hover:bg-[var(--color-pill-hover)] hover:text-[var(--color-text)]",
                )}
              >
                <span className="w-full truncate text-[13.5px]">{row.label}</span>
                <span className="w-full truncate text-[11px] text-[var(--color-text-disabled)] tabular-nums">
                  {row.description}
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}

      {/* Paging tail: the sentinel the observer watches, plus an honest line while
          a page is in flight. Both only exist while there might be more. */}
      {hasMore && (
        <div ref={sentinelRef} className="px-2 py-2 text-xs text-[var(--color-text-disabled)]">
          {controls?.loadingMore ? "Loading…" : ""}
        </div>
      )}
    </>
  );
}
