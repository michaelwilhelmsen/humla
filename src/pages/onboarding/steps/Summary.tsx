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
// Skip: "Skip for now" advances without gating. Note it does NOT undo work
// already done: on the local path, reaching a reachable server with a usable
// model commits it immediately (#147), so skipping afterwards leaves a working
// local summary provider configured rather than nothing. That's deliberate and
// matches the wizard's write-through philosophy elsewhere — the Language step
// persists its displayed default for the same reason (#9). The settings written
// are valid and the model is genuinely installed, so the user ends up with a
// working setup they can change in Settings, never a broken one.
//
// Which settings each path writes (mirrors Settings → Summary):
//   OpenAI → summary_provider=openai, summary_model=gpt-5.4-mini
//   Local  → summary_provider=local, local_llm_base_url=<probed>,
//            local_llm_model=<selected>, local_llm_think=false
import { useCallback, useEffect, useRef, useState } from "react";
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
  RECOMMENDED_OLLAMA_MODELS,
  RECOMMENDED_OLLAMA_MODEL_16GB,
  completionModels,
  isModelInstalled,
} from "../../../lib/localModels";
import { Select } from "../../../components/ui/Select";

// Mirrors Settings → Summary defaults (settings/types.ts DEFAULTS).
const DEFAULT_OPENAI_SUMMARY_MODEL = "gpt-5.4-mini";
const DEFAULT_LOCAL_BASE_URL = "http://localhost:11434/v1";
// Recommended Ollama models by RAM tier live in src/lib/localModels.ts
// (imported above): gemma4:12b-mlx headline (~24GB+), qwen3.5:4b 16GB fallback.
// `DEFAULTS.local_llm_model` is "" so there's no single-string default setting;
// these are the recommended pulls.
const OLLAMA_POLL_MS = 2000;

type Option = "openai" | "local" | null;

