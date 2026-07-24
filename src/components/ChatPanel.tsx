import { memo, useCallback, useEffect, useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { useNavigate } from "react-router-dom";
import { open as openExternal } from "@tauri-apps/plugin-shell";
import {
  AlertTriangle,
  ChevronDown,
  CornerDownLeft,
  FileText,
  Loader2,
  MessageCircle,
  Settings2,
} from "lucide-react";
import {
  ipc,
  onChatCitations,
  onChatError,
  onChatToolActivity,
  onChatTextDelta,
  type ChatCitation,
  type ChatMessageDto,
  type ChatScope,
  type ChatUsage,
  type ConversationMeta,
} from "../lib/ipc";
import { useNotesStore } from "../lib/store";
import { useCloudStore } from "../lib/cloud";
import { useChatReadiness } from "./provider/useChatReadiness";
import { SelectablePopover, type PopoverItem } from "./SelectablePopover";
import { RECOMMENDED_OLLAMA_MODEL } from "../lib/localModels";
import { CommandSnippet } from "./CommandSnippet";
import { cn } from "../lib/cn";

// Chat over the user's Notes (issues #46 + #47). The assistant runs an agentic
// retrieval loop on the backend: it searches/reads notes with tools, streams
// its answer, and cites the notes it drew from. A Scope popover controls how
// broadly it searches (this note / this folder / all notes) as a live filter.

const Markdown = memo(function Markdown({ source }: { source: string }) {
  return <ReactMarkdown remarkPlugins={[remarkGfm]}>{source}</ReactMarkdown>;
});

function partsText(m: ChatMessageDto): string {
  return m.parts
    .filter((p) => p.type === "text")
    .map((p) => (p.type === "text" ? p.text : ""))
    .join("");
}

// Citations for an assistant message, gathered from its tool parts and
// de-duplicated by note (a note cited by several tools shows one chip).
function messageCitations(m: ChatMessageDto): ChatCitation[] {
  const seen = new Set<string>();
  const out: ChatCitation[] = [];
  for (const p of m.parts) {
    if (p.type !== "tool" || !p.citations) continue;
    for (const c of p.citations) {
      if (!seen.has(c.noteId)) {
        seen.add(c.noteId);
        out.push(c);
      }
    }
  }
  return out;
}

// A running tool call → a human progress line (story 18).
function toolActivityLabel(name: string): string {
  switch (name) {
    case "search_notes":
      return "Searching your notes…";
    case "get_note":
      return "Reading a note…";
    case "list_notes":
      return "Browsing your notes…";
    default:
      return "Working…";
  }
}

// Past-tense, aggregated summary of the tools an assistant turn used (#63), for
// a persistent one-line receipt above the answer (like Claude Desktop's tool
// rows), e.g. "Searched your notes · Read 2 notes". Returns null when the turn
// used no tools. History, not progress — the caller renders it un-animated,
// unlike the live activity line.
function summarizeToolUse(m: ChatMessageDto): string | null {
  const counts = new Map<string, number>();
  for (const p of m.parts) {
    if (p.type === "tool") counts.set(p.name, (counts.get(p.name) ?? 0) + 1);
  }
  if (counts.size === 0) return null;
  const seen = new Set<string>();
  const segments: string[] = [];
  for (const [name, n] of counts) {
    const label = toolPastLabel(name, n);
    // Collapse duplicate labels (e.g. two distinct unknown tools → one "Used a tool").
    if (!seen.has(label)) {
      seen.add(label);
      segments.push(label);
    }
  }
  return segments.join(" · ");
}

function toolPastLabel(name: string, count: number): string {
  switch (name) {
    case "search_notes":
      return "Searched your notes";
    case "get_note":
      return count === 1 ? "Read a note" : `Read ${count} notes`;
    case "list_notes":
      return "Browsed your notes";
    default:
      return "Used a tool";
  }
}

// What the panel publishes upward so the Note header (owner of the +/history
// buttons per #62) can render session chrome without duplicating any chat
// state: the conversation list + which one is active, a visibility flag for the
// history affordance, and the two actions the header triggers. ChatPanel stays
// the sole owner of conversationId/messages; this is a read-only projection plus
// action callbacks.
export type ChatSessionControls = {
  /** The note these controls belong to — lets the header ignore a stale
   *  projection for one frame after a note switch. */
  noteId: string;
  conversations: ConversationMeta[];
  activeConversationId: string | null;
  /** Lone-empty-conversation rule (#62): nothing worth browsing → header hides history. */
  canBrowseHistory: boolean;
  newChat: () => Promise<void>;
  openConversation: (id: string) => Promise<void>;
};

export function ChatPanel({
  noteId,
  onControls,
}: {
  noteId: string;
  onControls?: (controls: ChatSessionControls | null) => void;
}) {
  const { loading: readinessLoading, ready, hint, provider, model } = useChatReadiness();
  const [messages, setMessages] = useState<ChatMessageDto[]>([]);
  // This Note's conversations (issue #61/#62), most-recent first from the
  // backend (local rows in Personal; server list merged in a workspace). Feeds
  // the history popover and the history-visibility rule. Reset to [] on note /
  // workspace switch so the header hides until the fresh list is known.
  const [conversations, setConversations] = useState<ConversationMeta[]>([]);
  // The session the panel is on (issue #61). Single-threaded for now: it tracks
  // the note's active/most-recent conversation, captured from the history load
  // and the send result. null until resolved (or when the note has none yet).
  const [conversationId, setConversationId] = useState<string | null>(null);
  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);
  const [streaming, setStreaming] = useState("");
  const [activity, setActivity] = useState<string | null>(null);
  const [liveCitations, setLiveCitations] = useState<ChatCitation[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [truncated, setTruncated] = useState(false);
  // Scope chip breadth. Display state only — the backend conversation row is the
  // source of truth (issue #58); this mirrors it and is (re)initialised from it.
  const [scope, setScope] = useState<ChatScope>("note");
  // A bulk load/switch is in flight (initial load, "+", history select). Drives
  // aria-busy on the log so a screen reader doesn't announce a wholesale message-
  // list replacement; normal sends (appended turns, streamed deltas) leave it
  // false so those stay announced (#64).
  const [bulkLoading, setBulkLoading] = useState(false);
  // Workspace turn allowance for the composer meter (issue #69). null in personal
  // context and on any unavailable/error/unmetered outcome → the display hides.
  const [usage, setUsage] = useState<ChatUsage | null>(null);
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const inputRef = useRef<HTMLTextAreaElement | null>(null);
  // The composer is auto-focused exactly once per mount, on the first transition
  // to ready. Guards against the Ollama readiness probe (polls every 2s) flipping
  // ready false→true and yanking focus out of the note body mid-typing (#64).
  const hasAutoFocusedRef = useRef(false);
  const sendingRef = useRef(false);
  // Bumped on every context switch (note/workspace load, "+", history select).
  // In-flight async work (a streaming send, a history fetch) captures the gen
  // at start and drops its writes if the gen moved — so a stale response can't
  // leak into the conversation the user switched to (#62).
  const switchGenRef = useRef(0);

  // The anchor note's folder drives the "this folder" scope option.
  const folder = useNotesStore((s) => {
    const note = s.notes.find((n) => n.id === noteId);
    const fid = note?.folder_id;
    return fid ? s.folders.find((f) => f.id === fid) ?? null : null;
  });

  // Re-fetch the note's conversation list. Guarded by the switch gen so a slow
  // list load can't clobber the list after a note/workspace switch. Stable per
  // note so the callbacks and publish effect below keep stable identities.
  const reloadConversationList = useCallback(
    async (gen: number) => {
      try {
        const list = await ipc.chatListConversations(noteId);
        if (gen === switchGenRef.current && Array.isArray(list)) setConversations(list);
      } catch {
        /* keep the prior list */
      }
    },
    [noteId],
  );

  // Clear the transient per-turn UI (streaming/activity/errors) and stop
  // accepting stream deltas. Shared by the note-switch load effect and every
  // in-pane switch so the reset stays in one place.
  const resetTransient = useCallback(() => {
    sendingRef.current = false;
    setSending(false);
    setStreaming("");
    setActivity(null);
    setLiveCitations([]);
    setError(null);
    setTruncated(false);
  }, []);

  // Advance the switch gen (dropping any in-flight send/history writes) and
  // reset transient UI. Shared by "+" and history select so a switch mid-stream
  // can't bleed into the loaded conversation.
  const beginSwitch = useCallback((): number => {
    const gen = ++switchGenRef.current;
    resetTransient();
    // The message list is about to be wholesale-replaced — defer SR announcements.
    setBulkLoading(true);
    return gen;
  }, [resetTransient]);

  // "+" — start (or reuse) a fresh conversation and switch the pane to it. The
  // backend no-ops when the most-recent conversation is already empty, returning
  // it instead of creating a duplicate; either way we land on `meta` with an
  // empty message list and its inherited breadth reflected in the Scope chip.
  const newChat = useCallback(async () => {
    const gen = beginSwitch();
    try {
      const meta = await ipc.chatNewConversation(noteId);
      if (gen !== switchGenRef.current || !meta) return;
      setConversationId(meta.id);
      setMessages([]);
      setScope(meta.breadth ?? "note");
      setConversations((prev) => [meta, ...prev.filter((c) => c.id !== meta.id)]);
      void reloadConversationList(gen);
    } catch (e) {
      if (gen === switchGenRef.current) setError(String(e));
    } finally {
      if (gen === switchGenRef.current) setBulkLoading(false);
      // Return focus to the composer after the switch so keyboard users aren't
      // dropped into the void (#64).
      inputRef.current?.focus();
    }
  }, [noteId, beginSwitch, reloadConversationList]);

  // Load a conversation chosen from the history popover: its messages and its
  // own stored breadth (so the Scope chip tracks the loaded conversation, #62).
  const openConversation = useCallback(
    async (id: string) => {
      const gen = beginSwitch();
      setConversationId(id);
      try {
        const [h, b] = await Promise.all([
          ipc.chatHistory(noteId, id),
          ipc.chatGetBreadth(noteId, id),
        ]);
        if (gen !== switchGenRef.current) return;
        setMessages(h?.messages ?? []);
        if (h?.conversationId) setConversationId(h.conversationId);
        if (b) setScope(b);
      } catch {
        if (gen === switchGenRef.current) setMessages([]);
      } finally {
        if (gen === switchGenRef.current) setBulkLoading(false);
        // Return focus to the composer after loading the conversation (#64).
        inputRef.current?.focus();
      }
    },
    [noteId, beginSwitch],
  );

  // Chat is pinned to the loaded context (issue #58). The chat follows the
  // active workspace (Personal when none) — there's no tenant picker; the
  // sidebar WorkspaceSwitcher is the only way to change where chat goes. We read
  // the current workspace id to re-key the load effect (a switch reloads the
  // other context's conversation) and the name for the context indicator.
  const workspaceName = useCloudStore((s) =>
    s.status.logged_in ? s.status.current_workspace?.name ?? null : null,
  );
  const workspaceId = useCloudStore((s) =>
    s.status.logged_in ? s.status.current_workspace?.id ?? null : null,
  );

  // Fetch the workspace turn allowance for the composer meter (issue #69). Only
  // in a workspace — personal chat is unmetered, so we skip the round-trip and
  // clear the display. Gen-guarded so a slow response can't land after a switch.
  // The backend already collapses every unavailable/error/unmetered case to
  // null, so we just mirror whatever it returns (null hides the display).
  const refreshUsage = useCallback(
    async (gen: number) => {
      if (!workspaceId) {
        setUsage(null);
        return;
      }
      try {
        const u = await ipc.chatUsage();
        if (gen === switchGenRef.current) setUsage(u);
      } catch {
        if (gen === switchGenRef.current) setUsage(null);
      }
    },
    [workspaceId],
  );

  // Load persisted history + breadth on mount, note change, and workspace
  // switch, clearing transient state. Keying on the workspace id makes a context
  // switch reload the right conversation — this replaces the old per-tenant
  // reset that lived in `changeTenant`. On unmount / change, rebuild the note's
  // retrieval index so edits made this session are searchable next time.
  useEffect(() => {
    const gen = ++switchGenRef.current;
    let cancelled = false;
    setInput("");
    resetTransient();
    setConversationId(null);
    // Hide the history affordance until the fresh list is known (no flicker of
    // the previous note's conversations on switch, #62).
    setConversations([]);
    // Clear the turn meter until the new context's fetch resolves (#69). Scoped
    // to this effect, not resetTransient: usage is per-WORKSPACE, so clearing on
    // every per-conversation "+"/history switch would flicker it needlessly.
    setUsage(null);
    // Defer SR announcements until the initial message list has loaded (#64).
    setBulkLoading(true);
    // Both async loads gate on the gen as well as `cancelled`: an in-flight
    // initial fetch could otherwise resolve after the user hits "+" / opens a
    // history entry and clobber the just-switched-to conversation (#62). The gen
    // moves on every switch; `cancelled` alone doesn't (beginSwitch never flips
    // it), so we need both.
    ipc
      .chatHistory(noteId)
      .then((h) => {
        if (!cancelled && gen === switchGenRef.current) {
          setMessages(h.messages);
          setConversationId(h.conversationId);
          setBulkLoading(false);
        }
      })
      .catch(() => {
        if (!cancelled && gen === switchGenRef.current) {
          setMessages([]);
          setBulkLoading(false);
        }
      });
    // Initialise the chip from the backend's persisted breadth in one round
    // trip; fall back to "note" if it can't be read.
    ipc
      .chatGetBreadth(noteId)
      .then((b) => {
        if (!cancelled && gen === switchGenRef.current && b) setScope(b);
      })
      .catch(() => {
        if (!cancelled && gen === switchGenRef.current) setScope("note");
      });
    void reloadConversationList(gen);
    void refreshUsage(gen);
    return () => {
      cancelled = true;
      void ipc.chatReindexNote(noteId).catch(() => {});
    };
  }, [noteId, workspaceId, reloadConversationList, refreshUsage, resetTransient]);

  // Persist a breadth change to the conversation (issue #58): update the chip
  // optimistically, then write it through so the next turn reads the new value.
  function selectBreadth(next: ChatScope) {
    setScope(next);
    void ipc.chatSetBreadth(noteId, conversationId, next).catch((e) => setError(String(e)));
  }

  // Stream subscription. Bound once; cancelled-flag + claim keeps it StrictMode-
  // and async-listen-safe. Deltas/activity accepted only while our send is live.
  useEffect(() => {
    let cancelled = false;
    const unsubs: (() => void)[] = [];
    const claim = (u: () => void) => {
      if (cancelled) u();
      else unsubs.push(u);
    };
    onChatTextDelta((e) => {
      if (sendingRef.current) {
        setStreaming((s) => s + e.delta);
        setActivity(null); // text is flowing — clear the progress line
      }
    }).then(claim);
    onChatToolActivity((e) => {
      if (sendingRef.current) setActivity(toolActivityLabel(e.name));
    }).then(claim);
    onChatCitations((e) => {
      if (sendingRef.current) {
        setLiveCitations((prev) => {
          const seen = new Set(prev.map((c) => c.noteId));
          return [...prev, ...e.citations.filter((c) => !seen.has(c.noteId))];
        });
      }
    }).then(claim);
    onChatError((e) => {
      if (sendingRef.current) setError(e.message);
    }).then(claim);
    return () => {
      cancelled = true;
      unsubs.forEach((u) => u());
    };
  }, []);

  useEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [messages, streaming, sending, activity]);

  // Focus the composer the FIRST time the pane becomes usable (issue #64):
  // opening the Chat tab lands the cursor in the input. Guarded to one focus per
  // mount so the Ollama readiness probe re-flipping ready false→true doesn't
  // steal focus from the note body while the user is typing there.
  useEffect(() => {
    if (ready && !hasAutoFocusedRef.current) {
      hasAutoFocusedRef.current = true;
      inputRef.current?.focus();
    }
  }, [ready]);

  // Publish the session controls to the owner (Note header) whenever the list,
  // the active conversation, or its emptiness changes. Only while ready — no
  // chat chrome before a provider is configured. `null` tells the header to
  // render nothing. Actions are stable, so this fires only on real data changes.
  useEffect(() => {
    if (!onControls) return;
    if (!ready) {
      onControls(null);
      return;
    }
    // Hide only for a lone empty conversation: nothing to browse. Show once
    // there's a second conversation (incl. a teammate's), any stored non-empty
    // conversation, or a live just-sent turn. Keyed off list content rather than
    // the resolved active id so it doesn't flicker while history resolves (#62).
    const canBrowseHistory =
      conversations.length > 1 ||
      conversations.some((c) => c.messageCount > 0) ||
      messages.length > 0;
    onControls({
      noteId,
      conversations,
      activeConversationId: conversationId,
      canBrowseHistory,
      newChat,
      openConversation,
    });
  }, [
    onControls,
    ready,
    noteId,
    conversations,
    conversationId,
    messages.length,
    newChat,
    openConversation,
  ]);

  async function send() {
    const text = input.trim();
    if (!text || sending || !ready) return;
    setInput("");
    setError(null);
    setStreaming("");
    setActivity(null);
    setLiveCitations([]);
    setTruncated(false);
    setSending(true);
    sendingRef.current = true;
    // Snapshot the switch gen: if the user opens another conversation or hits
    // "+" mid-send, the gen moves and we drop this turn's writes (#62).
    const gen = switchGenRef.current;
    const optimistic: ChatMessageDto = {
      id: `optimistic-${Date.now()}`,
      role: "user",
      seq: -1,
      parts: [{ type: "text", id: "optimistic", text }],
      createdAt: Date.now(),
    };
    setMessages((m) => [...m, optimistic]);
    try {
      // Send to the resolved session (null lazily creates the note's first one),
      // capturing the id the backend landed on so the reload targets it (#61).
      const result = await ipc.chatSend(noteId, conversationId, text);
      if (gen !== switchGenRef.current) return;
      setConversationId(result.conversationId);
      const h = await ipc.chatHistory(noteId, result.conversationId);
      if (gen !== switchGenRef.current) return;
      setMessages(h.messages);
      setTruncated(result.truncated);
      // A first turn creates the conversation and bumps message counts — refresh
      // the list so the history popover + its visibility rule stay current (#62).
      void reloadConversationList(gen);
      // A completed turn consumes allowance — refresh the composer meter (#69).
      void refreshUsage(gen);
    } catch (e) {
      if (gen !== switchGenRef.current) return;
      setError((prev) => prev ?? String(e));
      try {
        const h = await ipc.chatHistory(noteId, conversationId);
        if (gen === switchGenRef.current) setMessages(h.messages);
      } catch {
        /* keep the optimistic view */
      }
    } finally {
      if (gen === switchGenRef.current) {
        setStreaming("");
        setActivity(null);
        setSending(false);
        sendingRef.current = false;
      }
    }
  }

  function onKeyDown(e: React.KeyboardEvent<HTMLTextAreaElement>) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      void send();
    }
  }

  if (!readinessLoading && !ready) {
    const isOllama = provider === "ollama";
    return (
      <div className="flex-1 min-h-0 flex flex-col items-center justify-center gap-3 px-8 text-center">
        <Settings2 size={22} strokeWidth={1.5} className="text-[var(--color-text-disabled)]" />
        <p className="text-sm text-[var(--color-text-muted)]">
          {hint || "Set up an AI Chat provider in Settings → Chat to start chatting."}
        </p>
        {isOllama && (
          <div className="w-full max-w-sm flex flex-col gap-2">
            <p className="text-xs text-[var(--color-text-disabled)]">
              Don't have Ollama yet?{" "}
              <button
                type="button"
                onClick={() => openExternal("https://ollama.com/download")}
                className="underline text-[var(--color-text-muted)] hover:text-[var(--color-text)]"
              >
                Install Ollama
              </button>
              , then pull a chat model:
            </p>
            <CommandSnippet command={`ollama pull ${model || RECOMMENDED_OLLAMA_MODEL}`} />
          </div>
        )}
        <p className="text-xs text-[var(--color-text-disabled)]">
          Configured in Settings → Chat, separately from your transcription and summary providers.
        </p>
      </div>
    );
  }

  const showTyping = sending && streaming.length === 0 && !activity;

  return (
    <div className="flex-1 min-h-0 flex flex-col">
      {/* Workspace context signal (#63): nothing in Personal; one muted line in
          a workspace. It sits OUTSIDE the scroll container as a fixed flex item,
          so it stays visible as a privacy signal while messages scroll beneath
          it. It must own its vertical space with a bottom hairline, or scrolled
          bubbles butt straight up against the text with no separation. No
          breadth/tenant chrome here — breadth moved to the composer, and the
          tenant is pinned to the loaded workspace (#58). */}
      {workspaceName && (
        <div className="shrink-0 px-4 py-2 border-b border-[var(--color-line)] text-xs text-[var(--color-text-muted)]">
          Chatting in {workspaceName} · visible to members
        </div>
      )}

      {/* Message area as an aria-live log (#64): streamed deltas, "Thinking…"
          and tool-activity lines are announced politely to screen readers. */}
      <div
        ref={scrollRef}
        role="log"
        aria-live="polite"
        aria-busy={bulkLoading}
        aria-label="Chat messages"
        className="flex-1 min-h-0 overflow-y-auto px-4 py-4 flex flex-col gap-3"
      >
        {messages.length === 0 && !sending ? (
          <div className="flex-1 flex flex-col items-center justify-center gap-3 px-6 text-center">
            <MessageCircle size={22} strokeWidth={1.5} className="text-[var(--color-text-disabled)]" />
            <p className="text-sm text-[var(--color-text-muted)]">
              Ask anything about your notes — it searches, reads, and cites them to answer.
            </p>
          </div>
        ) : (
          <ul className="flex flex-col gap-3 list-none">
            {messages.map((m) => (
              <li key={m.id}>
                <Bubble
                  role={m.role}
                  text={partsText(m)}
                  citations={messageCitations(m)}
                  toolSummary={summarizeToolUse(m)}
                />
              </li>
            ))}
            {sending && streaming.length > 0 && (
              <li>
                <Bubble role="assistant" text={streaming} citations={liveCitations} />
              </li>
            )}
          </ul>
        )}
        {activity && (
          <div className="self-start flex items-center gap-1.5 text-xs text-[var(--color-text-muted)] px-1">
            <Loader2
              size={12}
              strokeWidth={2}
              aria-hidden="true"
              className="animate-spin motion-reduce:animate-none"
            />
            {/* Same soft pulse as "Thinking…"; static under prefers-reduced-motion. */}
            <span className="animate-pulse motion-reduce:animate-none">{activity}</span>
          </div>
        )}
        {showTyping && (
          <div className="self-start text-xs text-[var(--color-text-muted)] px-1">
            <span className="animate-pulse motion-reduce:animate-none">Thinking…</span>
          </div>
        )}
      </div>

      {error && (
        <div className="mx-4 mb-2 flex items-start gap-2 rounded-[var(--radius)] bg-[var(--color-accent-soft)] px-3 py-2 text-xs text-[var(--color-accent-text)]">
          <AlertTriangle size={13} strokeWidth={1.7} className="mt-px shrink-0" />
          <span>{error}</span>
        </div>
      )}
      {truncated && !error && (
        <div className="mx-4 mb-2 text-xs text-[var(--color-text-muted)]">
          Note content was truncated to fit the context budget — the answer may miss details near
          the end.
        </div>
      )}

      <div className="shrink-0 border-t border-[var(--color-line)] p-2.5 flex flex-col gap-1.5">
        {/* Send button lives INSIDE the input (Claude-Desktop style): the input
            spans the full composer width, and the button is absolutely pinned to
            its right edge. The textarea is `block` so it has no inline-block
            baseline gap — the relative wrapper's height then equals the visible
            input's height, and `top-1/2 -translate-y-1/2` centres the button
            against the true input height (no pixel nudging). The small icon-button
            variant (28px) fits comfortably inside without touching the edges.
            The textarea has no native outline, so a token-based focus-within
            border on the wrapper makes keyboard focus visible (#64). */}
        <div className="relative rounded-[var(--radius)] border border-transparent transition-colors focus-within:border-[var(--color-text-muted)]">
          <textarea
            ref={inputRef}
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={onKeyDown}
            rows={1}
            placeholder="Ask about your notes…"
            aria-label="Ask about your notes"
            disabled={sending}
            className="block w-full resize-none max-h-40 bg-transparent text-sm leading-relaxed pl-2 pr-11 py-2 outline-none placeholder:text-[var(--color-text-muted)] disabled:opacity-60"
          />
          <button
            type="button"
            onClick={() => void send()}
            disabled={sending || input.trim().length === 0}
            title="Send"
            aria-label="Send"
            className={cn(
              "nd-btn-icon nd-btn-icon-sm absolute right-1.5 top-1/2 -translate-y-1/2",
              (sending || input.trim().length === 0) && "opacity-40 pointer-events-none",
            )}
          >
            <CornerDownLeft size={16} strokeWidth={1.7} />
          </button>
        </div>
        {/* Composer control row: breadth picker bottom-left, workspace turn
            allowance bottom-right (#69). The meter shows only in a metered
            workspace — `usage` is null in personal context and on any
            unavailable/error/unmetered outcome, so nothing renders then. */}
        <div className="flex items-center justify-between px-1">
          <BreadthPicker
            scope={scope}
            onScope={selectBreadth}
            folderName={folder?.name ?? null}
          />
          {usage && (
            <span className="text-xs text-[var(--color-text-muted)] tabular-nums">
              {usage.used}/{usage.cap} turns
            </span>
          )}
        </div>
      </div>
    </div>
  );
}

