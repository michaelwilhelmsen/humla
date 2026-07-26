// The `/chat` route (issue #95): chat's front door.
//
// Until now `ChatPanel` was mounted in exactly one place — the Chat tab of the
// Note right panel — so asking about your whole library meant opening an
// arbitrary note first and widening the breadth. This is the destination that
// makes a library-wide question a first-class thing to do.
//
// Everything substantive is borrowed: the panel arrives here parameterised by a
// chat target (#94), so activation, prompts, citations, truncation, streaming and
// a11y come for free, and the retrieval it drives is the same server- or
// local-side engine (#93). What this file owns is the page shell — the
// conversation list lives in the SIDEBAR (`ChatConversations`), because a list you
// navigate between belongs in the nav column, not in the right-hand slot this app
// uses for context about the thing on screen.
//
// Deliberately absent: a greeting. We have no local user name — one exists only
// via `CloudUser.name` when signed into cloud, so it would be blank for the
// local-only majority, and a nameless display heading does no work. The prompt
// cards on a new chat do the job a greeting pretends to.
//
// Also absent: a scope picker (its only option would be "All notes"; narrowing
// stays a tool argument the model chooses, per #81).

import { useEffect } from "react";
import { useOutletContext } from "react-router-dom";
import { Files, Users } from "lucide-react";
import { ChatPanel } from "../components/ChatPanel";
import { ChatHistoryControls } from "../components/ChatHistoryControls";
import type { LayoutOutletContext } from "../components/Layout";
import { useGlobalChatStore } from "../lib/globalChat";
import { useCloudStore } from "../lib/cloud";
import { useNotesStore } from "../lib/store";
import type { ChatTarget } from "../lib/chatTarget";

// Module-level so the identity is stable across renders — the panel keys its
// load effect off the target's note id, but a stable object costs nothing and
// keeps the prop honest.
const GLOBAL_TARGET: ChatTarget = { kind: "global" };

export function Chat() {
  const { sidebarCollapsed } = useOutletContext<LayoutOutletContext>();
  const controls = useGlobalChatStore((s) => s.controls);
  const setControls = useGlobalChatStore((s) => s.setControls);
  const workspaceName = useCloudStore((s) => s.status.current_workspace?.name ?? null);
  const noteCount = useNotesStore((s) => s.notes.length);

  // Clear on the way out, or the sidebar would keep rendering a list belonging to
  // a pane that no longer exists. (`ChatPanel` publishes `null` when chat isn't
  // usable, but unmounting isn't one of those moments — it can't publish then.)
  useEffect(() => () => setControls(null), [setControls]);

  return (
    <div className="h-full flex flex-col overflow-hidden">
      <div className="shrink-0 max-w-[820px] mx-auto w-full px-8 pt-14">
        <div className="flex items-center gap-3">
          <h1 className="text-[25px] font-semibold tracking-[-0.022em] truncate">Chat</h1>
          {/* The session chrome lives in exactly one place at a time. The sidebar
              owns it while it's open; collapsed (manually, or automatically under
              900px) it would take the conversation list with it, so the popover
              fallback appears here instead. Never both — two "new chat" buttons
              on one screen is a puzzle, not an affordance. */}
          {sidebarCollapsed && controls && (
            <div className="ml-auto">
              <ChatHistoryControls controls={controls} />
            </div>
          )}
        </div>

        {/* Meta bar, mirroring the Note view's: the identity of what you're talking
            to, in the header rather than as a banner inside the pane. In a
            workspace that's the tenant and — the part that actually matters — that
            teammates can read this. In Personal it's the size of the library being
            searched, the same count the sidebar's "All notes" shows, from the same
            store. Not shown for a workspace, where retrieval runs server-side and
            the local mirror can lag, so the number would be a claim we can't make.
            `-ml-2` pulls the first chip's own padding back to the title's edge. */}
        <div className="-ml-2 flex flex-wrap items-center">
          {workspaceName ? (
            <span className="nd-meta">
              <Users size={14} strokeWidth={1.7} />
              {workspaceName} · visible to members
            </span>
          ) : (
            <span className="nd-meta">
              <Files size={14} strokeWidth={1.7} />
              {noteCount === 1 ? "1 note" : `${noteCount} notes`}
            </span>
          )}
        </div>
      </div>

      {/* The page opens on the composer, and the panel runs edge to edge inside
          this gutter — no rail, no internal padding of its own. */}
      <div className="flex-1 min-h-0 max-w-[820px] mx-auto w-full px-8 pt-2 pb-5 flex flex-col">
        <ChatPanel target={GLOBAL_TARGET} onControls={setControls} variant="page" />
      </div>
    </div>
  );
}
