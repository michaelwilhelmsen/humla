import { useEffect, useState } from "react";
import { ipc, onDiarizeDownloadProgress, onLocalLlmProgress, onLocalWhisperProgress, type DiarizeModelStatus, type DiscoveredLlm, type LocalLlmModelEntry, type LocalLlmStatus, type LocalWhisperStatus, type SettingsKey } from "../lib/ipc";
import { useThemeStore, type Theme } from "../lib/theme";
import { Permissions } from "../components/Permissions";
import { SUMMARY_PRESETS, presetPromptForLang, presetLabelForLang } from "../lib/presets";

type EditableKey = Exclude<SettingsKey, "theme">;

// Returns the value of whichever preset matches the prompt text for the
// current language, or "custom" if the user has typed something else.
function detectActivePreset(prompt: string, lang: string): string {
  for (const p of SUMMARY_PRESETS) {
    if (presetPromptForLang(p, lang) === prompt) return p.value;
  }
  return "custom";
}

const DEFAULTS: Record<EditableKey, string> = {
  language: "no",
  transcribe_provider: "openai",
  transcribe_model: "whisper-1",
  whisper_preset: "quality",
  custom_vocabulary: "",
  summary_model: "gpt-5.4-mini",
  summary_prompt: SUMMARY_PRESETS[0].prompt_no,
  summary_provider: "openai",
  summary_local_model: "managed:qwen-4b",
};

const PROVIDERS_BASE = [
  { value: "openai", label: "OpenAI" },
];
const LOCAL_PROVIDER = { value: "local", label: "Local (Whisper turbo, on-device)" };

const SUMMARY_PROVIDERS = [
  { value: "openai", label: "Cloud (OpenAI)" },
  { value: "local", label: "Local (Gemma / Qwen, on-device)" },
];

// Extra hint after the size in the Model dropdown — flags the recommended
// pick for each tier so users don't have to read all three labels carefully.
function labelSuffix(variant: string): string {
  switch (variant) {
    case "qwen-1.7b":
      return " · ultra-budget";
    case "qwen-4b":
      return " · recommended";
    case "gemma-e4b":
      return " · best quality";
    default:
      return "";
  }
}

const WHISPER_PRESETS = [
  { value: "fast", label: "Fast — lower latency, may drop borderline words" },
  { value: "balanced", label: "Balanced — good speed and accuracy" },
  { value: "quality", label: "Quality — slowest, best for meetings" },
];

const THEMES: { value: Theme; label: string }[] = [
  { value: "system", label: "System" },
  { value: "light", label: "Light" },
  { value: "dark", label: "Dark" },
];

const LANGS = [
  { value: "auto", label: "Auto-detect" },
  { value: "no", label: "Norsk" },
  { value: "en", label: "English" },
  { value: "sv", label: "Svenska" },
  { value: "da", label: "Dansk" },
];

const TRANSCRIBE_MODELS = [
  "whisper-1",
  "gpt-4o-mini-transcribe",
  "gpt-4o-transcribe",
  "gpt-4o-transcribe-diarize",
];
const SUMMARY_MODELS = [
  "gpt-5.4-mini",
  "gpt-5.4",
  "gpt-5.4-nano",
  "gpt-5.5",
  "gpt-5",
  "gpt-5-mini",
  "gpt-5-nano",
  "o3",
];

const inputClass =
  "w-full px-3 py-2 rounded-md text-sm bg-[var(--color-input-bg)] border border-[var(--color-line)] focus:border-[var(--color-text-muted)]";

type Provider = "openai" | "local";
type KeyState = {
  draft: string;
  hasKey: boolean;
  testing: boolean;
  result: null | { ok: true } | { ok: false; message: string };
};

const EMPTY_KEY_STATE: KeyState = { draft: "", hasKey: false, testing: false, result: null };

type LocalState = {
  status: LocalWhisperStatus | null;
  downloading: boolean;
  received: number;
  total: number | null;
  error: string | null;
  flash: string | null;
};

