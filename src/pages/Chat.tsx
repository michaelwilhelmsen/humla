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
// local-side engine (#93). What this file owns is the app bar — the conversation
// list lives in the SIDEBAR (`ChatConversations`), because a list you navigate
// between belongs in the nav column, not in the right-hand slot this app uses for
// context about the thing on screen.
//
// The layout follows the Note view and Claude Desktop rather than inventing
// anything: identity and actions ride in the title-bar row, and the body below is
// only the conversation. No page heading — a second "Chat" under the bar's title
// would be the same word twice, and once a conversation is open its own title is
// the honest thing to show.
//
// Deliberately absent: a greeting. We have no local user name — one exists only
// via `CloudUser.name` when signed into cloud, so it would be blank for the
// local-only majority. The prompt cards on a new chat do the job a greeting
// pretends to. Also absent: a scope picker (its only option would be "All notes";
// narrowing stays a tool argument the model chooses, per #81).

import { useEffect } from "react";
import { useOutletContext } from "react-router-dom";
import { Lock, Sparkles, Users, type LucideIcon } from "lucide-react";
import { ChatPanel } from "../components/ChatPanel";
import { ChatHistoryControls } from "../components/ChatHistoryControls";
import type { LayoutOutletContext } from "../components/Layout";
import { useGlobalChatStore } from "../lib/globalChat";
import { useCloudStore } from "../lib/cloud";
import type { ChatTarget } from "../lib/chatTarget";
import { cn } from "../lib/cn";

// Module-level so the identity is stable across renders — the panel keys its
// load effect off the target's note id, but a stable object costs nothing and
// keeps the prop honest.
const GLOBAL_TARGET: ChatTarget = { kind: "global" };

/** A bordered status pill for the app bar.
 *
 *  Not `.nd-chip`: that utility is uppercase with wide tracking — a survivor of
 *  the pre-v0.30 aesthetic — and the current design system is sentence case
 *  throughout. This is the note meta row's typography in a pill outline, so
 *  identity and status can sit together without either shouting. */
function StatusPill({ icon: Icon, children }: { icon: LucideIcon; children: React.ReactNode }) {
  return (
    <span className="inline-flex items-center gap-1.5 rounded-full border border-[var(--color-line-visible)] px-2.5 py-[3px] text-[12px] text-[var(--color-text-muted)] whitespace-nowrap">
      <Icon size={13} strokeWidth={1.7} className="opacity-75" />
      {children}
    </span>
  );
}

export function Chat() {
  const { sidebarCollapsed } = useOutletContext<LayoutOutletContext>();
  const controls = useGlobalChatStore((s) => s.controls);
  const setControls = useGlobalChatStore((s) => s.setControls);
  const workspaceName = useCloudStore((s) => s.status.current_workspace?.name ?? null);

  // Clear on the way out, or the sidebar would keep rendering a list belonging to
  // a pane that no longer exists. (`ChatPanel` publishes `null` when chat isn't
  // usable, but unmounting isn't one of those moments — it can't publish then.)
  useEffect(() => () => setControls(null), [setControls]);

  // The open conversation's own title once it has one. A fresh thread's title is
  // empty until the backend derives it from the first turn, so "Chat" stands in
  // until then and gives way as soon as there's something to name.
  const active =
    controls?.conversations.find((c) => c.id === controls.activeConversationId) ?? null;
  const barTitle = active?.title.trim() || "Chat";

  return (
    <div className="h-full flex flex-col overflow-hidden">
      {/* Title-bar row, same geometry as the Note view's toolbar: `h-12`, a drag
          region, and a left inset that clears the macOS traffic lights when the
          sidebar card isn't there to host them. */}
      <div
        data-tauri-drag-region
        className={cn(
          "relative z-30 h-12 shrink-0 flex items-center gap-2 pr-3",
          sidebarCollapsed ? "pl-[116px]" : "pl-3",
        )}
      >
        <span className="truncate text-[13.5px] font-medium" title={barTitle}>
          {barTitle}
        </span>

        {/* Tenant, with the same initial badge the note's meta row uses. */}
        <span className="nd-meta shrink-0">
          <span
            className="grid place-items-center w-[17px] h-[17px] rounded-[5px] text-[9px] font-semibold"
            style={{ background: "var(--color-surface-raised)", color: "var(--color-text)" }}
          >
            {(workspaceName ?? "Personal").charAt(0).toUpperCase()}
          </span>
          {workspaceName ?? "Personal"}
        </span>

        {/* Who can read this — the highest-stakes fact on the screen, so it gets
            the pill treatment rather than being tucked into a sentence. */}
        <StatusPill icon={workspaceName ? Users : Lock}>
          {workspaceName ? "All members" : "Private"}
        </StatusPill>

        <div className="flex-1" />

        {/* What's about to answer. Published by the pane rather than re-derived —
            `useChatReadiness` polls a local Ollama server every 2s and a second
            caller would double that to render one label. Null in a workspace on
            purpose: the turn runs on the server's model, so naming the local
            setting would name something that isn't answering (#80). */}
        {controls?.status && (
          <span
            className="nd-meta shrink-0"
            title={`Answering with ${controls.status.model} (${controls.status.provider}) — change it in Settings → Chat`}
          >
            <Sparkles size={14} strokeWidth={1.7} />
            <span className="max-w-[200px] truncate">{controls.status.model}</span>
          </span>
        )}

        {/* Actions belong to the bar, as they do in the Note view — so the sidebar
            section is purely the list. History is the exception: while the sidebar
            is open it IS the history, and the popover would be a second copy of
            it; collapsed, the popover is the only way back to a past thread. */}
        {controls && <ChatHistoryControls controls={controls} showHistory={sidebarCollapsed} />}
      </div>

      {/* Just the conversation: the log fills the height (so a new chat's prompt
          cards sit centred in it) and the composer is seated at the bottom. */}
      <div className="flex-1 min-h-0 max-w-[820px] mx-auto w-full px-8 pb-4 flex flex-col">
        <ChatPanel target={GLOBAL_TARGET} onControls={setControls} variant="page" />
      </div>
    </div>
  );
}
