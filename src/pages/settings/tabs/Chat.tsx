import { useEffect, useState } from "react";
import { open as openExternal } from "@tauri-apps/plugin-shell";
import { Row, Section } from "../components/Section";
import { Select } from "../components/Select";
import { OllamaConnect } from "../../../components/provider/OllamaConnect";
import { ProviderKeyCard } from "../../../components/provider/ProviderKeyCard";
import { CommandSnippet } from "../../../components/CommandSnippet";
import { useOllamaProbe } from "../../../components/provider/useOllamaProbe";
import { useProviderKey } from "../../../components/provider/useProviderKey";
import { CHAT_PROVIDERS, SUMMARY_MODELS } from "../types";
import {
  EMBEDDING_OLLAMA_MODEL,
  RECOMMENDED_OLLAMA_MODEL,
  isEmbeddingModel,
  isModelInstalled,
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

  // Readiness — reflect exactly what's missing before chat can run.
  let ready = false;
  let hint = "";
  if (isOllama) {
    if (reachable === false) hint = "Start or install Ollama — it's detected automatically.";
    else if (!s.chat_model) hint = "Choose a chat model above.";
    else if (isEmbeddingModel(s.chat_model))
      hint = `“${s.chat_model}” is an embedding model — choose a chat model above.`;
    else if (installed && !installed.includes(s.chat_model))
      hint = `“${s.chat_model}” isn't installed on the server — run ollama pull ${s.chat_model}.`;
    else ready = true;
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
        description="Ask questions grounded in your notes. Cloud (OpenAI) uses your key; Local (Ollama) runs fully offline. Independent of your transcription and summary providers."
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
          {/* Embedding model for semantic retrieval (issue #48). Optional —
              chat works keyword-only without it — so this never blocks the
              readiness gate above; it's a soft recommendation. */}
          <div className="py-3 space-y-2 border-t border-[var(--color-line)]">
            {isModelInstalled(installed, EMBEDDING_OLLAMA_MODEL) ? (
              <p className="text-xs text-[var(--color-success)]">
                Semantic search ready ✓ — {EMBEDDING_OLLAMA_MODEL} is installed.
              </p>
            ) : (
              <>
                <p className="text-xs text-[var(--color-text-muted)]">
                  For semantic search — finding answers by meaning, not just keywords — also pull
                  the embedding model (~600 MB). Optional; chat works without it.
                </p>
                <CommandSnippet
                  command={`ollama pull ${EMBEDDING_OLLAMA_MODEL}`}
                  ariaLabel="Copy embedding-model pull command"
                />
              </>
            )}
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

/** Plural suffix, so the row's copy doesn't repeat the same ternary four times. */
const s = (n: number) => (n === 1 ? "" : "s");

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
