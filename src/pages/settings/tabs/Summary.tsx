import { open as openExternal } from "@tauri-apps/plugin-shell";
import { SUMMARY_PRESETS, presetPromptForLang, presetLabelForLang } from "../../../lib/presets";
import { Btn } from "../components/Btn";
import { Row, Section } from "../components/Section";
import { Select } from "../components/Select";
import {
  SUMMARY_MODELS,
  SUMMARY_PROVIDERS,
  detectActivePreset,
  inputClass,
} from "../types";
import type { SettingsHook } from "../useSettings";

export function SummaryTab({
  s,
  update,
  llmModels,
  refreshLlmModels,
}: Pick<SettingsHook, "s" | "update" | "llmModels" | "refreshLlmModels">) {
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
          {s.summary_provider === "local" && (
            <p className="text-xs text-[var(--color-text-muted)] mt-2">
              Note: post-recording transcript polish is skipped on local —
              it would otherwise regenerate the whole transcript and block
              Ollama for several minutes. Whisper turbo's raw output is the
              summary input.
            </p>
          )}
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
              , then run <code>ollama pull qwen3:4b</code> (or any model
              you prefer) in a Terminal before recording.
            </p>
          </Row>
          <Row label="Model">
            <div className="flex items-center gap-2">
              <Select
                value={s.local_llm_model}
                onChange={(v) => update("local_llm_model", v)}
                options={[
                  ...(s.local_llm_model && !(llmModels.list ?? []).includes(s.local_llm_model)
                    ? [{ value: s.local_llm_model, label: `${s.local_llm_model} (not on server)` }]
                    : []),
                  ...(llmModels.list ?? []).map((m) => ({ value: m, label: m })),
                  ...(!llmModels.list && !s.local_llm_model
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
            {llmModels.error && (
              <p className="text-xs text-red-600 dark:text-red-400 mt-2 break-all">
                {llmModels.error}
              </p>
            )}
            {llmModels.list && llmModels.list.length === 0 && (
              <p className="text-xs text-[var(--color-text-muted)] mt-2">
                Server is reachable but has no models loaded. Run
                <code> ollama pull qwen3:4b</code> (or similar) first.
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

      <Section title="Custom prompt">
        <Row label="Used when a note is set to Custom">
          <p className="text-xs text-[var(--color-text-muted)] mb-2">
            Each note picks a preset (Meeting, 1:1, Lecture, …) from its
            own header. The text below is only used when a note is set to
            "Custom". Use the preset menu to seed it with a known template.
          </p>
          <div className="flex items-center gap-2 mb-2">
            <span className="text-xs text-[var(--color-text-muted)]">Seed from preset:</span>
            <select
              value={detectActivePreset(s.summary_prompt, s.language)}
              onChange={(e) => {
                const preset = SUMMARY_PRESETS.find((p) => p.value === e.target.value);
                if (preset) update("summary_prompt", presetPromptForLang(preset, s.language));
              }}
              className={inputClass + " w-auto py-1 text-xs"}
            >
              {SUMMARY_PRESETS.map((p) => (
                <option key={p.value} value={p.value}>
                  {presetLabelForLang(p, s.language)}
                </option>
              ))}
              <option value="custom" disabled>
                Custom (edited)
              </option>
            </select>
          </div>
          <textarea
            value={s.summary_prompt}
            onChange={(e) => update("summary_prompt", e.target.value)}
            rows={10}
            className={inputClass + " leading-relaxed font-mono text-xs"}
          />
        </Row>
      </Section>
    </>
  );
}
