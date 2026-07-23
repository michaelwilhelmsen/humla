import { memo, useEffect, useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { useNavigate } from "react-router-dom";
import { open as openExternal } from "@tauri-apps/plugin-shell";
import { AlertTriangle, FileText, Loader2, MessageCircle, Send, Settings2 } from "lucide-react";
import {
  ipc,
  onChatCitations,
  onChatError,
  onChatToolActivity,
  onChatTextDelta,
  type ChatCitation,
  type ChatMessageDto,
  type ChatScope,
  type ChatTenant,
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

export function ChatPanel({ noteId }: { noteId: string }) {
  const { loading: readinessLoading, ready, hint, provider, model } = useChatReadiness();
  const [messages, setMessages] = useState<ChatMessageDto[]>([]);
  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);
  const [streaming, setStreaming] = useState("");
  const [activity, setActivity] = useState<string | null>(null);
  const [liveCitations, setLiveCitations] = useState<ChatCitation[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [truncated, setTruncated] = useState(false);
  const [scope, setScope] = useState<ChatScope>("note");
  const [tenant, setTenant] = useState<ChatTenant>("personal");
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const sendingRef = useRef(false);

  // The anchor note's folder drives the "this folder" scope option.
  const folder = useNotesStore((s) => {
    const note = s.notes.find((n) => n.id === noteId);
    const fid = note?.folder_id;
    return fid ? s.folders.find((f) => f.id === fid) ?? null : null;
  });

  // Team (workspace) chat is offered only when signed into a cloud workspace
  // (issue #50). The tenant row can never name a *different* workspace than the
  // active one — so a conversation can't cross tenants (story 5/19).
  const workspaceName = useCloudStore((s) =>
    s.status.logged_in ? s.status.current_workspace?.name ?? null : null,
  );

  // Load persisted history on mount / note change, and clear transient state.
  // On unmount / note change, rebuild the note's retrieval index so edits made
  // in this session are searchable next time (content-settled checkpoint).
  useEffect(() => {
    let cancelled = false;
    setInput("");
    setStreaming("");
    setActivity(null);
    setLiveCitations([]);
    setError(null);
    setTruncated(false);
    setSending(false);
    setScope("note");
    setTenant("personal");
    sendingRef.current = false;
    ipc
      .chatHistory(noteId, "personal")
      .then((h) => {
        if (!cancelled) setMessages(h);
      })
      .catch(() => {
        if (!cancelled) setMessages([]);
      });
    return () => {
      cancelled = true;
      void ipc.chatReindexNote(noteId).catch(() => {});
    };
  }, [noteId]);

  // If the workspace goes away (sign-out / switch) while a Team conversation is
  // open, fall back to Personal so we never send to a stale tenant.
  useEffect(() => {
    if (tenant === "workspace" && !workspaceName) changeTenant("personal");
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [workspaceName]);

  // Switching tenant starts a fresh conversation in that tenant (issue #50):
  // reset transient state and read-through that tenant's history.
  function changeTenant(next: ChatTenant) {
    if (next === tenant) return;
    setTenant(next);
    setStreaming("");
    setActivity(null);
    setLiveCitations([]);
    setError(null);
    setTruncated(false);
    setInput("");
    setScope("note");
    setSending(false);
    sendingRef.current = false;
    ipc
      .chatHistory(noteId, next)
      .then(setMessages)
      .catch(() => setMessages([]));
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
    const optimistic: ChatMessageDto = {
      id: `optimistic-${Date.now()}`,
      role: "user",
      seq: -1,
      parts: [{ type: "text", id: "optimistic", text }],
      createdAt: Date.now(),
    };
    setMessages((m) => [...m, optimistic]);
    try {
      const result = await ipc.chatSend(noteId, text, scope, tenant);
      setMessages(await ipc.chatHistory(noteId, tenant));
      setTruncated(result.truncated);
    } catch (e) {
      setError((prev) => prev ?? String(e));
      try {
        setMessages(await ipc.chatHistory(noteId, tenant));
      } catch {
        /* keep the optimistic view */
      }
    } finally {
      setStreaming("");
      setActivity(null);
      setSending(false);
      sendingRef.current = false;
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
                className="underline hover:text-[var(--color-text)]"
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
      <ScopeBar
        scope={scope}
        onScope={setScope}
        folderName={folder?.name ?? null}
        tenant={tenant}
        onTenant={changeTenant}
        workspaceName={workspaceName}
      />

      <div ref={scrollRef} className="flex-1 min-h-0 overflow-y-auto px-4 py-4 flex flex-col gap-3">
        {messages.length === 0 && !sending ? (
          <div className="flex-1 flex flex-col items-center justify-center gap-3 px-6 text-center">
            <MessageCircle size={22} strokeWidth={1.5} className="text-[var(--color-text-disabled)]" />
            <p className="text-sm text-[var(--color-text-muted)]">
              Ask anything about your notes — it searches, reads, and cites them to answer.
            </p>
          </div>
        ) : (
          messages.map((m) => (
            <Bubble key={m.id} role={m.role} text={partsText(m)} citations={messageCitations(m)} />
          ))
        )}
        {sending && streaming.length > 0 && (
          <Bubble role="assistant" text={streaming} citations={liveCitations} />
        )}
        {activity && (
          <div className="self-start flex items-center gap-1.5 text-xs text-[var(--color-text-muted)] px-1">
            <Loader2 size={12} strokeWidth={2} className="animate-spin" />
            {activity}
          </div>
        )}
        {showTyping && (
          <div className="self-start text-xs text-[var(--color-text-muted)] px-1">Thinking…</div>
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

      <div className="shrink-0 border-t border-[var(--color-line)] p-2.5 flex items-end gap-2">
        <textarea
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={onKeyDown}
          rows={1}
          placeholder="Ask about your notes…"
          disabled={sending}
          className="flex-1 resize-none max-h-40 bg-transparent text-sm leading-relaxed px-2 py-1.5 outline-none placeholder:text-[var(--color-text-disabled)] disabled:opacity-60"
        />
        <button
          type="button"
          onClick={() => void send()}
          disabled={sending || input.trim().length === 0}
          title="Send"
          aria-label="Send"
          className={cn(
            "nd-btn-icon shrink-0",
            (sending || input.trim().length === 0) && "opacity-40 pointer-events-none",
          )}
        >
          <Send size={16} strokeWidth={1.7} />
        </button>
      </div>
    </div>
  );
}

// Tenant + breadth selectors at the top of the chat. The tenant row (Personal /
// the active workspace) only appears when signed into a workspace (issue #50);
// "This folder" only appears when the note has a folder (nothing to widen to
// otherwise).
function ScopeBar({
  scope,
  onScope,
  folderName,
  tenant,
  onTenant,
  workspaceName,
}: {
  scope: ChatScope;
  onScope: (s: ChatScope) => void;
  folderName: string | null;
  tenant: ChatTenant;
  onTenant: (t: ChatTenant) => void;
  workspaceName: string | null;
}) {
  const items: PopoverItem[] = [
    { id: "note", label: "This note" },
    ...(folderName ? [{ id: "folder", label: `Folder: ${folderName}` }] : []),
    { id: "all", label: "All notes" },
  ];
  // If the folder disappears while "folder" is selected, fall back to "note".
  const activeId = scope === "folder" && !folderName ? "note" : scope;
  const label = items.find((i) => i.id === activeId)?.label ?? "This note";

  const tenantItems: PopoverItem[] = [
    { id: "personal", label: "Personal" },
    ...(workspaceName ? [{ id: "workspace", label: workspaceName }] : []),
  ];
  const activeTenant = tenant === "workspace" && !workspaceName ? "personal" : tenant;
  const tenantLabel =
    tenantItems.find((i) => i.id === activeTenant)?.label ?? "Personal";

  return (
    <div className="shrink-0 flex items-center gap-2 px-4 py-2 border-b border-[var(--color-line)]">
      {workspaceName && (
        <>
          <SelectablePopover
            ariaLabel="Chat tenant"
            items={tenantItems}
            activeId={activeTenant}
            onSelect={(id) => onTenant((id as ChatTenant) ?? "personal")}
            trigger={
              <span className="nd-chip inline-flex items-center gap-1 text-xs cursor-pointer">
                {tenantLabel}
              </span>
            }
          />
          <span className="text-xs text-[var(--color-text-disabled)]">·</span>
        </>
      )}
      <span className="text-xs text-[var(--color-text-disabled)]">Search</span>
      <SelectablePopover
        ariaLabel="Chat scope"
        items={items}
        activeId={activeId}
        onSelect={(id) => onScope((id as ChatScope) ?? "note")}
        trigger={
          <span className="nd-chip inline-flex items-center gap-1 text-xs cursor-pointer">
            {label}
          </span>
        }
      />
    </div>
  );
}

function Bubble({
  role,
  text,
  citations,
}: {
  role: "user" | "assistant";
  text: string;
  citations: ChatCitation[];
}) {
  const isUser = role === "user";
  return (
    <div className={cn("flex flex-col", isUser ? "items-end" : "items-start")}>
      <div
        className={cn(
          "max-w-[85%] rounded-[var(--radius-card)] px-3 py-2 text-sm leading-relaxed",
          isUser
            ? "bg-[var(--color-accent-soft)] text-[var(--color-accent-text)]"
            : "bg-[var(--color-pill-hover)] text-[var(--color-text)]",
        )}
      >
        {isUser ? (
          <span className="whitespace-pre-wrap">{text}</span>
        ) : (
          <div className="prose-summary">
            <Markdown source={text} />
          </div>
        )}
      </div>
      {!isUser && citations.length > 0 && (
        <div className="mt-1.5 flex flex-wrap gap-1.5 max-w-[85%]">
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
      className="nd-chip inline-flex items-center gap-1 text-xs cursor-pointer hover:text-[var(--color-text)]"
    >
      <FileText size={11} strokeWidth={1.7} className="shrink-0" />
      <span className="truncate max-w-[16rem]">{title}</span>
    </button>
  );
}
