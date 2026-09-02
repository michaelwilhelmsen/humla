import { useCallback, useEffect, useState } from "react";
import { open as openExternal } from "@tauri-apps/plugin-shell";
import { Row, Section } from "../components/Section";
import { s } from "../components/format";
import { Select } from "../../../components/ui/Select";
import { OllamaConnect } from "../../../components/provider/OllamaConnect";
import { ProviderKeyCard } from "../../../components/provider/ProviderKeyCard";
import { CommandSnippet } from "../../../components/CommandSnippet";
import { useOllamaProbe } from "../../../components/provider/useOllamaProbe";
import { localChatHint } from "../../../components/provider/useChatReadiness";
import { useEmbedProbe } from "../../../components/provider/useEmbedProbe";
import { useProviderKey } from "../../../components/provider/useProviderKey";
import { CHAT_PROVIDERS, SUMMARY_MODELS } from "../types";
import {
  EMBEDDING_OLLAMA_MODEL,
  RECOMMENDED_OLLAMA_MODEL,
  isOllamaUrl,
} from "../../../lib/localModels";
import { ipc } from "../../../lib/ipc";
import type { SettingsHook } from "../useSettings";

// AI Chat provider setting (issue #44). A dedicated provider choice, separate
// from transcription/summary: OpenAI (cloud, shared key) or Ollama (local).
// No chat behaviour yet — just the choice, its persistence, and a readiness
// prompt saying exactly what's still missing. The embedding model is
// auto-derived (text-embedding-3-small / embeddinggemma) and not surfaced here;
// its setup lands with semantic retrieval.
export function ChatTab({ s, update }: Pick<SettingsHook, "s" | "update">) {
  const isOllama = s.chat_provider === "ollama";
  // Both hooks run unconditionally (rules of hooks); the probe parks itself
  // when chat isn't on Ollama.
  const key = useProviderKey("openai");
  const { reachable, installed } = useOllamaProbe(s.local_llm_base_url, { enabled: isOllama });
  // The embedder's own address and name (#179), each empty meaning "follow the
  // chat server / embeddinggemma" — mirrored by `resolve_embed` in
  // src-tauri/src/commands/chat.rs, which resolves the same two blanks.
  const embedUrl = s.embed_base_url.trim() || s.local_llm_base_url;
  const embedModel = s.embed_model.trim() || EMBEDDING_OLLAMA_MODEL;
  const embed = useEmbedProbe(embedUrl, embedModel, { enabled: isOllama });

  // Readiness — reflect exactly what's missing before chat can run. The local
  // ladder is `localChatHint`, shared with the Note pane's own readiness.
  let ready = false;
  let hint = "";
  if (isOllama) {
    hint = localChatHint({
      reachable,
      installed,
      model: s.chat_model,
      baseUrl: s.local_llm_base_url,
      where: "above",
    });
    ready = hint === "";
  } else {
    if (!key.hasKey) hint = "Add your OpenAI key above to use chat.";
    else if (!s.chat_model) hint = "Choose a chat model above.";
    else ready = true;
  }

  // Show the stored value even when it isn't a known option (e.g. an empty
  // choice, or a model list the app doesn't hard-code) so nothing looks blank.
  const openaiModelOptions = [
    ...(s.chat_model === "" ? [{ value: "", label: "Choose a model…" }] : []),
    ...SUMMARY_MODELS.map((m) => ({ value: m, label: m })),
  ];

  return (
    <Section title="AI Chat">
      <p className="text-xs text-[var(--color-text-muted)] leading-relaxed py-3.5">
        These settings cover chat over your personal notes. Workspace (Teams) chat is configured
        per workspace under Organization → Workspace chat.
      </p>
      <Row
        label="Provider"
        description="Ask questions grounded in your notes. Cloud (OpenAI) uses your key; Local runs fully offline against Ollama or any OpenAI-compatible server. Independent of your transcription and summary providers."
        control={
          <Select
            value={s.chat_provider}
            onChange={(v) => update("chat_provider", v)}
            options={CHAT_PROVIDERS}
          />
        }
      />

      {!isOllama && (
        <>
          <Row
            label="Model"
            description="A GPT-5-class model. Reasoning and tool-calling are handled automatically."
            control={
              <Select
                value={s.chat_model}
                onChange={(v) => update("chat_model", v)}
                options={openaiModelOptions}
              />
            }
          />
          <ProviderKeyCard
            provider="openai"
            description="Reused across cloud transcription, summaries, and chat — one key. Workspace chat uses the workspace's own key."
          />
        </>
      )}

      {isOllama && (
        <>
          <OllamaConnect
            baseUrl={s.local_llm_base_url}
            onBaseUrlChange={(v) => update("local_llm_base_url", v)}
            model={s.chat_model}
            onModelChange={(v) => update("chat_model", v)}
          />
          {isOllamaUrl(s.local_llm_base_url) && (
            <div className="py-3 space-y-2">
              <p className="text-xs text-[var(--color-text-muted)]">
                Runs fully offline. Don't have Ollama yet?{" "}
                <button
                  type="button"
                  onClick={() => openExternal("https://ollama.com/download")}
                  className="underline hover:text-[var(--color-text)]"
                >
                  Install Ollama
                </button>
                , then pull a tool-calling-capable model:
              </p>
              <CommandSnippet
                command={`ollama pull ${s.chat_model || RECOMMENDED_OLLAMA_MODEL}`}
                ariaLabel="Copy Ollama pull command"
              />
            </div>
          )}

          {/* Semantic retrieval (#48, #179). Optional — chat degrades to
              keyword-only — so this never blocks the readiness gate above.
              The embedder needs its own address because the chat server is
              often not the one that can embed: mlx_lm.server serves no
              /v1/embeddings at all, and llama-server wants its own process. */}
          <div className="py-3 space-y-3 border-t border-[var(--color-line)]">
            <p className="text-xs text-[var(--color-text-muted)]">
              Semantic search finds answers by meaning, not just keywords. It needs an embedding
              model; leave these blank to use {EMBEDDING_OLLAMA_MODEL} on the chat server above.
            </p>
            <Row
              label="Embedding server"
              control={
                <input
                  type="url"
                  value={s.embed_base_url}
                  onChange={(e) => update("embed_base_url", e.target.value)}
                  placeholder={s.local_llm_base_url}
                  aria-label="Embedding server URL"
                  className="w-56 text-sm px-3 py-1.5 rounded-md border border-[var(--color-line-visible)] bg-[var(--color-surface)] focus:border-[var(--color-text-muted)] transition-colors"
                />
              }
            />
            <Row
              label="Embedding model"
              control={
                <input
                  type="text"
                  value={s.embed_model}
                  onChange={(e) => update("embed_model", e.target.value)}
                  placeholder={EMBEDDING_OLLAMA_MODEL}
                  aria-label="Embedding model"
                  className="w-56 text-sm px-3 py-1.5 rounded-md border border-[var(--color-line-visible)] bg-[var(--color-surface)] focus:border-[var(--color-text-muted)] transition-colors"
                />
              }
            />
            {embed.checking && (
              <p className="text-xs text-[var(--color-text-muted)]">Checking the embedder…</p>
            )}
            {!embed.checking && embed.dims !== null && (
              <p className="text-xs text-[var(--color-success)]">
                Semantic search ready ✓ — {embedModel} answered with {embed.dims} dimensions.
              </p>
            )}
            {!embed.checking && embed.error && (
              <>
                <p className="text-xs text-[var(--color-warning-text)]">
                  {embedModel} didn't answer at {embedUrl} — chat searches by keyword only.{" "}
                  {embed.error}
                </p>
                {isOllamaUrl(embedUrl) && (
                  <CommandSnippet
                    command={`ollama pull ${embedModel}`}
                    ariaLabel="Copy embedding-model pull command"
                  />
                )}
              </>
            )}
            {/* Vectors are keyed by the model's name, so a working embedder and an
                embedded corpus are two different facts — and only the first is
                what the probe above establishes. */}
            {!embed.checking && embed.dims !== null && <EmbedCorpusLine model={embedModel} />}
          </div>
        </>
      )}

      <Row label="Status">
        <span className={ready ? "text-xs text-[var(--color-success)]" : "text-xs text-[var(--color-warning)]"}>
          {ready ? "Ready ✓" : "Setup needed"}
        </span>
        {!ready && hint && (
          <p className="text-xs text-[var(--color-text-muted)] mt-1">{hint}</p>
        )}
      </Row>

      <RebuildIndexRow />
    </Section>
  );
}

