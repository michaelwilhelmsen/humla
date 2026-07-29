import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { useNavigate } from "react-router-dom";
import { open as openExternal } from "@tauri-apps/plugin-shell";
import {
  AlertTriangle,
  ChevronDown,
  CornerDownLeft,
  FileText,
  Files,
  Folder,
  Loader2,
  MessageCircle,
  Settings2,
  Square,
  UserRound,
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
  type ChatIndexState,
  ChatUsage,
  type ConversationMeta,
} from "../lib/ipc";
import { useNotesStore } from "../lib/store";
import { cloudApi, formatSeatPrice, useCloudStore, useMemberName, type ChatAddon, type ChatKeyMeta } from "../lib/cloud";
import { useChatReadiness } from "./provider/useChatReadiness";
import { SelectablePopover, type PopoverItem } from "./SelectablePopover";
import { ChatKeyEntry } from "./ChatKeyEntry";
import {
  usageTone,
  liveChatErrorCopy,
  groundingLikelyTruncated,
  conversationTitle,
} from "../lib/chatSessions";
import {
  targetNoteId,
  targetKey,
  targetDefaultScope,
  targetResumesOnOpen,
  type ChatTarget,
} from "../lib/chatTarget";
import { opensPromptPicker, promptsFor, type ChatPrompt } from "../lib/chatPrompts";
import { RECOMMENDED_OLLAMA_MODEL } from "../lib/localModels";
import { CommandSnippet } from "./CommandSnippet";
import { cn } from "../lib/cn";

// Chat over the user's Notes (issues #46 + #47). The assistant runs an agentic
// retrieval loop on the backend: it searches/reads notes with tools, streams
// its answer, and cites the notes it drew from. A Scope popover controls how
// broadly it searches (this note / this folder / all notes) as a live filter.

// Conversations fetched per request (issue #95). `/chat` lists them uncapped in
// the sidebar, so they arrive a page at a time as it scrolls. Sized to overfill
// a tall sidebar on first paint — the point is to avoid loading hundreds, not to
// make the common case take two round-trips.
const PAGE_SIZE = 30;

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
  /** The target these controls belong to, as a stable scalar (`targetKey`) — lets
   *  the header ignore a stale projection for one frame after a switch. A key
   *  rather than the target object because the comparison must be by value. */
  targetKey: string;
  conversations: ConversationMeta[];
  activeConversationId: string | null;
  /** Lone-empty-conversation rule (#62): nothing worth browsing → header hides history. */
  canBrowseHistory: boolean;
  /** Whether another page of conversations might exist (#95). False once a short
   *  page has come back, so a list viewer knows when to stop asking. */
  hasMore: boolean;
  /** A page fetch is in flight — for a "loading…" line, and so a viewer doesn't
   *  need its own guard against firing twice. */
  loadingMore: boolean;
  newChat: () => Promise<void>;
  openConversation: (id: string) => Promise<void>;
  /** Delete a conversation and its messages (#109). Hard — the caller confirms
   *  first. Deleting the OPEN conversation lands the pane on a fresh chat rather
   *  than a dead id. */
  deleteConversation: (id: string) => Promise<void>;
  /** Rename a conversation (#109). Optimistic; a rejected rename puts the old
   *  title back. An empty/unchanged title is a no-op. */
  renameConversation: (id: string, title: string) => Promise<void>;
  /** Append the next page. Safe to call repeatedly: it no-ops while a fetch is in
   *  flight or once the end is known, which a scroll observer relies on. */
  loadMore: () => Promise<void>;
  /** What the pane is about to answer with (#95), for a host that shows it in its
   *  own header chrome instead of the composer row — `/chat` does.
   *
   *  Published rather than re-derived: `useChatReadiness` polls a local Ollama
   *  server every 2s, so a second caller would double that probe to show one
   *  label. Null in a workspace, where the turn runs on the SERVER's model and
   *  this local setting would name something that isn't answering (#80). */
  status: { provider: string; model: string } | null;
};

/** Which host the panel is rendering into (issue #95).
 *
 *  `panel` is the Note's right-hand context card: narrow, already inside a
 *  bordered surface, and the only place its own tenant line can go — so it keeps
 *  its internal gutters and hairlines.
 *
 *  `page` is the `/chat` route: the page owns the gutter and the header, so the
 *  panel drops both its horizontal padding and its separators and lets the
 *  content run edge to edge. Wide enough, too, for the prompt cards a narrow
 *  panel can't fit.
 *
 *  A variant rather than a scatter of booleans: every difference below follows
 *  from which host is responsible for the chrome, and that's one fact. */
export type ChatPanelVariant = "panel" | "page";

