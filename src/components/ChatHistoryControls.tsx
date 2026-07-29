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
//
// Rename and delete ride here too (issue #109), and this popover is not a
// nice-to-have copy of the sidebar's: on a Note route the sidebar shows folders
// and notes, so this is the ONLY surface a note-scoped conversation can be
// reached from. Without these, every thread anchored to a note would stay
// undeletable.
//
// `SelectablePopover` already carries per-row rename/delete affordances (built
// for the Client picker), so rename wires straight through. Delete does NOT:
// the popover fires `onDelete` immediately, which is acceptable for a Client and
// not for a thread whose messages can't be recovered. So delete opens the same
// confirm the sidebar uses, and only then calls through.

import { useState } from "react";
import { History, Plus } from "lucide-react";
import { SelectablePopover } from "./SelectablePopover";
import { Modal } from "../pages/settings/components/Modal";
import type { ChatSessionControls } from "./ChatPanel";
import { conversationRows, type ConversationRow } from "../lib/chatSessions";

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
  const {
    conversations,
    activeConversationId,
    canBrowseHistory,
    newChat,
    openConversation,
    deleteConversation,
    renameConversation,
  } = controls;
  const [pendingDelete, setPendingDelete] = useState<ConversationRow | null>(null);
  const [deleting, setDeleting] = useState(false);
  // Same projection the `/chat` sidebar list renders from (#95) — ordering, the
  // empty-title fallback and the relative date are decided in one place. The
  // popover takes its own `activeId`, so it ignores each row's `active`.
  //
  // Deliberately NOT paged: this popover is the fallback view, and a popover is a
  // poor place to scroll for more. It shows the pages already loaded, which for a
  // note is everything and for `/chat` is at least the most recent 30.
  const items = conversationRows(conversations, activeConversationId);

  async function confirmDelete() {
    if (!pendingDelete) return;
    setDeleting(true);
    try {
      await deleteConversation(pendingDelete.id);
    } finally {
      setDeleting(false);
      setPendingDelete(null);
    }
  }

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
          onRename={(id, name) => renameConversation(id, name)}
          onDelete={(id) => {
            // Gate, don't destroy: hand the row to the confirm below rather than
            // deleting on the click, which is what the popover would do by default.
            const row = items.find((i) => i.id === id);
            if (row) setPendingDelete(row);
          }}
          trigger={
            <span className="nd-btn-icon" title="Chat history">
              <History size={16} strokeWidth={1.7} aria-hidden="true" />
            </span>
          }
        />
      )}

      {/* Same copy and same stakes as the sidebar's confirm — there is no Trash for
          chat, so the dialog is the only thing between a click and losing a thread. */}
      <Modal
        open={pendingDelete !== null}
        onClose={() => {
          if (!deleting) setPendingDelete(null);
        }}
        title="Delete conversation"
      >
        <p className="text-[14px] text-[var(--color-text)]">
          Delete “{pendingDelete?.label}”? Its messages go with it, and this can’t be undone.
        </p>
        <div className="flex justify-end gap-2 mt-5">
          <button className="nd-btn" onClick={() => setPendingDelete(null)} disabled={deleting}>
            Cancel
          </button>
          <button
            className="nd-btn"
            style={{ color: "var(--color-danger)" }}
            onClick={() => void confirmDelete()}
            disabled={deleting}
          >
            Delete
          </button>
        </div>
      </Modal>
    </div>
  );
}
