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
import { useCallback, useEffect, useState } from "react";
import { open as openExternal } from "@tauri-apps/plugin-shell";
import { Sparkles, Cloud, Server, Check, Copy, ExternalLink } from "lucide-react";
import { ipc, type TranscribeProvider } from "../../../lib/ipc";
import type { StepContext } from "../types";
import { StepShell } from "../StepShell";

// Mirrors Settings → Summary defaults (settings/types.ts DEFAULTS).
const DEFAULT_OPENAI_SUMMARY_MODEL = "gpt-5.4-mini";
const DEFAULT_LOCAL_BASE_URL = "http://localhost:11434/v1";
// The recommended Ollama model. Source: src/pages/settings/tabs/Summary.tsx
// ("ollama pull qwen3.5:4b") — the Qwen 3.5 variant the sampling profile in
// src-tauri/src/openai.rs is tuned for. `DEFAULTS.local_llm_model` is "" so
// there's no single-string default setting; this is the recommended pull.
const RECOMMENDED_OLLAMA_MODEL = "qwen3.5:4b";
const OLLAMA_POLL_MS = 2000;

type Option = "openai" | "local" | null;

export function SummaryStep({ ctx }: { ctx: StepContext }) {
  // null = still resolving whether an OpenAI key exists.
  const [hasOpenAiKey, setHasOpenAiKey] = useState<boolean | null>(null);
  const [selection, setSelection] = useState<Option>(null);

  // OpenAI inline-key state (only when no key exists yet).
  const [keyDraft, setKeyDraft] = useState("");
  const [keySaved, setKeySaved] = useState(false);
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<
    null | { ok: true } | { ok: false; message: string }
  >(null);
  const [openaiConfigured, setOpenaiConfigured] = useState(false);

  // Local (Ollama) sub-flow state.
  const [probing, setProbing] = useState(false);
  const [reachable, setReachable] = useState<boolean | null>(null);
  const [installed, setInstalled] = useState<string[] | null>(null);
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
  // A single probe: hit the model-listing endpoint. Reachable → we also get
  // the installed model list for free. Unreachable classifies as "install
  // Ollama".
  const probeOllama = useCallback(async () => {
    setProbing(true);
    try {
      const list = await ipc.localLlmListModels(DEFAULT_LOCAL_BASE_URL);
      setReachable(true);
      setInstalled(list);
      // Preselect the recommended model if present; else leave empty so the
      // pull command shows.
      setSelectedModel((prev) => {
        if (prev && list.includes(prev)) return prev;
        if (list.includes(RECOMMENDED_OLLAMA_MODEL)) return RECOMMENDED_OLLAMA_MODEL;
        return list[0] ?? "";
      });
    } catch {
      setReachable(false);
      setInstalled(null);
    } finally {
      setProbing(false);
    }
  }, []);

  // While the Local option is selected, probe every ~2s. This is the
  // "Waiting for Ollama…" loop: it flips `reachable`/`installed` when the
  // server appears, and re-lists so a freshly-pulled model shows up without
  // a retry button (design/ONBOARDING.md § 5). A 2s /models GET is cheap;
  // the interval is torn down on unmount / option change.
  useEffect(() => {
    if (selection !== "local") return;
    void probeOllama(); // immediate probe on selection
    const timer = window.setInterval(() => void probeOllama(), OLLAMA_POLL_MS);
    return () => window.clearInterval(timer);
  }, [selection, probeOllama]);

  // ---- OpenAI path --------------------------------------------------------
  async function useSameOpenAiKey() {
    try {
      await ipc.setSetting("summary_provider", "openai");
      await ipc.setSetting("summary_model", DEFAULT_OPENAI_SUMMARY_MODEL);
      setOpenaiConfigured(true);
    } catch (e) {
      console.warn("[onboarding] failed to write openai summary settings:", e);
    }
  }

  async function saveOpenAiKey() {
    const trimmed = keyDraft.trim();
    if (!trimmed) return;
    try {
      await ipc.setProviderKey("openai" as TranscribeProvider, trimmed);
      setKeySaved(true);
      setKeyDraft("");
      setTestResult(null);
    } catch (e) {
      setTestResult({ ok: false, message: String(e) });
    }
  }

  async function testOpenAiKey() {
    setTesting(true);
    setTestResult(null);
    try {
      const r = await ipc.testProviderKey("openai" as TranscribeProvider);
      if (r.ok) {
        setTestResult({ ok: true });
        await ipc.setSetting("summary_provider", "openai");
        await ipc.setSetting("summary_model", DEFAULT_OPENAI_SUMMARY_MODEL);
        setOpenaiConfigured(true);
      } else {
        setTestResult({
          ok: false,
          message: `${r.status}: ${r.error ?? "unknown error"}`,
        });
      }
    } catch (e) {
      setTestResult({ ok: false, message: String(e) });
    } finally {
      setTesting(false);
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
    setSelection("local");
    setReachable(null);
    setInstalled(null);
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
          ? "Humla fuses your notes with the transcript into a summary. Local: private, needs Ollama + ~6 GB RAM · OpenAI: better summaries, needs API key."
          : "Humla fuses your notes with the transcript into a summary. This is optional — you can skip it and set it up later."
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
                      value={keyDraft}
                      onChange={(e) => {
                        setKeyDraft(e.target.value);
                        if (keySaved) setKeySaved(false);
                        if (testResult) setTestResult(null);
                      }}
                      placeholder={keySaved ? "•••••••• stored" : "sk-…"}
                      className="flex-1 min-w-0 px-3 py-2 rounded-md text-sm bg-[var(--color-input-bg)] border border-[var(--color-line)] focus:border-[var(--color-text-muted)]"
                    />
                    <button
                      type="button"
                      onClick={saveOpenAiKey}
                      disabled={!keyDraft.trim()}
                      className="nd-btn"
                    >
                      Save
                    </button>
                    <button
                      type="button"
                      onClick={testOpenAiKey}
                      disabled={!keySaved || testing}
                      className="nd-btn nd-btn-primary"
                    >
                      {testing ? "Testing…" : "Test"}
                    </button>
                  </div>
                  {testResult?.ok === true && (
                    <p className="text-xs text-[var(--color-success)] flex items-center gap-1.5">
                      <Check size={13} strokeWidth={2.5} />
                      Connected
                    </p>
                  )}
                  {testResult?.ok === false && (
                    <p className="text-xs text-[var(--color-danger)] break-all">
                      {testResult.message}
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
              {reachable === null && probing && (
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
        {installed.map((m) => (
          <option key={m} value={m}>
            {m}
            {m === RECOMMENDED_OLLAMA_MODEL ? " (recommended)" : ""}
          </option>
        ))}
      </select>
    </label>
  );
}
