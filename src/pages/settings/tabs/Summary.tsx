import { useEffect } from "react";
import { open as openExternal } from "@tauri-apps/plugin-shell";
import { Btn } from "../components/Btn";
import { Row, Section } from "../components/Section";
import { Select } from "../components/Select";
import { SummaryPromptsManager } from "../components/SummaryPromptsManager";
import {
  SUMMARY_MODELS,
  SUMMARY_PROVIDERS,
  inputClass,
} from "../types";
import type { SettingsHook } from "../useSettings";

export function SummaryTab({
  s,
  update,
  llmModels,
  refreshLlmModels,
}: Pick<SettingsHook, "s" | "update" | "llmModels" | "refreshLlmModels">) {
  // Auto-refresh the model list the first time the user lands on Settings
  // with Local provider selected. Without this, the dropdown opens with
  // whatever was saved last and an unhelpful "(not on server)" suffix —
  // before we've even tried to ask the server. Only fires when we have no
  // list yet *and* aren't already loading/erroring, so user-driven Refresh
  // clicks aren't shadowed.
  const isLocal = s.summary_provider === "local";
  const baseUrl = s.local_llm_base_url;
  useEffect(() => {
    if (
      isLocal &&
      !llmModels.list &&
      !llmModels.loading &&
      !llmModels.error
    ) {
      refreshLlmModels(baseUrl);
    }
  }, [isLocal, baseUrl, llmModels.list, llmModels.loading, llmModels.error, refreshLlmModels]);

  // Dropdown suffix logic: only judge the saved model when we actually have
  // a list to check against. Before any refresh has succeeded, render the
  // saved name plain — claiming "(not on server)" before we've checked is
  // what confused our beta tester.
  const savedNotInList =
    llmModels.list !== null &&
    s.local_llm_model !== "" &&
    !llmModels.list.includes(s.local_llm_model);
  const savedSuffix = savedNotInList ? " (not installed)" : "";

  return (
    <>
      <Section title="Provider">
        <Row label="Source">
          <Select
            value={s.summary_provider}
            onChange={(v) => update("summary_provider", v)}
            options={SUMMARY_PROVIDERS}
          />
          <p className="text-xs text-[var(--color-text-muted)] mt-2">
            Local keeps the transcript on your Mac — pick this for confidential
            meetings. Cloud is faster and produces better summaries but sends
            the transcript to OpenAI.
          </p>
        </Row>
        {s.summary_provider === "openai" && (
          <Row label="Model">
            <Select
              value={s.summary_model}
              onChange={(v) => update("summary_model", v)}
              options={SUMMARY_MODELS.map((m) => ({ value: m, label: m }))}
            />
          </Row>
        )}
      </Section>

      {s.summary_provider === "local" && (
        <Section title="Local server">
          <Row label="Server URL">
            <input
              type="text"
              value={s.local_llm_base_url}
              onChange={(e) => update("local_llm_base_url", e.target.value)}
              placeholder="http://localhost:11434/v1"
              className={inputClass}
              style={{ fontFamily: "var(--font-mono)" }}
            />
            <p className="text-xs text-[var(--color-text-muted)] mt-2">
              OpenAI-compatible endpoint. Defaults to Ollama's standard
              port. Also works with LM Studio (<code>http://localhost:1234/v1</code>),
              <code> llama-server</code>, vLLM, and most modern local-LLM
              tools.
            </p>
            <p className="text-xs text-[var(--color-text-muted)] mt-2">
              Don't have one yet?{" "}
              <button
                type="button"
                onClick={() => openExternal("https://ollama.com/download")}
                className="underline hover:text-[var(--color-text)]"
              >
                Install Ollama
              </button>
              , then run <code>ollama pull qwen3.5:4b</code> (or any model
              you prefer) in a Terminal before recording. On 16 GB Macs,
              stick to 4B-class models — 9B and up will OOM during summary.
            </p>
          </Row>
          <Row label="Model">
            <div className="flex items-center gap-2">
              <Select
                value={s.local_llm_model}
                onChange={(v) => update("local_llm_model", v)}
                options={[
                  ...(s.local_llm_model
                    ? [{ value: s.local_llm_model, label: `${s.local_llm_model}${savedSuffix}` }]
                    : []),
                  ...(llmModels.list ?? [])
                    .filter((m) => m !== s.local_llm_model)
                    .map((m) => ({ value: m, label: m })),
                  ...(!s.local_llm_model && !llmModels.list
                    ? [{ value: "", label: "— click Refresh to load —" }]
                    : []),
                ]}
              />
              <Btn
                onClick={() => refreshLlmModels(s.local_llm_base_url)}
                disabled={llmModels.loading}
              >
                {llmModels.loading ? "Loading…" : "Refresh"}
              </Btn>
            </div>
            {llmModels.error?.kind === "unreachable" && (
              <p className="text-xs text-red-600 dark:text-red-400 mt-2">
                Couldn't reach <code>{llmModels.error.baseUrl}</code>. Start
                Ollama (open the Ollama app from Applications, or run{" "}
                <code>ollama serve</code> in Terminal), then click Refresh.
              </p>
            )}
            {llmModels.error?.kind === "other" && (
              <p className="text-xs text-red-600 dark:text-red-400 mt-2 break-all">
                {llmModels.error.message}
              </p>
            )}
            {!llmModels.error && llmModels.list && llmModels.list.length === 0 && (
              <p className="text-xs text-[var(--color-text-muted)] mt-2">
                Server is reachable but no models are installed. Run{" "}
                <code>ollama pull qwen3.5:4b</code> in Terminal, then click
                Refresh.
              </p>
            )}
            {!llmModels.error && savedNotInList && (
              <p className="text-xs text-[var(--color-text-muted)] mt-2">
                <code>{s.local_llm_model}</code> isn't installed on this
                server. Run <code>ollama pull {s.local_llm_model}</code> in
                Terminal, then click Refresh — or pick another model from
                the list.
              </p>
            )}
          </Row>
          <Row label="Thinking mode">
            <label className="flex items-center gap-2 cursor-pointer text-sm">
              <input
                type="checkbox"
                checked={s.local_llm_think === "true"}
                onChange={(e) =>
                  update("local_llm_think", e.target.checked ? "true" : "false")
                }
              />
              Enable Qwen 3+ thinking mode (slower, sometimes higher quality)
            </label>
            <p className="text-xs text-[var(--color-text-muted)] mt-2">
              Off by default — thinking mode makes the model reason
              internally before answering, which can take many minutes
              on a long meeting. Turn on to A/B against the fast path.
              Only applies to Ollama; other servers ignore this.
            </p>
          </Row>
        </Section>
      )}

      <Section title="Summary prompts">
        <SummaryPromptsManager language={s.language} />
      </Section>
    </>
  );
}