const EMPTY_LOCAL_STATE: LocalState = {
  status: null,
  downloading: false,
  received: 0,
  total: null,
  error: null,
  flash: null,
};

type DiarizeState = {
  status: DiarizeModelStatus | null;
  downloading: boolean;
  fraction: number;
  phase: "listing" | "downloading" | "compiling" | null;
  error: string | null;
  flash: string | null;
};

const EMPTY_DIARIZE_STATE: DiarizeState = {
  status: null,
  downloading: false,
  fraction: 0,
  phase: null,
  error: null,
  flash: null,
};

type LlmState = {
  status: LocalLlmStatus | null;
  downloading: string | null; // variant identifier (e.g. "qwen-4b") or null
  received: number;
  total: number | null;
  scan: DiscoveredLlm[] | null;
  scanning: boolean;
  error: string | null;
  // Transient confirmation message ("Model deleted", "Download complete").
  // Cleared by a setTimeout in the action handler.
  flash: string | null;
};

const EMPTY_LLM_STATE: LlmState = {
  status: null,
  downloading: null,
  received: 0,
  total: null,
  scan: null,
  scanning: false,
  error: null,
  flash: null,
};

export function Settings() {
  const [openaiKey, setOpenaiKey] = useState<KeyState>(EMPTY_KEY_STATE);
  const [local, setLocal] = useState<LocalState>(EMPTY_LOCAL_STATE);
  const [diarize, setDiarize] = useState<DiarizeState>(EMPTY_DIARIZE_STATE);
  const [llm, setLlm] = useState<LlmState>(EMPTY_LLM_STATE);
  const [memoryGb, setMemoryGb] = useState<number>(0);
  const [s, setS] = useState<Record<EditableKey, string>>(DEFAULTS);
  const theme = useThemeStore((t) => t.theme);
  const setThemePref = useThemeStore((t) => t.setTheme);

  useEffect(() => {
    (async () => {
      const [k1, lw, ds, ls] = await Promise.all([
        ipc.getApiKey(),
        ipc.localWhisperStatus(),
        ipc.diarizeStatus().catch(() => null),
        ipc.localLlmStatus().catch(() => null),
      ]);
      setOpenaiKey((p) => ({ ...p, hasKey: !!k1 }));
      setLocal((p) => ({ ...p, status: lw }));
      setDiarize((p) => ({ ...p, status: ds }));
      setLlm((p) => ({ ...p, status: ls }));
      ipc.systemMemoryGb().then(setMemoryGb).catch(() => setMemoryGb(0));
      const entries = await Promise.all(
        (Object.keys(DEFAULTS) as EditableKey[]).map(async (key) => [key, (await ipc.getSetting(key)) ?? DEFAULTS[key]] as const)
      );
      setS(Object.fromEntries(entries) as Record<EditableKey, string>);
    })();
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    onLocalWhisperProgress((p) => {
      setLocal((s) => ({ ...s, received: p.received, total: p.total }));
    }).then((u) => (unlisten = u));
    return () => { unlisten?.(); };
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    onDiarizeDownloadProgress((p) => {
      setDiarize((s) => ({ ...s, fraction: p.fraction, phase: p.phase }));
    }).then((u) => (unlisten = u));
    return () => { unlisten?.(); };
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    onLocalLlmProgress((p) => {
      setLlm((s) => ({ ...s, received: p.received, total: p.total }));
    }).then((u) => (unlisten = u));
    return () => { unlisten?.(); };
  }, []);

  function flashMessage(msg: string) {
    setLlm((p) => ({ ...p, flash: msg }));
    window.setTimeout(() => {
      setLlm((p) => (p.flash === msg ? { ...p, flash: null } : p));
    }, 4000);
  }

  async function downloadLlm(variant: string) {
    const label = llm.status?.models.find((m) => m.variant === variant)?.label ?? variant;
    setLlm((p) => ({ ...p, downloading: variant, received: 0, total: null, error: null, flash: null }));
    try {
      await ipc.localLlmDownload(variant);
      const status = await ipc.localLlmStatus();
      setLlm((p) => ({ ...p, status, downloading: null }));
      flashMessage(`${label} downloaded`);
    } catch (e) {
      const status = await ipc.localLlmStatus().catch(() => null);
      setLlm((p) => ({ ...p, status, downloading: null, error: String(e) }));
    }
  }

  async function deleteLlm(variant: string) {
    // Snapshot before delete so the toast can name what was removed (the
    // status refresh wipes path/label by the time we'd render).
    const before = llm.status?.models.find((m) => m.variant === variant);
    const beforePath = before?.path;
    const beforeLabel = before?.label ?? variant;
    try {
      await ipc.localLlmDelete(variant);
      const status = await ipc.localLlmStatus();
      setLlm((p) => ({ ...p, status, error: null }));
      flashMessage(beforePath ? `Deleted ${beforePath}` : `${beforeLabel} deleted`);
    } catch (e) {
      setLlm((p) => ({ ...p, error: String(e) }));
    }
  }

  async function scanLlm() {
    setLlm((p) => ({ ...p, scanning: true, error: null }));
    try {
      const found = await ipc.localLlmScan();
      // Sort: compatible first, then alphabetical by name.
      found.sort((a, b) => {
        if (a.compatible !== b.compatible) return a.compatible ? -1 : 1;
        return a.name.localeCompare(b.name);
      });
      setLlm((p) => ({ ...p, scanning: false, scan: found }));
    } catch (e) {
      setLlm((p) => ({ ...p, scanning: false, error: String(e) }));
    }
  }

  async function selectExistingLlm(path: string) {
    try {
      await ipc.localLlmSelectExisting(path);
      // Refresh the persisted setting so the radio picks up the path.
      const v = (await ipc.getSetting("summary_local_model")) ?? `path:${path}`;
      setS((prev) => ({ ...prev, summary_local_model: v }));
      setLlm((p) => ({ ...p, error: null }));
    } catch (e) {
      setLlm((p) => ({ ...p, error: String(e) }));
    }
  }

  // Generic flash helper — schedules a 4s clear that only fires if the same
  // message is still showing (so a fresh action doesn't get its toast wiped
  // by a stale timer).
  function flashLocal(msg: string) {
    setLocal((p) => ({ ...p, flash: msg }));
    window.setTimeout(() => {
      setLocal((p) => (p.flash === msg ? { ...p, flash: null } : p));
    }, 4000);
  }
  function flashDiarize(msg: string) {
    setDiarize((p) => ({ ...p, flash: msg }));
    window.setTimeout(() => {
      setDiarize((p) => (p.flash === msg ? { ...p, flash: null } : p));
    }, 4000);
  }

  async function downloadModel() {
    setLocal({ status: null, downloading: true, received: 0, total: null, error: null, flash: null });
    try {
      await ipc.localWhisperDownload();
      const status = await ipc.localWhisperStatus();
      setLocal({ status, downloading: false, received: 0, total: null, error: null, flash: null });
      flashLocal("Whisper model downloaded");
    } catch (e) {
      const status = await ipc.localWhisperStatus().catch(() => null);
      setLocal({ status, downloading: false, received: 0, total: null, error: String(e), flash: null });
    }
  }

  async function deleteModel() {
    const beforePath = local.status?.path;
    try {
      await ipc.localWhisperDelete();
      const status = await ipc.localWhisperStatus();
      setLocal({ status, downloading: false, received: 0, total: null, error: null, flash: null });
      flashLocal(beforePath ? `Deleted ${beforePath}` : "Whisper model deleted");
    } catch (e) {
      setLocal((p) => ({ ...p, error: String(e) }));
    }
  }

  async function downloadDiarize() {
    setDiarize({ status: null, downloading: true, fraction: 0, phase: null, error: null, flash: null });
    try {
      await ipc.diarizeDownload();
      const status = await ipc.diarizeStatus();
      setDiarize({ status, downloading: false, fraction: 0, phase: null, error: null, flash: null });
      flashDiarize("Speaker diarization model downloaded");
    } catch (e) {
      const status = await ipc.diarizeStatus().catch(() => null);
      setDiarize({ status, downloading: false, fraction: 0, phase: null, error: String(e), flash: null });
    }
  }

  async function deleteDiarize() {
    const beforePath = diarize.status?.path;
    try {
      await ipc.diarizeDelete();
      const status = await ipc.diarizeStatus();
      setDiarize({ status, downloading: false, fraction: 0, phase: null, error: null, flash: null });
      flashDiarize(beforePath ? `Deleted ${beforePath}` : "Speaker diarization model deleted");
    } catch (e) {
      setDiarize((p) => ({ ...p, error: String(e) }));
    }
  }

  async function update(key: EditableKey, value: string) {
    setS((prev) => ({ ...prev, [key]: value }));
    await ipc.setSetting(key, value);
  }

  async function saveKey() {
    if (!openaiKey.draft.trim()) return;
    await ipc.setApiKey(openaiKey.draft.trim());
    setOpenaiKey({ draft: "", hasKey: true, testing: false, result: null });
  }

  async function testKey() {
    setOpenaiKey((p) => ({ ...p, testing: true }));
    try {
      const r = await ipc.testApiKey();
      const result = r.ok
        ? ({ ok: true } as const)
        : ({ ok: false, message: `${r.status}: ${r.error ?? "unknown error"}` } as const);
      setOpenaiKey((p) => ({ ...p, testing: false, result }));
    } catch (e) {
      setOpenaiKey((p) => ({ ...p, testing: false, result: { ok: false, message: String(e) } }));
    }
  }

  const provider: Provider = (s.transcribe_provider as Provider) ?? "openai";

  return (
    <div className="h-full overflow-y-auto">
      <div className="max-w-2xl mx-auto px-12 py-16">
        <h1 className="text-5xl font-light tracking-[-0.02em] mb-12">Settings</h1>

        <Section title="Permissions">
          <Permissions />
        </Section>

        <Section title="Appearance">
          <Row label="Theme">
            <div className="flex gap-1 p-1 rounded-md border border-[var(--color-line-visible)] bg-[var(--color-surface)] w-fit">
              {THEMES.map((t) => (
                <button
                  key={t.value}
                  onClick={() => setThemePref(t.value)}
                  className={
                    "px-3 py-1 rounded text-sm " +
                    (theme === t.value
                      ? "bg-[var(--color-surface)] shadow-sm"
                      : "text-[var(--color-text-muted)] hover:text-[var(--color-text)]")
                  }
                >
                  {t.label}
                </button>
              ))}
            </div>
          </Row>
        </Section>

        <Section title="API keys">
          <Row label="OpenAI">
            <ApiKeyField
              state={openaiKey}
              setState={setOpenaiKey}
              placeholder="sk-…"
              onSave={saveKey}
              onTest={testKey}
            />
          </Row>
        </Section>

        <Section title="Transcription">
          <Row label="Provider">
            <Select
              value={provider}
              onChange={(v) => update("transcribe_provider", v)}
              options={
                local.status?.downloaded
                  ? [...PROVIDERS_BASE, LOCAL_PROVIDER]
                  : PROVIDERS_BASE
              }
            />
            {provider === "local" && !local.status?.downloaded && (
              <p className="text-xs text-red-600 dark:text-red-400 mt-2">
                The local model isn't downloaded. Download it below before recording.
              </p>
            )}
          </Row>
          <Row label="Language">
            <Select value={s.language} onChange={(v) => update("language", v)} options={LANGS} />
          </Row>
          {provider === "openai" && (
            <Row label="Model">
              <Select
                value={s.transcribe_model}
                onChange={(v) => update("transcribe_model", v)}
                options={TRANSCRIBE_MODELS.map((m) => ({ value: m, label: m }))}
              />
              {s.transcribe_model === "gpt-4o-transcribe-diarize" && (
                <p className="text-xs text-[var(--color-text-muted)] mt-2">
                  Note: <code>gpt-4o-transcribe-diarize</code> treats the language
                  setting as a hint and does not accept a biasing prompt. For
                  strict language locking, use <code>whisper-1</code> or
                  <code> gpt-4o-transcribe</code>.
                </p>
              )}
            </Row>
          )}
          {provider === "local" && (
            <Row label="Quality preset">
              <Select
                value={s.whisper_preset}
                onChange={(v) => update("whisper_preset", v)}
                options={WHISPER_PRESETS}
              />
              <p className="text-xs text-[var(--color-text-muted)] mt-2">
                Trades latency for accuracy. Quality runs beam search with
                an aggressive no-speech threshold so almost no segments are
                silently dropped — best for meetings and dense speech. Fast
                falls back to greedy decoding for live-caption snappiness.
              </p>
            </Row>
          )}
          <Row label="Local model">
            <LocalModelManager
              state={local}
              onDownload={downloadModel}
              onDelete={deleteModel}
            />
          </Row>
          <Row label="Speaker diarization">
            <DiarizeModelManager
              state={diarize}
              onDownload={downloadDiarize}
              onDelete={deleteDiarize}
            />
            <p className="text-xs text-[var(--color-text-muted)] mt-2">
              When downloaded, every recording is automatically tagged with
              <code> Speaker 1: </code>, <code> Speaker 2: </code> labels
              before polishing. Runs locally via CoreML / Apple Neural Engine.
            </p>
          </Row>
          <Row label="Custom vocabulary">
            <textarea
              value={s.custom_vocabulary}
              onChange={(e) => update("custom_vocabulary", e.target.value)}
              rows={3}
              placeholder="Tauri, Humla, ScreenCaptureKit, Granola"
              className={inputClass + " leading-relaxed"}
              style={{ fontFamily: "var(--font-mono)" }}
            />
            <p className="text-xs text-[var(--color-text-muted)] mt-2">
              Comma- or newline-separated. Names, jargon, and uncommon
              spellings — biases the transcriber toward these tokens.
              <code> gpt-4o-transcribe-diarize </code> ignores it.
            </p>
          </Row>
        </Section>

        <Section title="Summary">
          <Row label="Provider">
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
          {s.summary_provider === "local" && (
            <>
              <Row label="Model">
                <Select
                  value={s.summary_local_model}
                  onChange={(v) => {
                    if (v.startsWith("path:")) {
                      // Route through the validation command — it sniffs the
                      // GGUF and rejects incompatible files before persisting.
                      selectExistingLlm(v.slice(5));
                    } else {
                      update("summary_local_model", v);
                    }
                  }}
                  options={[
                    ...(llm.status?.models ?? []).map((m) => ({
                      value: `managed:${m.variant}`,
                      label: `${m.label} — ${formatBytes(m.bytesHint)}${labelSuffix(m.variant)}`,
                    })),
                    ...(llm.scan ?? [])
                      .filter((m) => m.compatible)
                      .map((m) => ({
                        value: `path:${m.path}`,
                        label: `${m.name} — ${m.source} · ${m.architecture} ${m.quantization}`,
                      })),
                  ]}
                />
                {memoryGb > 0 && memoryGb <= 16 && s.summary_local_model === "managed:gemma-e4b" && (
                  <p className="text-xs text-[var(--color-text-muted)] mt-2">
                    Heads up: your Mac has {memoryGb} GB of RAM. Gemma 4 E4B
                    uses ~5 GB resident during summary; with Whisper and your
                    browser already loaded that may swap. Qwen 3 4B is the
                    safer pick on 16 GB systems.
                  </p>
                )}
              </Row>
              <Row label="Local model">
                <LocalLlmModelManager
                  state={llm}
                  selected={s.summary_local_model}
                  onDownload={downloadLlm}
                  onDelete={deleteLlm}
                />
              </Row>
              <Row label="Already installed?">
                <div className="flex flex-col gap-2">
                  <div className="flex gap-2">
                    <Btn onClick={scanLlm} disabled={llm.scanning}>
                      {llm.scanning ? "Scanning…" : "Scan LM Studio / Ollama / HF"}
                    </Btn>
                  </div>
                  {llm.scan && llm.scan.length === 0 && (
                    <p className="text-xs text-[var(--color-text-muted)]">
                      No compatible GGUFs found. We look in <code>~/.cache/lm-studio</code>,
                      <code> ~/.ollama</code>, and <code>~/.cache/huggingface</code>.
                    </p>
                  )}
                  {llm.scan && llm.scan.length > 0 && (
                    <p className="text-xs text-[var(--color-text-muted)]">
                      {llm.scan.filter((m) => m.compatible).length} compatible model(s)
                      added to the Model dropdown above.
                    </p>
                  )}
                </div>
              </Row>
            </>
          )}
          <Row label="Custom prompt">
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

      </div>
    </div>
  );
}

