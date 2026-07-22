// Step 5 — AI Summary (design/ONBOARDING.md § 5. AI Summary).
//
// A fork with asymmetric depth. On mount, check whether an OpenAI key
// exists (provider_key_get("openai") — set in step 4's Cloud card).
//
//   - OpenAI: if a key exists → PRESELECT this option, one click ("Use the
//     same OpenAI key") writes the summary settings and Continue. If no key
//     exists → inline key field with Save + Test.
//   - Local (Ollama): guided sub-flow. On select, probe localhost:11434.
//     Reachable → list installed models, preselect the recommended one if
//     present, else show the pull command with poll-until-present. Not
//     reachable → install instructions (ollama.com link + copy-button pull
//     command) with a "Waiting for Ollama…" poll that flips to a check.
//
// Neutral framing: if NO OpenAI key exists (user chose local transcription
// in step 4), show BOTH options neutrally with the trade-off line and never
// preselect Local.
//
// Skip: "Skip for now" advances without configuring. Summary is optional.
//
// Which settings each path writes (mirrors Settings → Summary):
//   OpenAI → summary_provider=openai, summary_model=gpt-5.4-mini
//   Local  → summary_provider=local, local_llm_base_url=<probed>,
//            local_llm_model=<selected>, local_llm_think=false
import { useEffect, useState } from "react";
import { open as openExternal } from "@tauri-apps/plugin-shell";
import { Sparkles, Cloud, Server, Check, Copy, ExternalLink } from "lucide-react";
import { ipc } from "../../../lib/ipc";
import { useOllamaProbe } from "../../../components/provider/useOllamaProbe";
import { useProviderKey } from "../../../components/provider/useProviderKey";
import { CommandSnippet } from "../../../components/CommandSnippet";
import type { StepContext } from "../types";
import { StepShell } from "../StepShell";
import {
  EMBEDDING_OLLAMA_MODEL,
  RECOMMENDED_OLLAMA_MODEL,
  RECOMMENDED_OLLAMA_MODEL_16GB,
  completionModels,
  isModelInstalled,
} from "../../../lib/localModels";

// Mirrors Settings → Summary defaults (settings/types.ts DEFAULTS).
const DEFAULT_OPENAI_SUMMARY_MODEL = "gpt-5.4-mini";
const DEFAULT_LOCAL_BASE_URL = "http://localhost:11434/v1";
// Recommended Ollama models by RAM tier live in src/lib/localModels.ts
// (imported above): gemma4:12b-mlx headline (~24GB+), qwen3.5:4b 16GB fallback.
// `DEFAULTS.local_llm_model` is "" so there's no single-string default setting;
// these are the recommended pulls.
const OLLAMA_POLL_MS = 2000;

type Option = "openai" | "local" | null;