// Retrieval-breadth picker for the composer (#63). A quiet, sentence-case
// labelled dropdown — not a chip, not bordered — that always shows the current
// breadth on the trigger (fixing the old bug where the active value only
// appeared inside the popover). Semantics unchanged (issue #58): per-
// conversation, persisted via chat_set_breadth. "Folder: {name}" only appears
// when the note has a folder (nothing to widen to otherwise).
function BreadthPicker({
  scope,
  onScope,
  folderName,
}: {
  scope: ChatScope;
  onScope: (s: ChatScope) => void;
  folderName: string | null;
}) {
  const items: PopoverItem[] = [
    { id: "note", label: "This note" },
    ...(folderName ? [{ id: "folder", label: `Folder: ${folderName}` }] : []),
    { id: "all", label: "All notes" },
  ];
  // If the folder disappears while "folder" is selected, fall back to "note".
  const activeId = scope === "folder" && !folderName ? "note" : scope;
  const label = items.find((i) => i.id === activeId)?.label ?? "This note";

  return (
    <SelectablePopover
      ariaLabel="Chat scope"
      items={items}
      activeId={activeId}
      onSelect={(id) => onScope((id as ChatScope) ?? "note")}
      trigger={
        <span className="inline-flex items-center gap-1 text-xs text-[var(--color-text-muted)] cursor-pointer hover:text-[var(--color-text)] transition-colors">
          {label}
          <ChevronDown size={12} strokeWidth={2} aria-hidden="true" className="shrink-0" />
        </span>
      }
    />
  );
}