// Status + actions for the currently-selected local LLM. Mirrors
// LocalModelManager's three states (downloading / installed / not-yet) so
// the Settings UI feels uniform across Whisper / Diarize / Local LLM.
//
// `selected` is the persisted setting string. Custom paths render a minimal
// status with no managed actions; managed variants pluck the matching entry
// out of state.status.models and act on it.
function LocalLlmModelManager({
  state,
  selected,
  onDownload,
  onDelete,
}: {
  state: LlmState;
  selected: string;
  onDownload: (variant: string) => void;
  onDelete: (variant: string) => void;
}) {
  if (selected.startsWith("path:")) {
    const path = selected.slice(5);
    return (
      <div className="flex flex-col gap-2">
        <div className="text-sm break-all">Using {path}</div>
        <p className="text-xs text-[var(--color-text-muted)]">
          External file managed outside Humla. If you remove or rename it,
          summaries will fail until you re-pick a model.
        </p>
        {state.flash && (
          <p className="text-xs px-2 py-1 rounded bg-[var(--color-pill-hover)] inline-block break-all" role="status">
            {state.flash}
          </p>
        )}
        {state.error && (
          <p className="text-sm text-red-600 dark:text-red-400 break-all">{state.error}</p>
        )}
      </div>
    );
  }

  const variant = selected.startsWith("managed:") ? selected.slice(8) : null;
  const entry: LocalLlmModelEntry | undefined =
    state.status?.models.find((m) => m.variant === variant);
  if (!variant || !entry) {
    return (
      <p className="text-sm text-[var(--color-text-muted)]">
        Pick a model from the dropdown above.
      </p>
    );
  }

  if (state.downloading === variant) {
    const total = state.total ?? entry.bytesHint;
    const pct = total ? Math.min(100, (state.received / total) * 100) : null;
    return (
      <div className="flex flex-col gap-2">
        <div className="text-sm">
          Downloading {entry.label}{total ? ` ${formatBytes(state.received)} / ${formatBytes(total)}` : ` ${formatBytes(state.received)}`}…
        </div>
        <div className="h-1.5 rounded bg-[var(--color-pill-hover)] overflow-hidden">
          <div
            className="h-full bg-[var(--color-text-muted)] transition-[width] duration-150"
            style={{ width: pct === null ? "30%" : `${pct}%` }}
          />
        </div>
      </div>
    );
  }

  if (entry.downloaded) {
    return (
      <div className="flex flex-col gap-2">
        <div className="text-sm">
          Downloaded — {entry.label}
          {entry.sizeBytes ? ` (${formatBytes(entry.sizeBytes)})` : ""}
        </div>
        {entry.path && (
          <div className="text-xs text-[var(--color-text-muted)] font-mono break-all">
            {entry.path}
          </div>
        )}
        <div className="flex gap-2">
          <Btn onClick={() => onDelete(variant)}>Delete model</Btn>
        </div>
        {state.flash && (
          <p className="text-xs px-2 py-1 rounded bg-[var(--color-pill-hover)] inline-block break-all" role="status">
            {state.flash}
          </p>
        )}
        {state.error && (
          <p className="text-sm text-red-600 dark:text-red-400 break-all">{state.error}</p>
        )}
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-2">
      <div className="text-sm">
        Not downloaded. {entry.label} is {formatBytes(entry.bytesHint)} and runs
        on-device with Metal.
      </div>
      <div className="flex gap-2">
        <Btn onClick={() => onDownload(variant)}>Download model</Btn>
      </div>
      {state.flash && (
        <p className="text-xs px-2 py-1 rounded bg-[var(--color-pill-hover)] inline-block break-all" role="status">
          {state.flash}
        </p>
      )}
      {state.error && (
        <p className="text-sm text-red-600 dark:text-red-400 break-all">{state.error}</p>
      )}
    </div>
  );
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className="mb-12">
      <h2 className="nd-label mb-5">{title}</h2>
      <div className="flex flex-col gap-5">{children}</div>
    </section>
  );
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div>
      <div className="text-sm mb-1.5">{label}</div>
      {children}
    </div>
  );
}