// Rebuild the whole library's retrieval index (issues #104, #122).
//
// NOT behind developer mode (#122). Hiding it meant a user whose library predates a
// chunking change had worse chat retrieval over their own notes than over a shared
// workspace's — the server rebuilds its index on deploy, the client never did — with
// nothing on screen to explain the gap or fix it.
//
// It stays quiet when there is nothing to repair. `chat_stale_note_count` reports how
// many notes still hold chunks from an older chunker, so this renders as a plain
// status line on a current library and as an actionable prompt only when it isn't.
// That is what makes it safe to show by default: a permanently visible "rebuild"
// button trains people to either ignore it or press it pointlessly, and pressing it
// pointlessly spends their embedding key.
type RebuildState =
  | { kind: "idle" }
  | { kind: "running" }
  | { kind: "done"; count: number }
  | { kind: "error"; message: string };

/** How many notes a rebuild would repair. `"loading"` on first read and `"unknown"`
 *  when the count could not be read — deliberately NOT collapsed into `0`, because
 *  `0` renders "Up to date ✓", which is a stronger claim than the one we'd be
 *  avoiding. Not knowing and knowing-it's-fine are different facts. */
type StaleCount = { kind: "loading" } | { kind: "unknown" } | { kind: "known"; count: number };

/** Whether the corpus is embedded under the model now in force (#179). The probe
 *  above answers "can this embedder be reached"; vectors are keyed by the model's
 *  name, so that says nothing about whether any note is findable through it. A
 *  count of 0 is deliberately silent — a correct state needs no line. */