// Seed the AI Chat provider (issue #47) to mirror the summary choice, so chat
// works out of the box without a second setup — chat reuses the same OpenAI key
// / Ollama server. It can still be changed later in Settings → Chat. Module
// scope so the commit paths below can be stable useCallbacks.
async function seedChatProvider(chatProvider: "openai" | "ollama", chatModel: string) {
  try {
    await ipc.setSetting("chat_provider", chatProvider);
    await ipc.setSetting("chat_model", chatModel);
  } catch (e) {
    console.warn("[onboarding] failed to seed chat provider:", e);
  }
}

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
  // `pickedModel` is the user's EXPLICIT choice — "" means they haven't made
  // one and `effectiveModel` (below) resolves the preselect instead.
  const [pickedModel, setPickedModel] = useState<string>("");
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

  // Only completion models are valid picks — never embeddinggemma etc. (#48).
  const usable = completionModels(installed);

  // The model the picker shows and the settings commit to: the user's explicit
  // pick while it's still installed, else the recommended one for this Mac's
  // RAM tier, else whatever else is installed, else "" (nothing usable → the
  // pull command shows). DERIVED AT RENDER rather than pushed into state by an
  // effect, so there is exactly one answer per render and no window in which
  // the picker shows one model while the settings hold another — which is the
  // bug class #147 belonged to.
  const recommendedInstalled = RECOMMENDED_OLLAMA_MODELS.find((m) => usable.includes(m));
  const effectiveModel =
    pickedModel && usable.includes(pickedModel)
      ? pickedModel
      : (recommendedInstalled ?? usable[0] ?? "");

  // ---- Local path: the single commit point --------------------------------
  // Reached from the preselect effect below AND from the picker's onChange, so
  // an auto-preselected model and a hand-picked one are indistinguishable
  // downstream (#147).
  //
  // `committedRef` makes it idempotent, and it carries real load: the effect
  // below re-runs on every ~2s probe re-list (deliberately — that's the retry),
  // so without the ref each poll would rewrite all five settings. It also
  // absorbs React StrictMode's dev-only double-invoke on mount, and tells the
  // mid-flight supersede checks which commit is still the current one.
  const committedRef = useRef<string>("");
  const commitLocalModel = useCallback(async (model: string) => {
    if (committedRef.current === model) return;
    committedRef.current = model;
    try {
      // These three are constants, so their order against a superseding commit
      // doesn't matter.
      await ipc.setSetting("summary_provider", "local");
      await ipc.setSetting("local_llm_base_url", DEFAULT_LOCAL_BASE_URL);
      await ipc.setSetting("local_llm_think", "false");
      // The model-name writes DO matter. A pick made during the round-trips
      // above has already claimed the ref; bail instead of racing its writes,
      // or the last one to land wins and SQLite ends up disagreeing with the
      // model the card says it's using.
      if (committedRef.current !== model) return;
      await ipc.setSetting("local_llm_model", model);
      if (committedRef.current !== model) return;
      await seedChatProvider("ollama", model);
      if (committedRef.current !== model) return;
      setLocalConfigured(true);
    } catch (e) {
      // Not committed after all: clear the guard so the next probe re-list
      // retries (see the effect's `installed` dep), and keep Continue disabled
      // rather than promising a setup that isn't there.
      if (committedRef.current === model) committedRef.current = "";
      setLocalConfigured(false);
      console.warn("[onboarding] failed to write local summary settings:", e);
    }
  }, []);

  // #147 — accepting the preselected model must write the same settings an
  // explicit pick writes. Preselection used to call setState directly, so it
  // displayed a model while `localConfigured` stayed false: no ✓, no "Using
  // <model>", and Continue disabled until the user poked the dropdown. Same
  // shape as #9, where the Language step showed a default it never persisted.
  // Committing here rather than in the picker's onChange is what makes the two
  // paths literally the same code.
  //
  // `installed` is a dep on purpose, and it is what `committedRef` guards: the
  // probe re-lists every ~2s with a fresh array identity, so this re-runs per
  // poll and the ref turns all but the first into a no-op. That re-run IS the
  // retry — a commit that failed cleared the ref, so the next poll attempts it
  // again. Keying on `effectiveModel` alone was tidier but left a transient
  // settings_set failure permanently stuck with Continue disabled, since
  // re-picking the same model changes no state and fires no effect.
  useEffect(() => {
    if (!effectiveModel) {
      setLocalConfigured(false);
      return;
    }
    void commitLocalModel(effectiveModel);
  }, [effectiveModel, commitLocalModel, installed]);

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
                  {/* Nothing usable to summarise with → the pull command is the
                      only way forward, and Continue stays disabled. Keyed on
                      `usable`, not `installed`: a server holding only
                      embeddinggemma has models but none that can chat, and it
                      used to land here with an empty picker and no waiting
                      indicator. */}
                  {usable.length === 0 ? (
                    <>
                      <p className="text-xs leading-relaxed text-[var(--color-text-muted)]">
                        {installed.length === 0
                          ? "Ollama is running, but no models are installed. Pull the recommended one:"
                          : "Ollama is running, but none of your installed models can write summaries. Pull the recommended one:"}
                      </p>
                      <PullCommand copied={copied} onCopy={copyPullCommand} />
                      <p className="text-xs text-[var(--color-text-muted)] flex items-center gap-2">
                        <span className="inline-block w-2 h-2 rounded-full bg-[var(--color-warning)] animate-pulse" />
                        Waiting for the model…
                      </p>
                    </>
                  ) : (
                    <>
                      <p className="text-xs text-[var(--color-success)] flex items-center gap-1.5">
                        <Check size={13} strokeWidth={2.5} />
                        Ollama is running
                      </p>
                      {/* Neither recommendation installed: the model below is
                          already committed, so this is an upgrade hint, not a
                          blocker. It used to be an alternative branch gated on
                          an empty selection — which auto-preselection made
                          unreachable, so the hint silently vanished (#147). */}
                      {!recommendedInstalled && (
                        <>
                          <p className="text-xs leading-relaxed text-[var(--color-text-muted)]">
                            The recommended model isn't installed — Humla will
                            use the one below. For better summaries, pull it:
                          </p>
                          <PullCommand copied={copied} onCopy={copyPullCommand} />
                        </>
                      )}
                      <ModelSelect
                        models={usable}
                        value={effectiveModel}
                        onChange={setPickedModel}
                      />
                      {localConfigured && (
                        <p className="text-xs text-[var(--color-text-muted)]">
                          Using <code>{effectiveModel}</code>.
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
          // --font-mono is a no-op alias onto the Hanken stack; a real shell
          // command wants --font-code. Visible side by side with the embedding
          // model's CommandSnippet, which already uses it.
          style={{ fontFamily: "var(--font-code)" }}
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

// `models` is already filtered to completion models by the caller — it must be,
// since the same list decides what gets auto-committed. Re-filtering here would
// be a second derivation of one truth, and the two could drift.
function ModelSelect({
  models,
  value,
  onChange,
}: {
  models: string[];
  value: string;
  onChange: (v: string) => void;
}) {
  return (
    <Select
      ariaLabel="Model"
      value={value}
      onChange={onChange}
      options={[
        // Placeholder only when there is genuinely nothing selected. Offering
        // it alongside a committed model made "— pick a model —" a no-op that
        // snapped straight back to the preselect.
        ...(value === "" ? [{ value: "", label: "— pick a model —" }] : []),
        ...models.map((m) => ({
          value: m,
          label:
            m +
            (m === RECOMMENDED_OLLAMA_MODEL
              ? " (recommended)"
              : m === RECOMMENDED_OLLAMA_MODEL_16GB
                ? " (16 GB Macs)"
                : ""),
        })),
      ]}
      className="w-full max-w-none justify-between px-3 py-2 bg-[var(--color-input-bg)] border-[var(--color-line)] hover:bg-[var(--color-input-bg)] focus:border-[var(--color-text-muted)]"
    />
  );
}