function Select({
  value, onChange, options,
}: {
  value: string;
  onChange: (v: string) => void;
  options: { value: string; label: string }[];
}) {
  return (
    <select
      value={value}
      onChange={(e) => onChange(e.target.value)}
      className={inputClass + " w-auto min-w-[180px]"}
    >
      {options.map((o) => (
        <option key={o.value} value={o.value}>{o.label}</option>
      ))}
    </select>
  );
}

function formatBytes(n: number) {
  if (n >= 1024 * 1024 * 1024) return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
  if (n >= 1024 * 1024) return `${(n / 1024 / 1024).toFixed(0)} MB`;
  if (n >= 1024) return `${(n / 1024).toFixed(0)} KB`;
  return `${n} B`;
}

function LocalModelManager({
  state,
  onDownload,
  onDelete,
}: {
  state: LocalState;
  onDownload: () => void;
  onDelete: () => void;
}) {
  const total = state.total ?? null;
  const pct = state.downloading && total ? Math.min(100, (state.received / total) * 100) : null;

  if (state.downloading) {
    return (
      <div className="flex flex-col gap-2">
        <div className="text-sm">
          Downloading{total ? ` ${formatBytes(state.received)} / ${formatBytes(total)}` : ` ${formatBytes(state.received)}`}…
        </div>
        <div className="h-1.5 rounded bg-[var(--color-pill-hover)] overflow-hidden">
          <div
            className="h-full bg-[var(--color-text-muted)] transition-[width] duration-150"
            style={{ width: pct === null ? "30%" : `${pct}%` }}
          />
        </div>
      </div>
    );
  }

  if (state.status?.downloaded) {
    return (
      <div className="flex flex-col gap-2">
        <div className="text-sm">
          Downloaded — Whisper large-v3-turbo Q5_0
          {state.status.sizeBytes ? ` (${formatBytes(state.status.sizeBytes)})` : ""}
        </div>
        {state.status.path && (
          <div className="text-xs text-[var(--color-text-muted)] font-mono break-all">
            {state.status.path}
          </div>
        )}
        <div className="flex gap-2">
          <Btn onClick={onDelete}>Delete model</Btn>
        </div>
        {state.flash && (
          <p className="text-xs px-2 py-1 rounded bg-[var(--color-pill-hover)] inline-block break-all" role="status">
            {state.flash}
          </p>
        )}
        {state.error && (
          <p className="text-sm text-red-600 dark:text-red-400 break-all">{state.error}</p>
        )}
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-2">
      <div className="text-sm">
        Not downloaded. The model is ~547 MB and runs on-device with Metal.
      </div>
      <div className="flex gap-2">
        <Btn onClick={onDownload}>Download model</Btn>
      </div>
      {state.flash && (
        <p className="text-xs px-2 py-1 rounded bg-[var(--color-pill-hover)] inline-block break-all" role="status">
          {state.flash}
        </p>
      )}
      {state.error && (
        <p className="text-sm text-red-600 dark:text-red-400 break-all">{state.error}</p>
      )}
    </div>
  );
}

function DiarizeModelManager({
  state,
  onDownload,
  onDelete,
}: {
  state: DiarizeState;
  onDownload: () => void;
  onDelete: () => void;
}) {
  if (state.downloading) {
    const pct = Math.min(100, state.fraction * 100);
    const phaseLabel =
      state.phase === "compiling"
        ? "Compiling for Apple Neural Engine"
        : state.phase === "listing"
        ? "Listing files"
        : "Downloading models";
    return (
      <div className="flex flex-col gap-2">
        <div className="text-sm">
          {phaseLabel}… {state.phase === "compiling" ? "" : `${pct.toFixed(0)}%`}
        </div>
        <div className="h-1.5 rounded bg-[var(--color-pill-hover)] overflow-hidden">
          <div
            className="h-full bg-[var(--color-text-muted)] transition-[width] duration-150"
            style={{ width: state.phase === "compiling" ? "100%" : `${pct}%` }}
          />
        </div>
      </div>
    );
  }

  if (state.status?.downloaded) {
    return (
      <div className="flex flex-col gap-2">
        <div className="text-sm">
          Downloaded — FluidAudio diarization (CoreML)
          {state.status.sizeBytes ? ` (${formatBytes(state.status.sizeBytes)})` : ""}
        </div>
        {state.status.path && (
          <div className="text-xs text-[var(--color-text-muted)] font-mono break-all">
            {state.status.path}
          </div>
        )}
        <div className="flex gap-2">
          <Btn onClick={onDelete}>Delete model</Btn>
        </div>
        {state.flash && (
          <p className="text-xs px-2 py-1 rounded bg-[var(--color-pill-hover)] inline-block break-all" role="status">
            {state.flash}
          </p>
        )}
        {state.error && (
          <p className="text-sm text-red-600 dark:text-red-400 break-all">{state.error}</p>
        )}
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-2">
      <div className="text-sm">
        Not downloaded. The model is ~15 MB. First-time setup also compiles
        for the Apple Neural Engine, which takes 20-30 s.
      </div>
      <div className="flex gap-2">
        <Btn onClick={onDownload}>Download model</Btn>
      </div>
      {state.flash && (
        <p className="text-xs px-2 py-1 rounded bg-[var(--color-pill-hover)] inline-block break-all" role="status">
          {state.flash}
        </p>
      )}
      {state.error && (
        <p className="text-sm text-red-600 dark:text-red-400 break-all">{state.error}</p>
      )}
    </div>
  );
}

function ApiKeyField({
  state,
  setState,
  placeholder,
  onSave,
  onTest,
}: {
  state: KeyState;
  setState: React.Dispatch<React.SetStateAction<KeyState>>;
  placeholder: string;
  onSave: () => void;
  onTest: () => void;
}) {
  return (
    <>
      <div className="flex gap-2">
        <input
          type="password"
          value={state.draft}
          onChange={(e) => setState((p) => ({ ...p, draft: e.target.value }))}
          placeholder={state.hasKey ? "•••••••• stored" : placeholder}
          className={inputClass + " flex-1"}
        />
        <Btn onClick={onSave} disabled={!state.draft.trim()}>Save</Btn>
        <Btn onClick={onTest} disabled={!state.hasKey || state.testing}>
          {state.testing ? "Testing…" : "Test"}
        </Btn>
      </div>
      {state.result?.ok === true && (
        <p className="text-sm text-green-600 dark:text-green-400 mt-2">Connected ✓</p>
      )}
      {state.result?.ok === false && (
        <p className="text-sm text-red-600 dark:text-red-400 mt-2 break-all">
          {state.result.message}
        </p>
      )}
    </>
  );
}

function Btn({ children, ...props }: React.ButtonHTMLAttributes<HTMLButtonElement>) {
  return (
    <button
      {...props}
      className="px-3 py-2 rounded-md text-sm border border-[var(--color-line-visible)] bg-[var(--color-surface)] hover:border-[var(--color-text)] hover:bg-[var(--color-pill-hover)] disabled:opacity-50 disabled:hover:border-[var(--color-line-visible)] transition-colors"
    >
      {children}
    </button>
  );
}
