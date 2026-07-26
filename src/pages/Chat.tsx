// The `/chat` route (issue #95): chat's front door.
//
// Until now `ChatPanel` was mounted in exactly one place — the Chat tab of the
// Note right panel — so asking about your whole library meant opening some
// arbitrary note first and widening the breadth. This is the destination that
// makes a library-wide question a first-class thing to do.
//
// Everything substantive is borrowed: the panel arrives here parameterised by a
// chat target (#94), so activation, prompts, citations, truncation, streaming and
// a11y come for free, and the retrieval it drives is the same server- or
// local-side engine (#93). What this file owns is the page shell and the Recents
// rail.
//
// Deliberately absent: a greeting (we have no local user name — one exists only
// via `CloudUser.name` when signed into cloud, so it would be blank for the
// local-only majority) and a scope picker (its only option would be "All notes";
// narrowing stays a tool argument the model chooses, per #81).

import { useState } from "react";
import { Plus } from "lucide-react";
import { ChatPanel, type ChatSessionControls } from "../components/ChatPanel";
import { conversationRows } from "../lib/chatSessions";
import type { ChatTarget } from "../lib/chatTarget";
import { cn } from "../lib/cn";

// Module-level so the identity is stable across renders — the panel keys its
// load effect off the target's note id, but a stable object costs nothing and
// keeps the prop honest.
const GLOBAL_TARGET: ChatTarget = { kind: "global" };

// Flat list, no time-bucket headers: `relativeTime` already puts "just now" /
// "yesterday" / "4d ago" on every row, so buckets would restate it. Ten rows is
// enough that the list stays a glance rather than a filing cabinet — worth
// revisiting only once someone actually keeps 20+ conversations.
const RECENTS_LIMIT = 10;

export function Chat() {
  // The panel publishes its session projection upward (#62); here it feeds the
  // Recents rail instead of the Note header's popover.
  const [controls, setControls] = useState<ChatSessionControls | null>(null);

  // `canBrowseHistory` is the panel's lone-empty-conversation rule: a single
  // untouched conversation isn't history worth listing. Reused rather than
  // reimplemented, so the rail and the Note header agree on what counts.
  const rows = controls?.canBrowseHistory
    ? conversationRows(controls.conversations, controls.activeConversationId).slice(0, RECENTS_LIMIT)
    : [];

  return (
    <div className="h-full flex flex-col overflow-hidden">
      <div className="shrink-0 max-w-[1040px] mx-auto w-full px-8 pt-14">
        <h1 className="px-2 text-[25px] font-semibold tracking-[-0.022em] truncate">Chat</h1>
      </div>

      {/* The page opens on the composer: no display heading, no hero. The panel
          gets the room and the rail sits beside it. */}
      <div className="flex-1 min-h-0 max-w-[1040px] mx-auto w-full px-8 pt-3 pb-5 flex gap-5">
        {/* `min-h-0` as well as `min-w-0`: the panel scrolls its own message log,
            which only works while its height stays bounded by this column. */}
        <div className="flex-1 min-w-0 min-h-0 flex flex-col">
          <ChatPanel target={GLOBAL_TARGET} onControls={setControls} />
        </div>

        <aside
          aria-label="Recent conversations"
          className="w-[224px] shrink-0 flex flex-col border-l border-[var(--color-line)] pl-4"
        >
          <div className="shrink-0 flex items-center justify-between gap-2 pb-1">
            <span className="nd-label">Recent</span>
            <button
              type="button"
              onClick={() => void controls?.newChat()}
              disabled={!controls}
              title="New chat"
              aria-label="New chat"
              // `.nd-btn-icon` has no `:disabled` rule and its hover isn't
              // guarded, so a bare `disabled` would still light up on hover while
              // doing nothing. Same dimming the send button uses.
              className={cn("nd-btn-icon", !controls && "opacity-40 pointer-events-none")}
            >
              <Plus size={16} strokeWidth={1.7} aria-hidden="true" />
            </button>
          </div>

          <div className="flex-1 min-h-0 overflow-y-auto -mx-1 px-1">
            {rows.length === 0 ? (
              <p className="px-2 py-2 text-xs text-[var(--color-text-disabled)]">
                No conversations yet
              </p>
            ) : (
              // Read-only rows: selecting one loads it, and "new chat" is the only
              // other move. No rename or delete — in a workspace, who may remove a
              // conversation every member can see is still an open question (#60),
              // and shipping a delete that ignores it would answer it by accident.
              <ul className="flex flex-col gap-0.5 list-none">
                {rows.map((row) => (
                  <li key={row.id}>
                    <button
                      type="button"
                      onClick={() => void controls?.openConversation(row.id)}
                      aria-current={row.active ? "true" : undefined}
                      className={cn(
                        "w-full rounded-[var(--radius)] px-2 py-1.5 text-left transition-colors",
                        row.active
                          ? "bg-[var(--color-accent-soft)] text-[var(--color-accent-text)]"
                          : "hover:bg-[var(--color-pill-hover)]",
                      )}
                    >
                      <span className="block truncate text-[13px]">{row.label}</span>
                      <span className="block truncate text-xs text-[var(--color-text-muted)] tabular-nums">
                        {row.description}
                      </span>
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </div>
        </aside>
      </div>
    </div>
  );
}
