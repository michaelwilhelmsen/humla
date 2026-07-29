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
//
// Rows carry a right-click menu with Rename and Delete (issue #109), mirroring
// `FolderRow` in `Sidebar.tsx` — the app's one existing context-menu idiom. A
// hover trash icon (what the sidebar's NOTE rows use) was the alternative and
// doesn't fit here: rename needs somewhere to live, and a menu holds both verbs
// without parking a destructive button under the pointer on every hover.

import { useEffect, useRef, useState } from "react";
import { useGlobalChatStore } from "../lib/globalChat";
import { conversationRows, type ConversationRow } from "../lib/chatSessions";
import { ContextMenu, ContextMenuItem } from "./ContextMenu";
import { Modal } from "../pages/settings/components/Modal";
import { cn } from "../lib/cn";

export function ChatConversations() {
  const controls = useGlobalChatStore((s) => s.controls);
  const sentinelRef = useRef<HTMLDivElement | null>(null);
  // Read through a ref so the observer below can be set up once, rather than torn
  // down and rebuilt every time a page lands (which is itself a state change).
  const loadMoreRef = useRef<(() => Promise<void>) | null>(null);
  loadMoreRef.current = controls?.loadMore ?? null;

  // Which row is mid-rename, and which is awaiting a delete confirm. Both live
  // here rather than inside the row so only one can be open at a time — two
  // inline editors, or two stacked confirms, is never what the user meant.
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [pendingDelete, setPendingDelete] = useState<ConversationRow | null>(null);
  const [deleting, setDeleting] = useState(false);

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

  // `renamingId` needs no cleanup when a row disappears: it only ever selects a
  // row that IS rendered, so a stale id shows no editor and is overwritten by the
  // next rename. Nothing to reconcile.

  async function confirmDelete() {
    if (!pendingDelete || !controls) return;
    setDeleting(true);
    try {
      await controls.deleteConversation(pendingDelete.id);
    } finally {
      setDeleting(false);
      setPendingDelete(null);
    }
  }

  return (
    <>
      {/* Label only. "New chat" lives in the app bar with the other actions, the
          way the Note view puts its actions there — the sidebar is the list. */}
      <div className="nd-label px-2 pb-1.5">Conversations</div>

      {controls && rows.length === 0 && (
        <div className="px-2 py-3 text-xs text-[var(--color-text-disabled)]">
          No conversations yet
        </div>
      )}

      {rows.length > 0 && (
        <ul className="list-none">
          {rows.map((row) => (
            <li key={row.id}>
              <ConversationListRow
                row={row}
                renaming={renamingId === row.id}
                onOpen={() => void controls?.openConversation(row.id)}
                onStartRename={() => setRenamingId(row.id)}
                onEndRename={() => setRenamingId(null)}
                onCommitRename={(title) => void controls?.renameConversation(row.id, title)}
                onRequestDelete={() => setPendingDelete(row)}
              />
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

      {/* Deleting a conversation is unrecoverable — there is no Trash for chat the
          way there is for notes — so it confirms first, and the copy names the
          thread and says plainly that it can't be undone. `window.confirm` is
          blocked in the Tauri webview, hence the Modal. */}
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
    </>
  );
}

/** One conversation row: a button that opens it, a right-click menu carrying
 *  Rename and Delete, and an inline editor while renaming.
 *
 *  Deliberately the same shape as `FolderRow`: `onContextMenu` opens a
 *  `ContextMenu` at the pointer, and rename swaps the row for an input that
 *  commits on Enter or blur and abandons on Escape. Matching it means both lists
 *  in this sidebar behave identically under the same gesture. */
function ConversationListRow({
  row,
  renaming,
  onOpen,
  onStartRename,
  onEndRename,
  onCommitRename,
  onRequestDelete,
}: {
  row: ConversationRow;
  renaming: boolean;
  onOpen: () => void;
  onStartRename: () => void;
  onEndRename: () => void;
  onCommitRename: (title: string) => void;
  onRequestDelete: () => void;
}) {
  const [menuPos, setMenuPos] = useState<{ x: number; y: number } | null>(null);
  const [draft, setDraft] = useState(row.label);

  function openMenu(e: React.MouseEvent) {
    e.preventDefault();
    e.stopPropagation();
    setMenuPos({ x: e.clientX, y: e.clientY });
  }

  function startRename() {
    setMenuPos(null);
    // Seed from the row's displayed label so the user edits what they can see —
    // including the "New chat" fallback an untitled thread renders.
    setDraft(row.label);
    onStartRename();
  }

  function commitRename() {
    const next = draft.trim();
    // An empty or unchanged name is an abandon, not a write: the backend rejects
    // an empty title (that's how "never titled" is spelled on both sides), so
    // sending one would surface an error for what is plainly a cancel.
    if (next && next !== row.label) onCommitRename(next);
    onEndRename();
  }

  if (renaming) {
    return (
      <div className="no-drag flex items-center px-2.5 py-1.5 rounded-[var(--radius)] border border-[var(--color-text-muted)] bg-[var(--color-surface)]">
        <input
          autoFocus
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") commitRename();
            else if (e.key === "Escape") onEndRename();
          }}
          onBlur={commitRename}
          aria-label="Conversation name"
          className="flex-1 text-[13.5px] min-w-0 bg-transparent"
        />
      </div>
    );
  }

  return (
    <>
      <button
        type="button"
        onClick={onOpen}
        onContextMenu={openMenu}
        aria-current={row.active ? "true" : undefined}
        title={row.label}
        className={cn(
          // `select-none`: a right-click in a webview places a selection before the
          // menu opens, so the row's title was left highlighted behind the menu.
          // The row is a control, not prose — there is nothing here worth selecting.
          "no-drag select-none w-full flex flex-col gap-0.5 px-2.5 py-1.5 rounded-[var(--radius)] text-left transition-colors",
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
      {menuPos && (
        <ContextMenu x={menuPos.x} y={menuPos.y} onClose={() => setMenuPos(null)}>
          <ContextMenuItem onClick={startRename}>Rename</ContextMenuItem>
          <ContextMenuItem
            onClick={() => {
              setMenuPos(null);
              onRequestDelete();
            }}
            danger
          >
            Delete
          </ContextMenuItem>
        </ContextMenu>
      )}
    </>
  );
}
