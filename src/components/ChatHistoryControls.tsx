// Compact chat session chrome (issue #62): a "+" that starts a fresh
// conversation, and a history button opening a popover of the target's
// conversations (title + relative date, most recent first, current one marked).
// Selecting one loads it.
//
// Extracted from Note.tsx in #95, where `/chat` needed the same chrome for the
// case its sidebar list can't cover — a collapsed sidebar. The alternative was a
// third rendering of the same list, which is what `conversationRows` exists to
// prevent.
//
// The history button hides for a lone empty conversation; the panel decides that
// via `canBrowseHistory`. All state lives in ChatPanel — this renders the
// projection it publishes and nothing else.

import { History, Plus } from "lucide-react";
import { SelectablePopover } from "./SelectablePopover";
import type { ChatSessionControls } from "./ChatPanel";
import { conversationRows } from "../lib/chatSessions";

export function ChatHistoryControls({
  controls,
  showHistory = true,
}: {
  controls: ChatSessionControls;
  /** Whether to offer the history popover alongside "+" (#95).
   *
   *  `/chat` sets this false while its sidebar list is visible: that list already
   *  IS the history, and a popover beside it would be the same thing rendered
   *  twice. The Note header has no list, so it keeps the default. */
  showHistory?: boolean;
}) {
  const { conversations, activeConversationId, canBrowseHistory, newChat, openConversation } =
    controls;
  // Same projection the `/chat` sidebar list renders from (#95) — ordering, the
  // empty-title fallback and the relative date are decided in one place. The
  // popover takes its own `activeId`, so it ignores each row's `active`.
  //
  // Deliberately NOT paged: this popover is the fallback view, and a popover is a
  // poor place to scroll for more. It shows the pages already loaded, which for a
  // note is everything and for `/chat` is at least the most recent 30.
  const items = conversationRows(conversations, activeConversationId);
  return (
    <div className="flex items-center gap-0.5">
      <button
        type="button"
        onClick={() => void newChat()}
        title="New chat"
        aria-label="New chat"
        className="nd-btn-icon"
      >
        <Plus size={16} strokeWidth={1.7} aria-hidden="true" />
      </button>
      {showHistory && canBrowseHistory && (
        <SelectablePopover
          ariaLabel="Chat history"
          align="end"
          items={items}
          activeId={activeConversationId}
          onSelect={(id) => {
            if (id) void openConversation(id);
          }}
          trigger={
            <span className="nd-btn-icon" title="Chat history">
              <History size={16} strokeWidth={1.7} aria-hidden="true" />
            </span>
          }
        />
      )}
    </div>
  );
}