function EmbedCorpusLine({ model }: { model: string }) {
  const [pending, setPending] = useState<number | null>(null);
  const [running, setRunning] = useState(false);

  const read = useCallback(() => {
    ipc
      .chatUnembeddedNoteCount()
      .then(setPending)
      .catch(() => setPending(null));
  }, []);
  // Re-read when the model changes: the whole point is that the answer is
  // per-model.
  useEffect(read, [read, model]);

  async function embedNow() {
    setRunning(true);
    try {
      await ipc.chatEmbedMissing();
    } finally {
      setRunning(false);
      read();
    }
  }

  if (pending === null || pending === 0) return null;
  return (
    <div className="space-y-2">
      <p className="text-xs text-[var(--color-warning-text)]">
        {pending} note{s(pending)} {pending === 1 ? "has" : "have"} no embedding under {model} yet
        — chat finds {pending === 1 ? "it" : "them"} by keyword only until then.
      </p>
      <button className="nd-btn" onClick={() => void embedNow()} disabled={running}>
        {running ? "Embedding…" : "Embed now"}
      </button>
    </div>
  );
}

function RebuildIndexRow() {
  const [state, setState] = useState<RebuildState>({ kind: "idle" });
  const [stale, setStale] = useState<StaleCount>({ kind: "loading" });

  // Re-read on mount and once a rebuild has FINISHED — `state.kind` changes on the way
  // into "running" too, so `done` is filtered explicitly rather than relying on the
  // dependency to do it: an extra count during the rebuild would contend with its
  // per-note locking for a number that cannot have settled yet.
  const settled = state.kind === "done";
  useEffect(() => {
    let live = true;
    ipc
      .chatStaleNoteCount()
      .then((n) => live && setStale({ kind: "known", count: n }))
      .catch(() => live && setStale({ kind: "unknown" }));
    return () => {
      live = false;
    };
  }, [settled]);

  async function rebuild() {
    setState({ kind: "running" });
    try {
      setState({ kind: "done", count: await ipc.chatRebuildIndex() });
    } catch (e) {
      setState({ kind: "error", message: String(e) });
    }
  }

  const running = state.kind === "running";

  return (
    <Row label="Chat search index">
      {stale.kind === "loading" && (
        <span className="text-xs text-[var(--color-text-muted)]">Checking…</span>
      )}
      {/* Couldn't read the count: say so rather than claiming either state. Offering
          the rebuild here would be an action on unknown state; claiming "up to date"
          would be the unverified assertion this whole row exists to avoid. */}
      {stale.kind === "unknown" && (
        <span className="text-xs text-[var(--color-text-muted)]">
          Couldn't check the index.
        </span>
      )}
      {stale.kind === "known" && stale.count === 0 && (
        <span className="text-xs text-[var(--color-success)]">Up to date ✓</span>
      )}
      {stale.kind === "known" && stale.count > 0 && (
        <>
          {/* `--color-warning-text` (not `--color-warning`) — the readable variant, per
              globals.css: raw warning gold doesn't clear AAA as body text. */}
          <p className="text-xs text-[var(--color-warning-text)]">
            {stale.count} recording{s(stale.count)} {stale.count === 1 ? "was" : "were"} indexed
            before the latest improvements — chat can't tell who spoke in{" "}
            {stale.count === 1 ? "it" : "them"}, and its excerpts may cut mid-sentence.
          </p>
          <button className="nd-btn mt-2" onClick={() => void rebuild()} disabled={running}>
            {running ? "Rebuilding…" : "Rebuild now"}
          </button>
          <p className="text-xs text-[var(--color-text-muted)] mt-1">
            Re-reads your whole library and re-embeds anything that changed, which uses your
            configured key — a few cents at most. Only affects what chat can find; your notes
            aren't changed.
          </p>
        </>
      )}
      {state.kind === "done" && (
        <p className="text-xs text-[var(--color-success)] mt-1">
          {/* Deliberately not "Rebuilt {count} of {stale}" — the rebuild walks the whole
              library, so this number is larger than the stale count it was offered for,
              and pairing them would read as a discrepancy. */}
          Index rebuilt across {state.count} note{s(state.count)} ✓
        </p>
      )}
      {state.kind === "error" && (
        <p className="text-xs text-[var(--color-danger)] mt-1">{state.message}</p>
      )}
    </Row>
  );
}