export function SummaryStep({ ctx }: { ctx: StepContext }) {
  // null = still resolving whether an OpenAI key exists.
  const [hasOpenAiKey, setHasOpenAiKey] = useState<boolean | null>(null);
  const [selection, setSelection] = useState<Option>(null);

  // OpenAI inline-key mechanics (only rendered when no key exists yet) come
  // from the shared hook (#22); the step wraps test() with its commit point.
  const key = useProviderKey("openai");
  const [openaiConfigured, setOpenaiConfigured] = useState(false);

  // Local (Ollama) sub-flow state. Reachability + model list come from the
  // shared probe hook (#22); presentation stays this wizard's staged cards.
  const [selectedModel, setSelectedModel] = useState<string>("");
  const [localConfigured, setLocalConfigured] = useState(false);
  const [copied, setCopied] = useState(false);

  // On mount: does an OpenAI key exist? If so, preselect the OpenAI option.
  useEffect(() => {
    let cancelled = false;
    ipc
      .getProviderKey("openai")
      .then((k) => {
        if (cancelled) return;
        const has = !!k;
        setHasOpenAiKey(has);
        if (has) setSelection("openai");
      })
      .catch(() => {
        if (!cancelled) setHasOpenAiKey(false);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // ---- Local (Ollama) probing + polling -----------------------------------
  // The shared hook probes the model-listing endpoint every ~2s while the
  // Local option is selected — the "Waiting for Ollama…" loop: it flips
  // `reachable`/`installed` when the server appears, and re-lists so a
  // freshly-pulled model shows up without a retry button
  // (design/ONBOARDING.md § 5). Deselecting parks it.
  const { reachable, installed } = useOllamaProbe(DEFAULT_LOCAL_BASE_URL, {
    pollMs: OLLAMA_POLL_MS,
    enabled: selection === "local",
  });

  // Preselect the recommended model if present; else first installed; else
  // leave empty so the pull command shows. Re-runs per poll so a pull that
  // completes mid-wizard is picked up.
  useEffect(() => {
    if (!installed) return;
    // Only completion models are valid picks — never embeddinggemma etc. (#48).
    const usable = completionModels(installed);
    setSelectedModel((prev) => {
      if (prev && usable.includes(prev)) return prev;
      if (usable.includes(RECOMMENDED_OLLAMA_MODEL)) return RECOMMENDED_OLLAMA_MODEL;
      if (usable.includes(RECOMMENDED_OLLAMA_MODEL_16GB)) return RECOMMENDED_OLLAMA_MODEL_16GB;
      return usable[0] ?? "";
    });
  }, [installed]);

  // Seed the AI Chat provider (issue #47) to mirror the summary choice, so
  // chat works out of the box without a second setup — chat reuses the same
  // OpenAI key / Ollama server. It can still be changed later in Settings → Chat.
  async function seedChatProvider(chatProvider: "openai" | "ollama", chatModel: string) {
    try {
      await ipc.setSetting("chat_provider", chatProvider);
      await ipc.setSetting("chat_model", chatModel);
    } catch (e) {
      console.warn("[onboarding] failed to seed chat provider:", e);
    }
  }

  // ---- OpenAI path --------------------------------------------------------
  async function useSameOpenAiKey() {
    try {
      await ipc.setSetting("summary_provider", "openai");
      await ipc.setSetting("summary_model", DEFAULT_OPENAI_SUMMARY_MODEL);
      await seedChatProvider("openai", DEFAULT_OPENAI_SUMMARY_MODEL);
      setOpenaiConfigured(true);
    } catch (e) {
      console.warn("[onboarding] failed to write openai summary settings:", e);
    }
  }

  // A passed Test is the commit point: write OpenAI as the summary provider.
  async function testOpenAiKey() {
    if (!(await key.test())) return;
    try {
      await ipc.setSetting("summary_provider", "openai");
      await ipc.setSetting("summary_model", DEFAULT_OPENAI_SUMMARY_MODEL);
      await seedChatProvider("openai", DEFAULT_OPENAI_SUMMARY_MODEL);
      setOpenaiConfigured(true);
    } catch (e) {
      console.warn("[onboarding] failed to write openai summary settings:", e);
    }
  }

  // ---- Local path: write settings on model selection ----------------------
  async function chooseLocalModel(model: string) {
    setSelectedModel(model);
    if (!model) {
      setLocalConfigured(false);
      return;
    }
    try {
      await ipc.setSetting("summary_provider", "local");
      await ipc.setSetting("local_llm_base_url", DEFAULT_LOCAL_BASE_URL);
      await ipc.setSetting("local_llm_model", model);
      await ipc.setSetting("local_llm_think", "false");
      await seedChatProvider("ollama", model);
      setLocalConfigured(true);
    } catch (e) {
      console.warn("[onboarding] failed to write local summary settings:", e);
    }
  }

  function copyPullCommand() {
    const cmd = `ollama pull ${RECOMMENDED_OLLAMA_MODEL}`;
    navigator.clipboard?.writeText(cmd).then(
      () => {
        setCopied(true);
        window.setTimeout(() => setCopied(false), 2000);
      },
      () => {
        /* clipboard blocked — non-fatal */
      },
    );
  }

  function selectOpenAi() {
    setSelection("openai");
  }
  function selectLocal() {
    // The probe hook resets to "no verdict" while disabled, so flipping the
    // selection starts from a clean probing state automatically.
    setSelection("local");
  }

  // Continue enables when either path is fully configured.
  const canContinue = openaiConfigured || localConfigured;

  const loading = hasOpenAiKey === null;

  // Neutral framing when no OpenAI key: show the trade-off line and never
  // preselect Local (we simply never call setSelection("local") on mount).
  const neutral = hasOpenAiKey === false;

  if (loading) {
    return (
      <StepShell
        icon={<Sparkles size={26} strokeWidth={1.6} />}
        title="Set up AI summaries"
        subtitle="Humla fuses your notes with the transcript into a summary."
      />
    );
  }

  return (
    <StepShell
      icon={<Sparkles size={26} strokeWidth={1.6} />}
      title="Set up AI summaries"
      subtitle={
        neutral
          ? "Humla fuses your notes with the transcript into a summary — and powers AI Chat over your notes. Local: private, needs Ollama + ~6 GB RAM · OpenAI: better summaries, needs API key."
          : "Humla fuses your notes with the transcript into a summary, and powers AI Chat over your notes. This is optional — you can skip it and set it up later."
      }
    >
      <div className="w-full max-w-md flex flex-col gap-3 text-left">
        {/* OpenAI option */}
        <div
          className={
            "rounded-[var(--radius)] border px-4 py-4 transition-colors " +
            (selection === "openai"
              ? "border-[var(--color-accent)] bg-[var(--color-accent-soft)]"
              : "border-[var(--color-line)] bg-[var(--color-surface)]")
          }
        >
          <button type="button" onClick={selectOpenAi} className="text-left w-full">
            <div className="flex items-start gap-3">
              <Cloud
                size={18}
                strokeWidth={1.8}
                className="mt-0.5 shrink-0 text-[var(--color-text-muted)]"
              />
              <div className="min-w-0 flex-1">
                <div className="flex items-center gap-2 flex-wrap">
                  <span className="text-sm font-semibold text-[var(--color-text)]">
                    OpenAI
                  </span>
                  {openaiConfigured && (
                    <Check
                      size={15}
                      strokeWidth={2.5}
                      className="text-[var(--color-accent-text)]"
                    />
                  )}
                </div>
                <p className="mt-1 text-xs leading-relaxed text-[var(--color-text-muted)]">
                  Better summaries. Sends the transcript to OpenAI.
                </p>
              </div>
            </div>
          </button>

          {selection === "openai" && (
            <div className="mt-4">
              {hasOpenAiKey ? (
                openaiConfigured ? (
                  <p className="text-xs text-[var(--color-success)] flex items-center gap-1.5">
                    <Check size={13} strokeWidth={2.5} />
                    Using your OpenAI key ({DEFAULT_OPENAI_SUMMARY_MODEL})
                  </p>
                ) : (
                  <button
                    type="button"
                    onClick={useSameOpenAiKey}
                    className="nd-btn nd-btn-primary"
                  >
                    Use the same OpenAI key
                  </button>
                )
              ) : (
                <div className="flex flex-col gap-3">
                  <div className="flex gap-2">
                    <input
                      type="password"
                      value={key.draft}
                      onChange={(e) => key.setDraft(e.target.value)}
                      placeholder={key.hasKey ? "•••••••• stored" : "sk-…"}
                      className="flex-1 min-w-0 px-3 py-2 rounded-md text-sm bg-[var(--color-input-bg)] border border-[var(--color-line)] focus:border-[var(--color-text-muted)]"
                    />
                    <button
                      type="button"
                      onClick={key.save}
                      disabled={!key.draft.trim()}
                      className="nd-btn"
                    >
                      Save
                    </button>
                    <button
                      type="button"
                      onClick={testOpenAiKey}
                      disabled={!key.hasKey || key.testing}
                      className="nd-btn nd-btn-primary"
                    >
                      {key.testing ? "Testing…" : "Test"}
                    </button>
                  </div>
                  {key.result?.ok === true && (
                    <p className="text-xs text-[var(--color-success)] flex items-center gap-1.5">
                      <Check size={13} strokeWidth={2.5} />
                      Connected
                    </p>
                  )}
                  {key.result?.ok === false && (
                    <p className="text-xs text-[var(--color-danger)] break-all">
                      {key.result.message}
                    </p>
                  )}
                </div>
              )}
            </div>
          )}
        </div>

        {/* Local (Ollama) option */}
        <div
          className={
            "rounded-[var(--radius)] border px-4 py-4 transition-colors " +
            (selection === "local"
              ? "border-[var(--color-accent)] bg-[var(--color-accent-soft)]"
              : "border-[var(--color-line)] bg-[var(--color-surface)]")
          }
        >
          <button type="button" onClick={selectLocal} className="text-left w-full">
            <div className="flex items-start gap-3">
              <Server
                size={18}
                strokeWidth={1.8}
                className="mt-0.5 shrink-0 text-[var(--color-text-muted)]"
              />
              <div className="min-w-0 flex-1">
                <div className="flex items-center gap-2 flex-wrap">
                  <span className="text-sm font-semibold text-[var(--color-text)]">
                    Local (Ollama)
                  </span>
                  {localConfigured && (
                    <Check
                      size={15}
                      strokeWidth={2.5}
                      className="text-[var(--color-accent-text)]"
                    />
                  )}
                </div>
                <p className="mt-1 text-xs leading-relaxed text-[var(--color-text-muted)]">
                  Private — keeps the transcript on your Mac. Needs Ollama + ~6 GB RAM.
                </p>
              </div>
            </div>
          </button>

          {selection === "local" && (
            <div className="mt-4 flex flex-col gap-3">
              {reachable === null && (
                <p className="text-xs text-[var(--color-text-muted)]">
                  Checking for Ollama…
                </p>
              )}

              {/* Not reachable → install instructions + waiting indicator. */}
              {reachable === false && (
                <div className="flex flex-col gap-3">
                  <p className="text-xs leading-relaxed text-[var(--color-text-muted)]">
                    Ollama isn't running. Install it, then pull the recommended
                    model:
                  </p>
                  <button
                    type="button"
                    onClick={() => openExternal("https://ollama.com/download")}
                    className="text-xs inline-flex items-center gap-1.5 self-start underline text-[var(--color-text)] hover:text-[var(--color-accent-text)]"
                  >
                    Download Ollama
                    <ExternalLink size={12} strokeWidth={2} />
                  </button>
                  <PullCommand copied={copied} onCopy={copyPullCommand} />
                  <p className="text-xs text-[var(--color-text-muted)] flex items-center gap-2">
                    <span className="inline-block w-2 h-2 rounded-full bg-[var(--color-warning)] animate-pulse" />
                    Waiting for Ollama…
                  </p>
                </div>
              )}

              {/* Reachable → model selection. */}
              {reachable === true && installed !== null && (
                <div className="flex flex-col gap-3">
                  {installed.length === 0 ? (
                    <>
                      <p className="text-xs leading-relaxed text-[var(--color-text-muted)]">
                        Ollama is running, but no models are installed. Pull the
                        recommended one:
                      </p>
                      <PullCommand copied={copied} onCopy={copyPullCommand} />
                      <p className="text-xs text-[var(--color-text-muted)] flex items-center gap-2">
                        <span className="inline-block w-2 h-2 rounded-full bg-[var(--color-warning)] animate-pulse" />
                        Waiting for the model…
                      </p>
                    </>
                  ) : !installed.includes(RECOMMENDED_OLLAMA_MODEL) &&
                    !installed.includes(RECOMMENDED_OLLAMA_MODEL_16GB) &&
                    !selectedModel ? (
                    <>
                      <p className="text-xs leading-relaxed text-[var(--color-text-muted)]">
                        The recommended model isn't installed. Pull it, or pick
                        one of your existing models below.
                      </p>
                      <PullCommand copied={copied} onCopy={copyPullCommand} />
                      <ModelSelect
                        installed={installed}
                        value={selectedModel}
                        onChange={chooseLocalModel}
                      />
                    </>
                  ) : (
                    <>
                      <p className="text-xs text-[var(--color-success)] flex items-center gap-1.5">
                        <Check size={13} strokeWidth={2.5} />
                        Ollama is running
                      </p>
                      <ModelSelect
                        installed={installed}
                        value={selectedModel}
                        onChange={chooseLocalModel}
                      />
                      {localConfigured && (
                        <p className="text-xs text-[var(--color-text-muted)]">
                          Using <code>{selectedModel}</code>.
                        </p>
                      )}
                    </>
                  )}
                </div>
              )}

              {/* Optional embedding model for semantic chat (issue #48). Never
                  blocks setup — chat works keyword-only without it. */}
              {reachable === true && (
                <div className="mt-3 pt-3 border-t border-[var(--color-line)] space-y-1.5">
                  {isModelInstalled(installed, EMBEDDING_OLLAMA_MODEL) ? (
                    <p className="text-xs text-[var(--color-success)] flex items-center gap-1.5">
                      <Check size={13} strokeWidth={2.5} />
                      Semantic chat search ready ({EMBEDDING_OLLAMA_MODEL})
                    </p>
                  ) : (
                    <>
                      <p className="text-xs text-[var(--color-text-muted)]">
                        Optional: for AI chat that finds answers by meaning, pull the small
                        embedding model too.
                      </p>
                      <CommandSnippet
                        command={`ollama pull ${EMBEDDING_OLLAMA_MODEL}`}
                        ariaLabel="Copy embedding-model pull command"
                      />
                    </>
                  )}
                </div>
              )}
            </div>
          )}
        </div>
      </div>

      {/* Continue + Skip. */}
      <div className="mt-8 w-full max-w-md flex flex-col items-center gap-3">
        <button
          type="button"
          className="nd-btn nd-btn-primary"
          disabled={!canContinue}
          onClick={ctx.goNext}
        >
          Continue
        </button>

        <button
          type="button"
          onClick={ctx.goNext}
          className="text-xs text-[var(--color-text-muted)] hover:text-[var(--color-text)] transition-colors"
        >
          Skip for now — you can set this up later
        </button>
      </div>
    </StepShell>
  );
}

function PullCommand({ copied, onCopy }: { copied: boolean; onCopy: () => void }) {
  return (
    <div className="flex flex-col gap-1.5">
      <div className="flex items-center gap-2">
        <code
          className="flex-1 min-w-0 truncate px-3 py-2 rounded-md text-xs bg-[var(--color-pill-hover)]"
          style={{ fontFamily: "var(--font-mono)" }}
        >
          ollama pull {RECOMMENDED_OLLAMA_MODEL}
        </code>
        <button
          type="button"
          onClick={onCopy}
          className="nd-btn shrink-0"
          aria-label="Copy pull command"
        >
          {copied ? (
            <>
              <Check size={13} strokeWidth={2.5} />
              Copied
            </>
          ) : (
            <>
              <Copy size={13} strokeWidth={2} />
              Copy
            </>
          )}
        </button>
      </div>
      <p className="text-[11px] text-[var(--color-text-muted)]">
        On a 16 GB Mac, use <code>ollama pull {RECOMMENDED_OLLAMA_MODEL_16GB}</code> instead
        — {RECOMMENDED_OLLAMA_MODEL} needs ~24 GB+.
      </p>
    </div>
  );
}

function ModelSelect({
  installed,
  value,
  onChange,
}: {
  installed: string[];
  value: string;
  onChange: (v: string) => void;
}) {
  return (
    <label className="block">
      <span className="sr-only">Model</span>
      <select
        value={value}
        onChange={(e) => onChange(e.target.value)}
        className="w-full px-3 py-2 rounded-md text-sm bg-[var(--color-input-bg)] border border-[var(--color-line)] focus:border-[var(--color-text-muted)]"
      >
        <option value="">— pick a model —</option>
        {completionModels(installed).map((m) => (
          <option key={m} value={m}>
            {m}
            {m === RECOMMENDED_OLLAMA_MODEL
              ? " (recommended)"
              : m === RECOMMENDED_OLLAMA_MODEL_16GB
                ? " (16 GB Macs)"
                : ""}
          </option>
        ))}
      </select>
    </label>
  );
}