function Bubble({
  role,
  text,
  citations,
  toolSummary,
}: {
  role: "user" | "assistant";
  text: string;
  citations: ChatCitation[];
  // Persistent tool-use receipt for an assistant turn (#63), null when none.
  toolSummary?: string | null;
}) {
  // User turns keep a right-aligned bubble (now the quiet gray pair — authorship
  // reads from alignment, #64). Assistant turns are plain full-width blocks on
  // the canvas, Claude-Desktop style: no bubble, no background — just the answer,
  // its tool-use receipt above, and citation chips below.
  if (role === "user") {
    return (
      <div className="flex flex-col items-end">
        <div className="max-w-[85%] rounded-[var(--radius-card)] px-3 py-2 text-sm leading-relaxed bg-[var(--color-pill-hover)] text-[var(--color-text)]">
          {/* Author label for screen readers — authorship no longer depends on
              colour/alignment alone (#64). */}
          <span className="sr-only">You: </span>
          <span className="whitespace-pre-wrap">{text}</span>
        </div>
      </div>
    );
  }
  return (
    <div className="flex flex-col">
      <span className="sr-only">Assistant: </span>
      {toolSummary && (
        // Reads like a paragraph of the answer: same body size (text-sm) and
        // left edge as the answer block (no inset), with margin above and below
        // so it isn't flush against neighbouring turns. Muted, since it's a
        // secondary receipt of what the assistant did (#63).
        <div className="my-1.5 text-sm text-[var(--color-text-muted)]">{toolSummary}</div>
      )}
      <div className="prose-summary text-sm leading-relaxed text-[var(--color-text)]">
        <Markdown source={text} />
      </div>
      {citations.length > 0 && (
        <div className="mt-1.5 flex flex-wrap gap-1.5">
          {citations.map((c) => (
            <CitationChip key={c.noteId} citation={c} />
          ))}
        </div>
      )}
    </div>
  );
}

function CitationChip({ citation }: { citation: ChatCitation }) {
  const navigate = useNavigate();
  const title = citation.title.trim() || "Untitled note";
  const date = new Date(citation.createdAt).toLocaleDateString();
  return (
    <button
      type="button"
      onClick={() => navigate(`/note/${citation.noteId}`)}
      title={`Open “${title}” (${date})`}
      // Sentence-case, quiet chip built from existing tokens (no nd-chip, so no
      // uppercase mono per design/REFACTOR.md typography rules, #63).
      className="inline-flex items-center gap-1 rounded-[var(--radius)] border border-[var(--color-line-visible)] bg-[var(--color-surface)] px-2 py-1 text-xs text-[var(--color-text-muted)] cursor-pointer transition-colors hover:text-[var(--color-text)] hover:border-[var(--color-text-muted)]"
    >
      <FileText size={11} strokeWidth={1.7} aria-hidden="true" className="shrink-0" />
      <span className="truncate max-w-[16rem]">{title}</span>
    </button>
  );
}
