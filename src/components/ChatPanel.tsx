import { memo, useEffect, useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { open as openExternal } from "@tauri-apps/plugin-shell";
import { AlertTriangle, MessageCircle, Send, Settings2 } from "lucide-react";
import {
  ipc,
  onChatError,
  onChatTextDelta,
  type ChatMessageDto,
} from "../lib/ipc";
import { useChatReadiness } from "./provider/useChatReadiness";
import { RECOMMENDED_OLLAMA_MODEL } from "../lib/localModels";
import { CommandSnippet } from "./CommandSnippet";
import { cn } from "../lib/cn";

// Chat with a single Note (issue #46). A message list + input where the user
// asks questions grounded in the current Note; the answer streams token-by-
// token (reusing the summary-streaming event style) and the conversation
// persists locally, reloading after restart. Personal scope only — one
// conversation per Note, created lazily on the backend on first send.

const Markdown = memo(function Markdown({ source }: { source: string }) {
  return <ReactMarkdown remarkPlugins={[remarkGfm]}>{source}</ReactMarkdown>;
});

function partsText(m: ChatMessageDto): string {
  return m.parts
    .filter((p) => p.type === "text")
    .map((p) => p.text)
    .join("");
}

export function ChatPanel({ noteId }: { noteId: string }) {
  const { loading: readinessLoading, ready, hint, provider, model } = useChatReadiness();
  const [messages, setMessages] = useState<ChatMessageDto[]>([]);
  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);
  const [streaming, setStreaming] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [truncated, setTruncated] = useState(false);
  const scrollRef = useRef<HTMLDivElement | null>(null);
  // A ref so the (once-bound) event listeners can gate on "is a send in
  // flight" without re-subscribing every time `sending` flips.
  const sendingRef = useRef(false);

  // Load persisted history on mount / note change, and clear transient state.
  useEffect(() => {
    let cancelled = false;
    setInput("");
    setStreaming("");
    setError(null);
    setTruncated(false);
    setSending(false);
    sendingRef.current = false;
    ipc
      .chatHistory(noteId)
      .then((h) => {
        if (!cancelled) setMessages(h);
      })
      .catch(() => {
        if (!cancelled) setMessages([]);
      });
    return () => {
      cancelled = true;
    };
  }, [noteId]);

  // Stream subscription. Bound once; the cancelled-flag + claim pattern keeps
  // it StrictMode- and async-listen-safe (mirrors the summary streaming in
  // Note.tsx). Deltas are accepted only while our own send is in flight.
  useEffect(() => {
    let cancelled = false;
    const unsubs: (() => void)[] = [];
    const claim = (u: () => void) => {
      if (cancelled) u();
      else unsubs.push(u);
    };
    onChatTextDelta((e) => {
      if (sendingRef.current) setStreaming((s) => s + e.delta);
    }).then(claim);
    onChatError((e) => {
      if (sendingRef.current) setError(e.message);
    }).then(claim);
    return () => {
      cancelled = true;
      unsubs.forEach((u) => u());
    };
  }, []);

  // Keep the newest content in view.
  useEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [messages, streaming, sending]);

  async function send() {
    const text = input.trim();
    if (!text || sending || !ready) return;
    setInput("");
    setError(null);
    setStreaming("");
    setTruncated(false);
    setSending(true);
    sendingRef.current = true;
    // Optimistic user bubble so the message shows instantly; replaced by the
    // authoritative history on completion.
    const optimistic: ChatMessageDto = {
      id: `optimistic-${Date.now()}`,
      role: "user",
      seq: -1,
      parts: [{ type: "text", id: "optimistic", text }],
      createdAt: Date.now(),
    };
    setMessages((m) => [...m, optimistic]);
    try {
      const result = await ipc.chatSend(noteId, text);
      setMessages(await ipc.chatHistory(noteId));
      setTruncated(result.truncated);
    } catch (e) {
      // The backend rolls back a failed assistant turn; reload reflects the
      // truth (the user turn persists on a stream error, nothing on a config
      // error). Only surface a message here if the chat_error event didn't.
      setError((prev) => prev ?? String(e));
      try {
        setMessages(await ipc.chatHistory(noteId));
      } catch {
        /* keep the optimistic view */
      }
    } finally {
      setStreaming("");
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

  // Not yet configured → the readiness/setup prompt from #44: say exactly
  // what's missing, and for Ollama surface the same install link + copy-pull
  // affordances the Settings tab uses. Key entry stays in Settings (sensitive
  // + shared across features), so OpenAI shows a pointer there.
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

  const showTyping = sending && streaming.length === 0;

  return (
    <div className="flex-1 min-h-0 flex flex-col">
      <div ref={scrollRef} className="flex-1 min-h-0 overflow-y-auto px-4 py-4 flex flex-col gap-3">
        {messages.length === 0 && !sending ? (
          <div className="flex-1 flex flex-col items-center justify-center gap-3 px-6 text-center">
            <MessageCircle size={22} strokeWidth={1.5} className="text-[var(--color-text-disabled)]" />
            <p className="text-sm text-[var(--color-text-muted)]">
              Ask anything about this note — it answers from your notes, transcript, and summary.
            </p>
          </div>
        ) : (
          messages.map((m) => <Bubble key={m.id} role={m.role} text={partsText(m)} />)
        )}
        {sending && streaming.length > 0 && <Bubble role="assistant" text={streaming} />}
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
          placeholder="Ask about this note…"
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

function Bubble({ role, text }: { role: "user" | "assistant"; text: string }) {
  const isUser = role === "user";
  return (
    <div className={cn("flex", isUser ? "justify-end" : "justify-start")}>
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
    </div>
  );
}