export function ChatPanel({
  target,
  onControls,
  variant = "panel",
}: {
  target: ChatTarget;
  onControls?: (controls: ChatSessionControls | null) => void;
  variant?: ChatPanelVariant;
}) {
  const onPage = variant === "page";
  // The page supplies its own gutter, so the panel's own padding would double it.
  const gutter = onPage ? "" : "px-4";
  // The anchor note id for IPC — null means the whole library (#93). This is also
  // the pane's dependency identity: it's already a stable scalar and `null` is
  // exactly "global", so a change in it is exactly a change of target. Depending on
  // `target` itself would re-run the load effect forever, since the parent rebuilds
  // the object on most renders.
  const noteId = targetNoteId(target);
  // Non-nullable identity for the projection the Note header compares by value.
  const paneKey = targetKey(target);
  const { loading: readinessLoading, ready, hint, provider, model } = useChatReadiness();
  const [messages, setMessages] = useState<ChatMessageDto[]>([]);
  // This Note's conversations (issue #61/#62), most-recent first from the
  // backend (local rows in Personal; server list merged in a workspace). Feeds
  // the history popover and the history-visibility rule. Reset to [] on note /
  // workspace switch so the header hides until the fresh list is known.
  const [conversations, setConversations] = useState<ConversationMeta[]>([]);
  // Paging state for that list (#95). `hasMore` starts true so the first "load
  // more" is allowed to try; it settles on the first short page.
  const [hasMoreConversations, setHasMoreConversations] = useState(true);
  const [loadingMoreConversations, setLoadingMoreConversations] = useState(false);
  // Refs, not state, for the two values `loadMoreConversations` reads at call
  // time: a ref can't hand a stale closure the wrong offset, and the in-flight
  // flag has to be set synchronously to swallow a repeat call in the same tick.
  const conversationsRef = useRef<ConversationMeta[]>([]);
  conversationsRef.current = conversations;
  const loadingMoreRef = useRef(false);
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
  // Machine reason for the last error (issue #76) → drives role-aware BYOK copy
  // in the live pane. null when cleared; the personal loop and unknown errors
  // report an empty reason, which liveChatErrorCopy treats the same (falls back
  // to the server message).
  const [errorReason, setErrorReason] = useState<string | null>(null);
  const [truncated, setTruncated] = useState(false);
  // Scope chip breadth. Display state only — the backend conversation row is the
  // source of truth (issue #58); this mirrors it and is (re)initialised from it.
  const [scope, setScope] = useState<ChatScope>(() => targetDefaultScope(target));
  // The conversation's pinned authorship filter (#103): a user id, or "" for off.
  // Display state only, mirroring the backend row exactly as `scope` does. A user
  // id and not a flag because a workspace's conversation list is shared — a
  // boolean would mean different notes to each reader of the same thread.
  const [ownerFilter, setOwnerFilter] = useState("");
  // Prompt picker visibility (#80). Opened by typing "/" into an empty composer;
  // the picker owns the keys while it's up (it takes focus), so the composer's
  // own key handler doesn't need to know about Escape.
  const [promptsOpen, setPromptsOpen] = useState(false);
  // A bulk load/switch is in flight (initial load, "+", history select). Drives
  // aria-busy on the log so a screen reader doesn't announce a wholesale message-
  // list replacement; normal sends (appended turns, streamed deltas) leave it
  // false so those stay announced (#64).
  const [bulkLoading, setBulkLoading] = useState(false);
  // Workspace turn allowance for the composer meter (issue #69). null in personal
  // context and on any unavailable/error/unmetered outcome → the display hides.
  const [usage, setUsage] = useState<ChatUsage | null>(null);
  // Workspace BYOK key metadata (issue #76). `undefined` = still checking (only
  // in a managed workspace, to avoid flashing the composer before we know), then
  // a `ChatKeyMeta` or `null`. Feeds the activation-pane decision.
  const [keyMeta, setKeyMeta] = useState<ChatKeyMeta | null | undefined>(null);
  // The server's view of the workspace retrieval index (#102). null = no
  // information (Personal, an older server, or a failed lookup) → the empty-state
  // copy falls back to the local sync proxy.
  const [indexState, setIndexState] = useState<ChatIndexState | null>(null);
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

  // The anchor note's folder drives the "this folder" scope option. A library-wide
  // pane has no anchor, so no folder option — the store lookup is skipped rather
  // than searched for a null id.
  const folder = useNotesStore((s) => {
    if (noteId === null) return null;
    const note = s.notes.find((n) => n.id === noteId);
    const fid = note?.folder_id;
    return fid ? s.folders.find((f) => f.id === fid) ?? null : null;
  });

  // The anchor note itself, for the pre-emptive truncation hint (#80). Selected
  // as the stored object — a derived `{...}` literal would be a fresh snapshot
  // on every call and spin useSyncExternalStore into an update loop. Null for a
  // library-wide pane, which injects no note content and so can't overflow the
  // grounding budget.
  const anchorNote = useNotesStore((s) =>
    noteId === null ? null : s.notes.find((n) => n.id === noteId) ?? null,
  );
  // Is there anything at all to retrieve from? `loaded` matters as much as the
  // count: `notes: []` on its own also means "the first load hasn't landed", and
  // acting on that would fire for a frame on every launch. (The full rule is
  // derived below as `nothingToSearch`, once the cloud selectors are in scope.)
  const libraryEmpty = useNotesStore((s) => s.loaded && s.notes.length === 0);

  const likelyTruncated = useMemo(() => {
    if (!anchorNote) return false;
    // Body is HTML in the store but plain text in the prompt — measure the text,
    // or tag-heavy markup would trip the hint far too early.
    const el = document.createElement("div");
    el.innerHTML = anchorNote.body ?? "";
    return groundingLikelyTruncated({
      bodyText: el.textContent || "",
      transcript: anchorNote.transcript ?? "",
      summary: anchorNote.summary ?? "",
    });
  }, [anchorNote]);

  // Re-fetch the FIRST page of the target's conversation list. Guarded by the
  // switch gen so a slow list load can't clobber the list after a note/workspace
  // switch. Stable per note so the callbacks and publish effect below keep stable
  // identities.
  //
  // Always page one: every caller is a "something changed, show me the top of the
  // list again" moment (initial load, "+", a send that retitled a thread), and the
  // list is ordered most-recent first, so page one is where the change landed.
  const reloadConversationList = useCallback(
    async (gen: number) => {
      try {
        const list = await ipc.chatListConversations(noteId, { limit: PAGE_SIZE, offset: 0 });
        if (gen !== switchGenRef.current || !Array.isArray(list)) return;
        setConversations(list);
        setHasMoreConversations(list.length === PAGE_SIZE);
      } catch {
        /* keep the prior list */
      }
    },
    [noteId],
  );

  // Append the next page (issue #95). A short page means the end: the backend
  // returns fewer rows than asked for, and an empty page past the end, so this
  // converges without needing a total count.
  //
  // Guarded three ways — an in-flight fetch, a known end, and the switch gen —
  // because the sidebar's scroll observer can fire repeatedly for one gesture.
  const loadMoreConversations = useCallback(async () => {
    if (loadingMoreRef.current || !hasMoreConversations) return;
    loadingMoreRef.current = true;
    const gen = switchGenRef.current;
    setLoadingMoreConversations(true);
    try {
      const offset = conversationsRef.current.length;
      const next = await ipc.chatListConversations(noteId, { limit: PAGE_SIZE, offset });
      if (gen !== switchGenRef.current || !Array.isArray(next)) return;
      // De-dupe by id: a conversation that got bumped to page one between the two
      // fetches would otherwise appear twice.
      setConversations((prev) => {
        const seen = new Set(prev.map((c) => c.id));
        return [...prev, ...next.filter((c) => !seen.has(c.id))];
      });
      setHasMoreConversations(next.length === PAGE_SIZE);
    } catch {
      /* keep what we have; the observer will retry on the next scroll */
    } finally {
      loadingMoreRef.current = false;
      setLoadingMoreConversations(false);
    }
  }, [noteId, hasMoreConversations]);

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
    setErrorReason(null);
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
    // A drafting target creates nothing (#120): "+" is a local reset to the same
    // empty state the pane already opens in, so it costs no IPC and leaves no row
    // behind if the user changes their mind. Breadth inherits from what's on
    // screen — the chip doesn't move, which matches the old inherit-on-create.
    if (!targetResumesOnOpen(target)) {
      setConversationId(null);
      setMessages([]);
      setInput("");
      setBulkLoading(false);
      inputRef.current?.focus();
      return;
    }
    try {
      const meta = await ipc.chatNewConversation(noteId);
      if (gen !== switchGenRef.current || !meta) return;
      setConversationId(meta.id);
      setMessages([]);
      setScope(meta.breadth ?? targetDefaultScope(target));
      setOwnerFilter(meta.ownerFilter ?? "");
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

  // Delete a conversation (issue #109). Hard: there is no Trash for chat, which
  // is why the callers confirm first.
  //
  // Server-then-local ordering lives in the command; what this owns is the pane's
  // state afterwards. Deleting the OPEN conversation can't just drop the row — the
  // pane would keep rendering messages belonging to an id that no longer exists,
  // and the next send would resurrect it — so it starts a fresh chat. Deleting any
  // other row leaves the open one alone, which is the common case from the sidebar.
  const deleteConversation = useCallback(
    async (id: string) => {
      const wasOpen = id === conversationId;
      try {
        await ipc.chatDeleteConversation(noteId, id);
      } catch (e) {
        // Surface it and keep the row: a failed delete that removed the row anyway
        // would read as success and leave the list disagreeing with the backend.
        setError(String(e));
        return;
      }
      setConversations((prev) => prev.filter((c) => c.id !== id));
      if (wasOpen) await newChat();
    },
    [noteId, conversationId, newChat],
  );

  // Rename a conversation (issue #109) — the user's override of the model-derived
  // title. Optimistic, like the breadth and owner-filter writes: the row updates
  // immediately and a failure puts the old title back, because a rename that
  // silently didn't take is worse than one that visibly bounces.
  const renameConversation = useCallback(
    async (id: string, title: string) => {
      const next = title.trim();
      if (!next) return;
      const previous = conversationsRef.current.find((c) => c.id === id);
      if (previous?.title === next) return;
      // Accepting the placeholder is not a rename. Both editors seed their input
      // from the row's DISPLAYED label, which for a still-untitled thread is the
      // "New chat" fallback — so opening the editor and clicking away would store
      // that fallback as a real title, and a real title is exactly what stops the
      // first turn from naming the thread. Compare against the fallback, not just
      // the stored value, or the placeholder becomes permanent.
      if (previous && !previous.title.trim() && next === conversationTitle(previous)) return;
      setConversations((prev) => prev.map((c) => (c.id === id ? { ...c, title: next } : c)));
      try {
        const updated = await ipc.chatRenameConversation(noteId, id, next);
        // Adopt the server's row wholesale — it carries the applied title (which
        // may be capped) and the bumped `updatedAt` the list orders by.
        if (updated) setConversations((prev) => prev.map((c) => (c.id === id ? updated : c)));
      } catch (e) {
        setError(String(e));
        if (previous) {
          setConversations((prev) => prev.map((c) => (c.id === id ? previous : c)));
        }
      }
    },
    [noteId],
  );

  // Load a conversation chosen from the history popover: its messages and its
  // own stored breadth (so the Scope chip tracks the loaded conversation, #62).
  const openConversation = useCallback(
    async (id: string) => {
      const gen = beginSwitch();
      setConversationId(id);
      try {
        const [h, b, o] = await Promise.all([
          ipc.chatHistory(noteId, id),
          ipc.chatGetBreadth(noteId, id),
          ipc.chatGetOwnerFilter(noteId, id).catch(() => ""),
        ]);
        if (gen !== switchGenRef.current) return;
        setMessages(h?.messages ?? []);
        if (h?.conversationId) setConversationId(h.conversationId);
        if (b) setScope(b);
        setOwnerFilter(o ?? "");
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
  // BYOK activation surface (issue #76). `billingEnabled` marks the managed
  // (humla-cloud) server — the only place the activation pane applies; on self-
  // host it never shows (chat_disabled keeps its existing behaviour). Role +
  // members drive owner-vs-member copy; the add-on config drives the pitch.
  const billingEnabled = useCloudStore((s) => s.status.billing_enabled);
  const wsRole = useCloudStore((s) => s.status.current_workspace?.role ?? null);
  const members = useCloudStore((s) => s.members);
  const addon = useCloudStore((s) => s.status.chat_addon ?? null);
  const inWorkspace = !!workspaceId;
  // The signed-in user's own id — compared against the pin DIRECTLY rather than
  // via `useOwnerName`, which returns null both for "that's you" and for "can't
  // resolve them". The chip has to tell those two apart (#103).
  const myUserId = useCloudStore((s) => s.status.user?.id ?? null);
  const isOwner = wsRole === "owner";
  const ownerMember = Object.values(members).find((m) => m.role === "owner");
  const ownerName = ownerMember ? ownerMember.name || ownerMember.email : "the workspace owner";
  // Personal chat gates on personal readiness (a configured provider/key);
  // workspace chat does NOT — it runs on the workspace key, so a member without
  // a personal key still reaches the pane (composer or activation state, #76).
  const paneUsable = inWorkspace || ready;

  // A library-wide pane with nothing to retrieve from would otherwise show the
  // standing invitation — inviting a question whose only honest answer is "I
  // couldn't find anything", which in a metered workspace costs a turn to
  // discover (#95). So the composer holds instead, in one of two states.
  //
  // A NOTE pane never reaches either: its anchor note IS grounding, so there is
  // always something to answer from.
  //
  // Two things split them, and in a workspace the server's word wins (#102).
  //
  // `syncing` is the local proxy: a workspace pulls its notes down after a
  // switch, so a mirror that looks empty mid-pull isn't evidence of an empty
  // workspace. But it's only a proxy — the pull can be idle, with nothing left to
  // pull, while the SERVER index is still backfilling. Workspace retrieval runs
  // over that index, so `indexState` is the authoritative answer: "empty"
  // (never indexed / backfilling) and "quarantined" (withheld in the indexer's
  // deactivation grace window) both mean "not yet", where only "ready" makes an
  // empty result a true statement about the library.
  //
  // null covers Personal (where the local store IS the retrieval corpus, so the
  // local check is already authoritative), an older server without the route, and
  // any failure — all of which fall back to the sync proxy rather than blocking.
  const syncing = useCloudStore((s) => s.syncStatus) === "syncing";
  const indexNotReady = indexState === "empty" || indexState === "quarantined";
  const globalPane = target.kind === "global";
  const nothingToSearch = globalPane && libraryEmpty && !syncing && !indexNotReady;
  const notesStillArriving = globalPane && libraryEmpty && (syncing || indexNotReady);
  const composerHeld = nothingToSearch || notesStillArriving;
  // Does the composer's control row have anything in it? The breadth picker needs
  // an anchor to offer a second option (#95), the model chip is panel-only and
  // Personal-only (#80), the truncation hint needs an anchor note, and the turn
  // meter needs a metered workspace (#69). On `/chat` in Personal that's nothing
  // at all — so don't render the row and leave a gap where content isn't.
  // The authorship pin (#103), shown as a toggle in the same control row.
  //
  // HIDDEN IN PERSONAL — every note is the user's own there, so the control could
  // only ever be the identity function — and HIDDEN UNDER `note` BREADTH, where
  // it is either a no-op (your own note) or empties the pane's own anchor (a
  // teammate's). That is the same reason the date window is dropped under `note`.
  // Presence tracks the SCOPE, not the note: conditioning on who owns the anchor
  // would only change the no-op case, while making the chip flicker between notes
  // for a reason nothing on screen explains.
  const showAuthorPin = inWorkspace && scope !== "note";
  // Editable iff the filter is off, or pinned to you. Anna's way out of a thread
  // pinned to Michael is her own thread — the alternative is her quietly rewriting
  // what his scrollback means. (The backend does NOT enforce this, matching
  // `chat_set_breadth`; making this the one chat setting with a server-side
  // authorization rule wasn't worth it.)
  const authorPinIsMine = ownerFilter !== "" && ownerFilter === myUserId;
  const authorPinEditable = ownerFilter === "" ? myUserId !== null : authorPinIsMine;
  // The pinned person's display name — for the chip's label and for the model's
  // disclosure line. Null when unresolvable (a removed member, or a roster that
  // hasn't loaded); the pin still filters, it just loses its wording.
  const pinnedAuthorName = useMemberName(ownerFilter || null);
  const showComposerControls =
    noteId !== null || (!onPage && !inWorkspace && !!model) || !!usage || showAuthorPin;

  // Activation gating (#76). Only on the managed server + a workspace. While the
  // key metadata is still loading (undefined) show neither composer nor pane, so
  // the composer doesn't flash before we know. `notActivated` = no key + no
  // add-on (usage null). Self-host / personal never reach here. Computed here
  // (above the effects) so the auto-focus effect can depend on it.
  const billingWorkspace = inWorkspace && billingEnabled;
  const activationLoading = billingWorkspace && keyMeta === undefined;
  const notActivated = billingWorkspace && keyMeta != null && !keyMeta.configured && !usage;

  // Fetch, gen-guarded, the workspace turn allowance for the composer meter
  // (#69) and — on the managed server — the BYOK key metadata for the activation
  // decision (#76). Personal → clear both, no round-trip. The backend collapses
  // every unavailable/error/unmetered usage case to null (the meter hides).
  const refreshActivation = useCallback(
    async (gen: number) => {
      if (!workspaceId) {
        setUsage(null);
        setKeyMeta(null);
        setIndexState(null);
        return;
      }
      try {
        const [u, m, ix] = await Promise.all([
          ipc.chatUsage(),
          billingEnabled ? cloudApi.chatKeyMeta(workspaceId) : Promise.resolve(null),
          // Settled separately: this one is a HINT. A rejection here must not
          // take the usage meter and activation decision down with it.
          ipc.chatIndexState().catch(() => null),
        ]);
        if (gen === switchGenRef.current) {
          setUsage(u);
          setKeyMeta(m);
          setIndexState(ix);
        }
      } catch {
        if (gen === switchGenRef.current) {
          setUsage(null);
          setKeyMeta(null);
          setIndexState(null);
        }
      }
    },
    [workspaceId, billingEnabled],
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
    // Mark activation "checking" on a managed workspace so the footer shows
    // neither composer nor pane until we know (#76); clear it otherwise.
    setKeyMeta(billingEnabled && !!workspaceId ? undefined : null);
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
        if (!cancelled && gen === switchGenRef.current) setScope(targetDefaultScope(target));
      });
    // Same for the authorship pin (#103). Off is the safe fallback: a chip that
    // wrongly reads "off" under-promises, where one that wrongly reads "on" would
    // claim a narrowing the turn isn't doing.
    ipc
      .chatGetOwnerFilter(noteId)
      .then((o) => {
        if (!cancelled && gen === switchGenRef.current) setOwnerFilter(o ?? "");
      })
      .catch(() => {
        if (!cancelled && gen === switchGenRef.current) setOwnerFilter("");
      });
    void reloadConversationList(gen);
    void refreshActivation(gen);
    return () => {
      cancelled = true;
      // Keep the anchor searchable on the way out. A library-wide pane has no
      // anchor to reindex — every note is reindexed at its own checkpoints.
      if (noteId !== null) void ipc.chatReindexNote(noteId).catch(() => {});
    };
  }, [noteId, workspaceId, billingEnabled, reloadConversationList, refreshActivation, resetTransient]);

  // Persist a breadth change to the conversation (issue #58): update the chip
  // optimistically, then write it through so the next turn reads the new value.
  //
  // On a DRAFTING target with no conversation open the chip is the whole record
  // (#120) — the write is skipped rather than lazily creating a row to hold it,
  // and `send` carries the value in instead. That lazy row is what used to be
  // hidden from the list yet still resolved to by the next send, so the pane could
  // show one setting while the stored row applied another.
  //
  // A Note with no session still writes through, which lazily creates it: that
  // pane RESUMES, so a breadth chosen before the first turn has to be stored
  // somewhere it will be found again. Mirrors `resolve_for_write` in Rust.
  function selectBreadth(next: ChatScope) {
    setScope(next);
    if (!conversationId && !targetResumesOnOpen(target)) return;
    void ipc.chatSetBreadth(noteId, conversationId, next).catch((e) => setError(String(e)));
  }

  // Pin the conversation to the caller's own notes, or clear the pin (#103).
  // Optimistic like the breadth write, and only ever offers MY id — the chip is
  // inert when someone else's pin is in force (see `authorPinEditable`).
  function toggleOwnerFilter() {
    const next = ownerFilter === "" ? (myUserId ?? "") : "";
    if (next === "" && ownerFilter === "") return;
    setOwnerFilter(next);
    // Same as breadth: nothing to write to on a draft, and the pin especially must
    // not be stored somewhere the chip can't see (#120/#103). A Note still writes
    // through and lazily creates, as it did before.
    if (!conversationId && !targetResumesOnOpen(target)) return;
    void ipc
      .chatSetOwnerFilter(noteId, conversationId, next === "" ? null : next)
      .catch((e) => setError(String(e)));
  }

  // Owner activated chat from the pane (#76): the entry reports fresh metadata →
  // flip to the composer WITHOUT a reload (BYOK is unmetered). Clear any prior
  // activation error so the live pane starts clean.
  function handleActivated(m: ChatKeyMeta) {
    setKeyMeta(m);
    setUsage(null);
    setError(null);
    setErrorReason(null);
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
      if (sendingRef.current) {
        setError(e.message);
        setErrorReason(e.reason ?? null);
      }
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
    // Only "consume" the one-per-mount focus once the composer actually mounted
    // (inputRef present) — otherwise the activation pane / loading state would
    // eat it and the composer would never get focused when it appears (#76).
    if (!hasAutoFocusedRef.current && inputRef.current) {
      hasAutoFocusedRef.current = true;
      inputRef.current.focus();
    }
  }, [paneUsable, notActivated, activationLoading]);

  // Grow the composer with the text. `rows={1}` sets the floor and `max-h-40` the
  // ceiling, but nothing in between: without this a wrapped question scrolls
  // inside a one-line box, so the user can't see what they're about to send.
  // Height has to be measured, hence an effect rather than CSS — `field-sizing`
  // isn't available in this webview. Reset to `auto` first so the box shrinks back
  // when text is deleted or cleared after a send; `max-height` still caps it, and
  // the textarea scrolls past that.
  //
  // Not unit-tested on purpose: jsdom reports `scrollHeight` as 0, so any
  // assertion here would pass for the wrong reason.
  useEffect(() => {
    const el = inputRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight}px`;
  }, [input]);

  // Publish the session controls to the owner (Note header) whenever the list,
  // the active conversation, or its emptiness changes. Only while ready — no
  // chat chrome before a provider is configured. `null` tells the header to
  // render nothing. Actions are stable, so this fires only on real data changes.
  useEffect(() => {
    if (!onControls) return;
    // Publish whenever the chat UI is usable — including an unactivated workspace,
    // so history (popover) stays reachable during the cutover (#76).
    if (!paneUsable) {
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
      targetKey: paneKey,
      conversations,
      activeConversationId: conversationId,
      canBrowseHistory,
      hasMore: hasMoreConversations,
      loadingMore: loadingMoreConversations,
      newChat,
      openConversation,
      deleteConversation,
      renameConversation,
      loadMore: loadMoreConversations,
      status: inWorkspace || !model ? null : { provider, model },
    });
  }, [
    onControls,
    paneUsable,
    paneKey,
    conversations,
    conversationId,
    messages.length,
    hasMoreConversations,
    loadingMoreConversations,
    newChat,
    openConversation,
    deleteConversation,
    renameConversation,
    loadMoreConversations,
    inWorkspace,
    provider,
    model,
  ]);

  async function send() {
    const text = input.trim();
    // Mirror the render gate, NOT personal readiness (#76): a workspace send runs
    // on the workspace key, so it must not require a personal provider key — an
    // owner who typed the key or any member can send in an activated workspace.
    if (!text || sending || !paneUsable) return;
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
      // The pinned author's NAME rides the turn for the prompt's disclosure line
      // only — the id lives on the conversation row and is what filters, so an
      // unresolvable name costs wording and nothing else (#103).
      // A drafting pane has persisted nothing, so its chips are the only record of
      // the breadth and pin the user chose — they ride along and are applied to the
      // row this turn creates (#120). With a conversation already open they're
      // omitted: the row is the source of truth and re-sending them could only
      // disagree with it.
      const result = await ipc.chatSend(
        noteId,
        conversationId,
        text,
        pinnedAuthorName,
        conversationId ? null : { breadth: scope, ownerFilter: ownerFilter || null },
      );
      if (gen !== switchGenRef.current) return;
      setConversationId(result.conversationId);
      const h = await ipc.chatHistory(noteId, result.conversationId);
      if (gen !== switchGenRef.current) return;
      setMessages(h.messages);
      setTruncated(result.truncated);
      // A first turn creates the conversation and bumps message counts — refresh
      // the list so the history popover + its visibility rule stay current (#62).
      void reloadConversationList(gen);
      // A completed turn consumes allowance — refresh the meter (#69).
      void refreshActivation(gen);
    } catch (e) {
      if (gen !== switchGenRef.current) return;
      setError((prev) => prev ?? String(e));
      // Authoritative activation fallback (#76): a send can fail with
      // chat_not_activated if the proactive check was stale — re-detect so the
      // pane flips in. (byok_* failures leave the key configured, so this stays
      // on the live pane and the error copy shows instead.)
      void refreshActivation(gen);
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

  // Stop the streaming turn (#80). Fire-and-forget: the command is a no-op when
  // nothing is in flight, and `sending` is cleared by the send's own completion
  // path — the backend still finishes the turn by persisting whatever streamed.
  const cancel = useCallback(() => {
    void ipc.chatCancel(noteId).catch(() => {});
  }, [noteId]);

  function onKeyDown(e: React.KeyboardEvent<HTMLTextAreaElement>) {
    // "/" on an empty composer opens the prompt picker; mid-sentence it stays a
    // literal slash (see opensPromptPicker).
    if (!sending && opensPromptPicker(e.key, input)) {
      e.preventDefault();
      setPromptsOpen(true);
      return;
    }
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      // Mid-turn, Enter stops rather than queueing a second turn.
      if (sending) cancel();
      else void send();
    }
  }

  // Fill the composer from a picked prompt and hand focus back, so the user can
  // edit before sending rather than firing blind. Not a hook — deliberately not
  // named `use*`.
  function applyPrompt(p: ChatPrompt) {
    setPromptsOpen(false);
    setInput(p.prompt);
    inputRef.current?.focus();
  }

  // Personal chat gates on personal readiness; workspace chat never shows this
  // setup prompt (it runs on the workspace key, activated via the pane below).
  if (!inWorkspace && !readinessLoading && !ready) {
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

  // Role-aware BYOK error copy when we have the reason; else the server message.
  const errorView = liveChatErrorCopy(errorReason, { isOwner, ownerName }) ?? error;

  return (
    <div className="flex-1 min-h-0 flex flex-col">
      {/* Workspace context signal (#63): nothing in Personal; one muted line in
          a workspace. It sits OUTSIDE the scroll container as a fixed flex item,
          so it stays visible as a privacy signal while messages scroll beneath
          it. It must own its vertical space with a bottom hairline, or scrolled
          bubbles butt straight up against the text with no separation. No
          breadth/tenant chrome here — breadth moved to the composer, and the
          tenant is pinned to the loaded workspace (#58). */}
      {workspaceName && !onPage && (
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
        className={cn("nd-scroll-hidden flex-1 min-h-0 overflow-y-auto py-4 flex flex-col gap-3", gutter)}
      >
        {messages.length === 0 && !sending ? (
          <div
            className={cn(
              "flex-1 flex flex-col items-center justify-center gap-3 text-center",
              onPage ? "gap-5" : "px-6",
            )}
          >
            <MessageCircle size={22} strokeWidth={1.5} className="text-[var(--color-text-disabled)]" />
            <p className="text-sm text-[var(--color-text-muted)]">
              {notesStillArriving
                ? "Still syncing your notes — this will be ready in a moment."
                : nothingToSearch
                  ? "No notes yet. Record or import a meeting, then ask about it here."
                  : "Ask anything about your notes — it searches, reads, and cites them to answer."}
            </p>
            {/* The same prompts the "/" menu offers (#80), shown up front on a new
                chat — a blank page tells a first-time user nothing about what this
                can do, and on a library-wide surface the useful questions are the
                least guessable ones. Cards only in the page variant: the Note's
                context panel can be 320px wide, where a grid of them would be
                unreadable, and its "/" menu is a keystroke away regardless.
                Suppressed when there's nothing to retrieve — offering four
                questions with the same dead-end answer would be a tease. */}
            {onPage && !composerHeld && (
              <>
                <PromptCards prompts={promptsFor(target)} onPick={applyPrompt} />
                {/* The "/" menu has been reachable since #80 and mentioned
                    nowhere, so nobody who didn't read the changelog knows it
                    exists. The new-chat screen is where it's worth saying: the
                    cards are right there to give "these" a referent. */}
                <p className="text-xs text-[var(--color-text-disabled)]">
                  Type <span className="font-medium">/</span> in the composer for these any time.
                </p>
              </>
            )}
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

      {!notActivated && errorView && (
        <div
          className={cn(
            "mb-2 flex items-start gap-2 rounded-[var(--radius)] bg-[var(--color-accent-soft)] px-3 py-2 text-xs text-[var(--color-accent-text)]",
            onPage ? "" : "mx-4",
          )}
        >
          <AlertTriangle size={13} strokeWidth={1.7} className="mt-px shrink-0" />
          <span>{errorView}</span>
        </div>
      )}
      {/* Authoritative post-turn truncation report. Kept even when the
          composer's pre-emptive hint is showing: the hint is an estimate and a
          warning ("may omit"), this is the confirmation that it actually
          happened. Suppressing it would leave the user unsure which. */}
      {truncated && !error && (
        <div className={cn("mb-2 text-xs text-[var(--color-text-muted)]", onPage ? "" : "mx-4")}>
          Note content was truncated to fit the context budget — the answer may miss details near
          the end.
        </div>
      )}

      {activationLoading ? (
        <div
          className={cn(
            "shrink-0 p-4 text-sm text-[var(--color-text-muted)]",
            onPage ? "" : "border-t border-[var(--color-line)]",
          )}
        >
          Checking chat activation…
        </div>
      ) : notActivated ? (
        <ActivationPane
          // Remount on workspace switch so the entry draft is destroyed
          // synchronously — belt-and-suspenders draft isolation (#76).
          key={workspaceId as string}
          workspaceId={workspaceId as string}
          isOwner={isOwner}
          ownerName={ownerName}
          addon={addon}
          onActivated={handleActivated}
        />
      ) : (
      <div
        className={cn(
          "relative shrink-0 p-2.5 flex flex-col gap-1.5",
          // On the page the composer is its own rounded box — the Codex/Claude
          // Desktop shape — instead of a hairline drawn across the whole pane.
          // A box reads as "type here"; a separator just divides two empty areas.
          onPage
            ? "rounded-[var(--radius-card)] border border-[var(--color-line-visible)] bg-[var(--color-surface)] transition-colors focus-within:border-[var(--color-text-muted)]"
            : "border-t border-[var(--color-line)]",
        )}
      >
        {/* Prompt picker (#80). Rendered in-flow above the composer rather than
            through SelectablePopover: that component is a click-to-select value
            picker with no controlled-open and no arrow-key nav, and bending it
            into a keyboard command menu would have put the Client and Breadth
            pickers at risk. The composer is pinned to the bottom of a
            fixed-width panel, so this needs no portal or flip logic either. */}
        {promptsOpen && (
          <PromptPicker
            prompts={promptsFor(target)}
            onPick={applyPrompt}
            onDismiss={() => {
              setPromptsOpen(false);
              inputRef.current?.focus();
            }}
          />
        )}
        {/* Send button lives INSIDE the input (Claude-Desktop style): the input
            spans the full composer width, and the button is absolutely pinned to
            its right edge. The textarea is `block` so it has no inline-block
            baseline gap — the relative wrapper's height then equals the visible
            input's height, and `top-1/2 -translate-y-1/2` centres the button
            against the true input height (no pixel nudging). The small icon-button
            variant (28px) fits comfortably inside without touching the edges.
            The textarea has no native outline, so a token-based focus-within
            border on the wrapper makes keyboard focus visible (#64). */}
        <div
          className={cn(
            "relative rounded-[var(--radius)]",
            // The page variant hoists this focus treatment out to the composer
            // box, so the two don't nest into a double border.
            onPage
              ? ""
              : "border border-transparent transition-colors focus-within:border-[var(--color-text-muted)]",
          )}
        >
          <textarea
            ref={inputRef}
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={onKeyDown}
            rows={1}
            // Stays enabled while a turn streams so Enter can stop it (#80) —
            // `send` no-ops on `sending`, so this can't queue a second turn.
            //
            // Nothing to retrieve from is the one case that closes the composer
            // (#95). `readOnly` + `aria-disabled` rather than `disabled`, because
            // `disabled` drops the control out of the tab order — a keyboard or
            // screen-reader user would then never reach the placeholder saying
            // WHY they can't type. Nothing can be entered either way, so `send`
            // stays unreachable: it no-ops on empty input.
            readOnly={composerHeld}
            aria-disabled={composerHeld || undefined}
            placeholder={
              notesStillArriving
                ? "Syncing your notes…"
                : nothingToSearch
                  ? "Nothing to search yet"
                  : sending
                    ? "Streaming — press Enter to stop"
                    : "Ask about your notes…"
            }
            aria-label="Ask about your notes"
            // The global `select, textarea` rule in globals.css is UNLAYERED, so it
            // outranks every utility here and hands this field a border, a fill and
            // 8px/12px padding whether we ask or not. In the PANEL that lands inside
            // the wrapper's own transparent border and has been the Note tab's look
            // since #46 — so leave it exactly alone. On the PAGE the composer box is
            // the surface, so the field opts out through a rule of the same kind
            // (`.nd-chat-input`), which also owns its padding.
            className={cn(
              "block w-full resize-none max-h-40 text-sm leading-relaxed outline-none placeholder:text-[var(--color-text-muted)] read-only:cursor-not-allowed",
              onPage ? "nd-chat-input" : "bg-transparent pl-2 pr-11 py-2",
            )}
          />
          {/* Send morphs into Stop while streaming (#80), so the same spot is
              always the turn's primary control. */}
          {sending ? (
            <button
              type="button"
              onClick={cancel}
              title="Stop"
              aria-label="Stop generating"
              className="nd-btn-icon nd-btn-icon-sm absolute right-1.5 top-1/2 -translate-y-1/2"
            >
              <Square size={13} strokeWidth={2} fill="currentColor" />
            </button>
          ) : (
            <button
              type="button"
              onClick={() => void send()}
              disabled={input.trim().length === 0}
              title="Send"
              aria-label="Send"
              className={cn(
                "nd-btn-icon nd-btn-icon-sm absolute right-1.5 top-1/2 -translate-y-1/2",
                input.trim().length === 0 && "opacity-40 pointer-events-none",
              )}
            >
              <CornerDownLeft size={16} strokeWidth={1.7} />
            </button>
          )}
        </div>
        {/* Composer control row: breadth picker bottom-left, workspace turn
            allowance bottom-right (#69). The meter shows only in a metered
            workspace — `usage` is null in personal context and on any
            unavailable/error/unmetered outcome, so nothing renders then.
            Skipped entirely when every one of its children would be absent, which
            on `/chat` in Personal is all of them: an empty flex row still costs
            the container's gap, and that showed up as an unexplained band under
            the composer that looked like space reserved for something. */}
        {showComposerControls && (
        <div className="flex items-center justify-between gap-2 px-1">
          <div className="flex min-w-0 items-center gap-2">
            <BreadthPicker
              scope={scope}
              onScope={selectBreadth}
              folderName={folder?.name ?? null}
              hasAnchor={noteId !== null}
              fallbackScope={targetDefaultScope(target)}
            />
            {showAuthorPin && (
              <AuthorPin
                pinned={ownerFilter !== ""}
                isMine={authorPinIsMine}
                name={pinnedAuthorName}
                editable={authorPinEditable}
                onToggle={toggleOwnerFilter}
              />
            )}
            {/* Which model is about to answer (#80) — previously invisible
                without opening Settings. Muted, not disabled: --color-text-
                disabled fails contrast on interactive text (see #65).
                Personal only: a workspace turn runs on the server's model, and
                `model` here is the LOCAL chat_model setting, so showing it in a
                workspace would name a model that isn't answering.
                Not on the page, where it's published upward and shown in the
                header's pill row — the same fact twice on one screen is clutter,
                and the header is where that surface keeps its identity info. */}
            {!onPage && !inWorkspace && model && (
              <span
                data-testid="chat-model-indicator"
                title={`Answering with ${model} (${provider}) — change it in Settings → Chat`}
                className="truncate text-xs text-[var(--color-text-muted)]"
              >
                {model}
              </span>
            )}
            {/* Truncation warned BEFORE the turn, not only after it (#80). An
                estimate, hence "may" — the post-turn banner stays authoritative. */}
            {likelyTruncated && (
              <span
                data-testid="chat-truncation-hint"
                title="Long note — the reference block is trimmed to fit the context budget, so detail near the end may be missing."
                className="shrink-0 text-xs text-[var(--color-text-muted)]"
              >
                · may omit part of this note
              </span>
            )}
          </div>
          {usage && (
            <span
              className={cn(
                "text-xs tabular-nums",
                // Colour-only status by fraction consumed (#69) — AAA on the
                // panel surface in both themes. Size/weight unchanged.
                {
                  default: "text-[var(--color-text-muted)]",
                  warning: "text-[var(--color-status-warning)]",
                  danger: "text-[var(--color-status-danger)]",
                }[usageTone(usage.used, usage.cap)],
              )}
            >
              {usage.used}/{usage.cap} turns
            </span>
          )}
        </div>
        )}
      </div>
      )}
    </div>
  );
}

// Role-aware activation pane for an unactivated workspace (#76). Replaces the
// composer while history stays readable above. Owner: a values-first line + the
// shared key entry (+ the managed add-on as the alternative when the server
// offers it). Member: nothing actionable, just who to ask. The privacy line
// respects the PRD claim boundary — "your OpenAI relationship", not "we can't
// see your data".
function ActivationPane({
  workspaceId,
  isOwner,
  ownerName,
  addon,
  onActivated,
}: {
  workspaceId: string;
  isOwner: boolean;
  ownerName: string;
  addon: ChatAddon | null;
  onActivated: (meta: ChatKeyMeta) => void;
}) {
  return (
    <div className="shrink-0 border-t border-[var(--color-line)] p-4 flex flex-col gap-3">
      {isOwner ? (
        <>
          <p className="text-sm leading-relaxed">
            Turn on chat for this workspace with your own OpenAI key — your team's chat runs on your
            OpenAI relationship, free and unmetered.
          </p>
          <ChatKeyEntry workspaceId={workspaceId} onActivated={onActivated} />
          {addon?.available && (
            <p className="text-xs text-[var(--color-text-muted)] leading-relaxed">
              Prefer not to manage a key? The managed add-on
              {addon.price_cents != null &&
                ` (${formatSeatPrice(addon.price_cents, addon.currency)}/mo)`}{" "}
              runs chat on Humla's key — set it up in Organization → Workspace chat.
            </p>
          )}
        </>
      ) : (
        <p className="text-sm text-[var(--color-text-muted)]">
          Chat isn't activated for this workspace yet — ask {ownerName}.
        </p>
      )}
    </div>
  );
}

// The "/" prompt menu (#80). A keyboard-first command menu: arrow keys move,
// Enter picks, Escape dismisses. Deliberately NOT SelectablePopover — that's a
// click-to-select value picker with internal open state and no key nav; see the
// call site for why sharing it would have been the wrong trade.
// The prompt set as cards, for a new chat on the `/chat` page (issue #95, after
// the Codex-style new-chat screen). Same prompts and same `onPick` as the "/"
// menu — this is a second surface for one list, not a second list.
//
// Picking one FILLS the composer rather than sending it, exactly as the menu
// does: these are starting points, and a card that spends a turn (a metered one,
// in a workspace) on a question you hadn't finished thinking about would be a
// trap. The user can edit and press Enter.
function PromptCards({
  prompts,
  onPick,
}: {
  prompts: ChatPrompt[];
  onPick: (p: ChatPrompt) => void;
}) {
  return (
    <ul className="grid w-full max-w-[520px] grid-cols-2 gap-2 list-none">
      {prompts.map((p) => (
        <li key={p.label}>
          <button
            type="button"
            onClick={() => onPick(p)}
            className="h-full w-full rounded-[var(--radius-card)] border border-[var(--color-line)] bg-[var(--color-surface)] px-3 py-2.5 text-left transition-colors hover:border-[var(--color-line-visible)] hover:bg-[var(--color-pill-hover)]"
          >
            <span className="block text-[13px] font-medium text-[var(--color-text)]">{p.label}</span>
            <span className="mt-0.5 block text-xs leading-snug text-[var(--color-text-muted)]">
              {p.description}
            </span>
          </button>
        </li>
      ))}
    </ul>
  );
}

function PromptPicker({
  prompts,
  onPick,
  onDismiss,
}: {
  prompts: ChatPrompt[];
  onPick: (p: ChatPrompt) => void;
  onDismiss: () => void;
}) {
  const [active, setActive] = useState(0);
  const listRef = useRef<HTMLDivElement | null>(null);

  // Own the keys while open. Bound on the list (which takes focus) so the
  // composer's own handler doesn't also see them.
  useEffect(() => {
    listRef.current?.focus();
  }, []);

  function onKeyDown(e: React.KeyboardEvent<HTMLDivElement>) {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setActive((i) => (i + 1) % prompts.length);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActive((i) => (i - 1 + prompts.length) % prompts.length);
    } else if (e.key === "Enter") {
      e.preventDefault();
      onPick(prompts[active]);
    } else if (e.key === "Escape") {
      e.preventDefault();
      onDismiss();
    }
  }

  return (
    <div
      ref={listRef}
      role="listbox"
      aria-label="Prompts"
      // Focus sits on the list, so the active row is announced via
      // activedescendant rather than by moving focus between options.
      aria-activedescendant={`chat-prompt-${active}`}
      tabIndex={-1}
      onKeyDown={onKeyDown}
      onBlur={onDismiss}
      className="absolute inset-x-2.5 bottom-full z-20 mb-1.5 overflow-hidden rounded-[var(--radius-card)] border border-[var(--color-line-visible)] bg-[var(--color-surface-raised)] p-1 shadow-[var(--shadow-md)] outline-none"
    >
      <div className="px-2 pb-1 pt-0.5 text-[11px] font-medium text-[var(--color-text-muted)]">
        Prompts
      </div>
      {prompts.map((p, i) => (
        <button
          key={p.label}
          id={`chat-prompt-${i}`}
          type="button"
          role="option"
          aria-selected={i === active}
          // mousedown, not click: the list's onBlur would dismiss before a click
          // resolved, so the pick would never land.
          onMouseDown={(e) => {
            e.preventDefault();
            onPick(p);
          }}
          onMouseEnter={() => setActive(i)}
          className={cn(
            "flex w-full flex-col items-start gap-0.5 rounded-[var(--radius)] px-2 py-1.5 text-left",
            i === active && "bg-[var(--color-pill-hover)]",
          )}
        >
          <span className="text-[13px] text-[var(--color-text)]">{p.label}</span>
          <span className="text-xs text-[var(--color-text-muted)]">{p.description}</span>
        </button>
      ))}
    </div>
  );
}

// The composer control row's shared resting look: quiet, sentence-case, muted.
// Both controls in the row wear it, so neither can drift into shouting past the
// other; each adds its own interaction and active states.
const QUIET_CONTROL = "inline-flex items-center gap-1 text-xs text-[var(--color-text-muted)] transition-colors";

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
  hasAnchor,
  fallbackScope,
}: {
  scope: ChatScope;
  onScope: (s: ChatScope) => void;
  folderName: string | null;
  /** False for a library-wide pane (#94), which has no anchor note — so neither
   *  "This note" nor "Folder: …" is offerable. The backend enforces the same rule
   *  (`chat::check_anchor`), so offering them would let the chip show a breadth
   *  the next write is guaranteed to reject. */
  hasAnchor: boolean;
  /** Where an unselectable or vanished breadth falls back to. */
  fallbackScope: ChatScope;
}) {
  // Per-scope icons so the options read at a glance (#69). Colour inherited.
  const items: PopoverItem[] = [
    ...(hasAnchor
      ? [
          {
            id: "note",
            label: "This note",
            icon: <FileText size={14} strokeWidth={1.7} aria-hidden="true" />,
          },
        ]
      : []),
    ...(hasAnchor && folderName
      ? [
          {
            id: "folder",
            label: `Folder: ${folderName}`,
            icon: <Folder size={14} strokeWidth={1.7} aria-hidden="true" />,
          },
        ]
      : []),
    { id: "all", label: "All notes", icon: <Files size={14} strokeWidth={1.7} aria-hidden="true" /> },
  ];
  // One option is not a choice: a picker whose only entry is "All notes" is noise,
  // so a library-wide pane shows no breadth chrome at all (#95). Narrowing is
  // still available there — as a tool argument the model chooses (#81), not as a
  // control. A pane WITH an anchor always has at least "This note" + "All notes",
  // so this never hides the picker where it does work.
  if (items.length < 2) return null;

  // If the folder disappears while "folder" is selected — or the pane has no
  // anchor at all — fall back to the pane's own default.
  const selectable = items.some((i) => i.id === scope);
  const activeId = selectable ? scope : fallbackScope;
  const active = items.find((i) => i.id === activeId);
  const label = active?.label ?? "All notes";

  return (
    <SelectablePopover
      ariaLabel="Chat scope"
      items={items}
      activeId={activeId}
      onSelect={(id) => onScope((id as ChatScope) ?? fallbackScope)}
      trigger={
        <span className={cn(QUIET_CONTROL, "cursor-pointer hover:text-[var(--color-text)]")}>
          {active?.icon}
          {label}
          <ChevronDown size={12} strokeWidth={2} aria-hidden="true" className="shrink-0" />
        </span>
      }
    />
  );
}

// The conversation's authorship filter (#103), beside the breadth picker: breadth
// says WHAT is in reach, this says WHOSE.
//
// "Created by me", not "My notes". `notes.owner` is who RECORDED a note, not who
// attended — so if a colleague records a meeting you were both in, this excludes
// it. In a shared workspace most people read "my notes" as "meetings I was in",
// and would get confidently wrong answers to exactly the questions this control
// exists for ("what did I commit to?"). Naming authorship keeps the false negative
// visible instead of silent. Attendance is #104 and is not attempted here.
//
// Styled to the row rather than as an `.nd-chip`: that utility is uppercase and
// letter-spaced, which would shout beside the quiet sentence-case breadth trigger
// and the muted model indicator. Off is muted text; ON is a filled accent-soft
// pill, because an active filter narrowing every answer must be impossible to
// miss — the same reason the turn discloses it to the model.
function AuthorPin({
  pinned,
  isMine,
  name,
  editable,
  onToggle,
}: {
  pinned: boolean;
  /** Pinned to the signed-in user. Resolved against their id, not their name. */
  isMine: boolean;
  /** The pinned person's display name, or null when it can't be resolved. */
  name: string | null;
  editable: boolean;
  onToggle: () => void;
}) {
  // Someone else's pin reads by name; an unresolvable one stays neutral rather
  // than guessing "me", which would misattribute a teammate's filter to the
  // reader. Off and pinned-to-me share a label, told apart by the fill — that is
  // what makes it read as one toggle rather than two states of a sentence.
  const label = !pinned || isMine ? "Created by me" : name ? `Created by ${name}` : "Created by someone else";
  const title = pinned
    ? editable
      ? "Only notes you recorded are searched. Meetings someone else recorded are excluded, even ones you attended."
      : `This conversation is pinned to notes recorded by ${name ?? "another member"}. Start your own conversation to search differently.`
    : "Search only the notes you recorded yourself.";
  return (
    <button
      type="button"
      data-testid="chat-author-pin"
      onClick={editable ? onToggle : undefined}
      // Inert, not hidden: the pin is changing every answer in the thread, so the
      // reader has to be able to see it — they just can't rewrite what someone
      // else's scrollback meant.
      disabled={!editable}
      aria-pressed={pinned}
      title={title}
      className={cn(
        QUIET_CONTROL,
        "shrink-0 rounded-full",
        pinned && "bg-[var(--color-accent-soft)] px-2 py-0.5 text-[var(--color-accent-text)]",
        editable ? "cursor-pointer hover:text-[var(--color-text)]" : "cursor-default",
      )}
    >
      <UserRound size={13} strokeWidth={1.7} aria-hidden="true" />
      {label}
    </button>
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
