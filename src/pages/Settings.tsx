import { useEffect, useState } from "react";
import { ipc, onDiarizeDownloadProgress, onLocalLlmProgress, onLocalWhisperProgress, type DiarizeModelStatus, type DiscoveredLlm, type LocalLlmStatus, type LocalWhisperStatus, type SettingsKey } from "../lib/ipc";
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
  summary_local_model: "managed:e4b",
};

const PROVIDERS_BASE = [
  { value: "openai", label: "OpenAI" },
];
const LOCAL_PROVIDER = { value: "local", label: "Local (Whisper turbo, on-device)" };

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
};

const EMPTY_LOCAL_STATE: LocalState = {
  status: null,
  downloading: false,
  received: 0,
  total: null,
  error: null,
};

type DiarizeState = {
  status: DiarizeModelStatus | null;
  downloading: boolean;
  fraction: number;
  phase: "listing" | "downloading" | "compiling" | null;
  error: string | null;
};

const EMPTY_DIARIZE_STATE: DiarizeState = {
  status: null,
  downloading: false,
  fraction: 0,
  phase: null,
  error: null,
};

type LlmState = {
  status: LocalLlmStatus | null;
  downloading: "e2b" | "e4b" | null;
  received: number;
  total: number | null;
  scan: DiscoveredLlm[] | null;
  scanning: boolean;
  error: string | null;
};

const EMPTY_LLM_STATE: LlmState = {
  status: null,
  downloading: null,
  received: 0,
  total: null,
  scan: null,
  scanning: false,
  error: null,
};

export function Settings() {
  const [openaiKey, setOpenaiKey] = useState<KeyState>(EMPTY_KEY_STATE);
  const [local, setLocal] = useState<LocalState>(EMPTY_LOCAL_STATE);
  const [diarize, setDiarize] = useState<DiarizeState>(EMPTY_DIARIZE_STATE);
  const [llm, setLlm] = useState<LlmState>(EMPTY_LLM_STATE);
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

  async function downloadLlm(variant: "e2b" | "e4b") {
    setLlm((p) => ({ ...p, downloading: variant, received: 0, total: null, error: null }));
    try {
      await ipc.localLlmDownload(variant);
      const status = await ipc.localLlmStatus();
      setLlm((p) => ({ ...p, status, downloading: null }));
    } catch (e) {
      const status = await ipc.localLlmStatus().catch(() => null);
      setLlm((p) => ({ ...p, status, downloading: null, error: String(e) }));
    }
  }

  async function deleteLlm(variant: "e2b" | "e4b") {
    try {
      await ipc.localLlmDelete(variant);
      const status = await ipc.localLlmStatus();
      setLlm((p) => ({ ...p, status, error: null }));
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

  async function downloadModel() {
    setLocal({ status: null, downloading: true, received: 0, total: null, error: null });
    try {
      await ipc.localWhisperDownload();
      const status = await ipc.localWhisperStatus();
      setLocal({ status, downloading: false, received: 0, total: null, error: null });
    } catch (e) {
      const status = await ipc.localWhisperStatus().catch(() => null);
      setLocal({ status, downloading: false, received: 0, total: null, error: String(e) });
    }
  }

  async function deleteModel() {
    try {
      await ipc.localWhisperDelete();
      const status = await ipc.localWhisperStatus();
      setLocal({ status, downloading: false, received: 0, total: null, error: null });
    } catch (e) {
      setLocal((p) => ({ ...p, error: String(e) }));
    }
  }

  async function downloadDiarize() {
    setDiarize({ status: null, downloading: true, fraction: 0, phase: null, error: null });
    try {
      await ipc.diarizeDownload();
      const status = await ipc.diarizeStatus();
      setDiarize({ status, downloading: false, fraction: 0, phase: null, error: null });
    } catch (e) {
      const status = await ipc.diarizeStatus().catch(() => null);
      setDiarize({ status, downloading: false, fraction: 0, phase: null, error: String(e) });
    }
  }

  async function deleteDiarize() {
    try {
      await ipc.diarizeDelete();
      const status = await ipc.diarizeStatus();
      setDiarize({ status, downloading: false, fraction: 0, phase: null, error: null });
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
            <div className="flex gap-2">
              {[
                { value: "openai", label: "Cloud (OpenAI)" },
                { value: "local", label: "Local (Gemma 4, on-device)" },
              ].map((opt) => (
                <button
                  key={opt.value}
                  onClick={() => update("summary_provider", opt.value)}
                  className={
                    "px-3 py-1.5 rounded-md text-sm border " +
                    (s.summary_provider === opt.value
                      ? "border-[var(--color-text)] bg-[var(--color-text)] text-[var(--color-bg)]"
                      : "border-[var(--color-line)]")
                  }
                >
                  {opt.label}
                </button>
              ))}
            </div>
            <p className="text-xs text-[var(--color-text-muted)] mt-2">
              Local keeps the transcript on your Mac — pick this for confidential
              meetings. Cloud is faster and produces better summaries but sends
              the transcript to OpenAI.
            </p>
          </Row>
          {s.summary_provider !== "local" && (
            <Row label="Model">
              <Select
                value={s.summary_model}
                onChange={(v) => update("summary_model", v)}
                options={SUMMARY_MODELS.map((m) => ({ value: m, label: m }))}
              />
            </Row>
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

        {s.summary_provider === "local" && (
          <Section title="Local summarization model">
            <p className="text-xs text-[var(--color-text-muted)] -mt-2">
              Gemma 4 runs entirely on your Mac. Pick a size, or scan for
              models you already have installed via LM Studio or Ollama.
            </p>
            <ManagedLlmRow
              label="Gemma 4 E2B"
              hint="~2.9 GB · Q8_0 · faster"
              variant="e2b"
              downloaded={!!llm.status?.e2bDownloaded}
              size={llm.status?.e2bSizeBytes ?? null}
              selected={s.summary_local_model === "managed:e2b"}
              progress={llm.downloading === "e2b" ? llm : null}
              onSelect={() => update("summary_local_model", "managed:e2b")}
              onDownload={() => downloadLlm("e2b")}
              onDelete={() => deleteLlm("e2b")}
            />
            <ManagedLlmRow
              label="Gemma 4 E4B"
              hint="~5.0 GB · Q4_K_M · recommended"
              variant="e4b"
              downloaded={!!llm.status?.e4bDownloaded}
              size={llm.status?.e4bSizeBytes ?? null}
              selected={s.summary_local_model === "managed:e4b"}
              progress={llm.downloading === "e4b" ? llm : null}
              onSelect={() => update("summary_local_model", "managed:e4b")}
              onDownload={() => downloadLlm("e4b")}
              onDelete={() => deleteLlm("e4b")}
            />
            <Row label="Already installed?">
              <button
                onClick={scanLlm}
                disabled={llm.scanning}
                className="px-3 py-1.5 rounded-md text-sm border border-[var(--color-line)] disabled:opacity-50"
              >
                {llm.scanning ? "Scanning…" : "Scan LM Studio / Ollama / HF"}
              </button>
              {llm.scan && llm.scan.length === 0 && (
                <p className="text-xs text-[var(--color-text-muted)] mt-2">
                  No compatible models found. We look in <code>~/.cache/lm-studio</code>,
                  <code> ~/.ollama</code>, and <code>~/.cache/huggingface</code>.
                </p>
              )}
              {llm.scan && llm.scan.length > 0 && (
                <div className="mt-3 space-y-2">
                  {llm.scan.map((m) => {
                    const active = s.summary_local_model === `path:${m.path}`;
                    const sizeGb = (m.sizeBytes / 1e9).toFixed(1);
                    return (
                      <div
                        key={m.path}
                        className="flex items-center justify-between rounded-md border border-[var(--color-line)] px-3 py-2 text-sm"
                      >
                        <div className="min-w-0 flex-1">
                          <div className="truncate" title={m.path}>{m.name}</div>
                          <div className="text-xs text-[var(--color-text-muted)]">
                            {m.source} · {m.architecture} {m.quantization} · {sizeGb} GB
                            {!m.compatible && " · incompatible"}
                          </div>
                        </div>
                        <button
                          onClick={() => selectExistingLlm(m.path)}
                          disabled={!m.compatible || active}
                          className="px-2 py-1 rounded text-xs border border-[var(--color-line)] disabled:opacity-40 ml-3 shrink-0"
                        >
                          {active ? "Active" : "Use this"}
                        </button>
                      </div>
                    );
                  })}
                </div>
              )}
            </Row>
            {llm.error && (
              <p className="text-xs text-[var(--color-accent)]">Error: {llm.error}</p>
            )}
          </Section>
        )}
      </div>
    </div>
  );
}

function ManagedLlmRow({
  label,
  hint,
  variant,
  downloaded,
  size,
  selected,
  progress,
  onSelect,
  onDownload,
  onDelete,
}: {
  label: string;
  hint: string;
  variant: "e2b" | "e4b";
  downloaded: boolean;
  size: number | null;
  selected: boolean;
  progress: LlmState | null;
  onSelect: () => void;
  onDownload: () => void;
  onDelete: () => void;
}) {
  const sizeGb = size != null ? (size / 1e9).toFixed(1) : null;
  const downloading = progress?.downloading === variant;
  const fraction =
    progress && progress.total && progress.total > 0
      ? progress.received / progress.total
      : 0;
  return (
    <div className="flex items-center justify-between gap-3 rounded-md border border-[var(--color-line)] px-3 py-2 text-sm">
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <span>{label}</span>
          {selected && (
            <span className="text-xs px-1.5 py-0.5 rounded bg-[var(--color-text)] text-[var(--color-bg)]">
              Active
            </span>
          )}
        </div>
        <div className="text-xs text-[var(--color-text-muted)]">
          {hint}
          {downloaded && sizeGb && ` · installed (${sizeGb} GB)`}
        </div>
        {downloading && (
          <div className="text-xs mt-1">
            Downloading… {Math.round(fraction * 100)}%
          </div>
        )}
      </div>
      <div className="flex gap-2 shrink-0">
        {!downloaded && !downloading && (
          <button
            onClick={onDownload}
            className="px-2 py-1 rounded text-xs border border-[var(--color-line)]"
          >
            Download
          </button>
        )}
        {downloaded && !selected && (
          <button
            onClick={onSelect}
            className="px-2 py-1 rounded text-xs border border-[var(--color-line)]"
          >
            Use
          </button>
        )}
        {downloaded && (
          <button
            onClick={onDelete}
            className="px-2 py-1 rounded text-xs border border-[var(--color-line)]"
          >
            Delete
          </button>
        )}
      </div>
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
        <div className="flex gap-2">
          <Btn onClick={onDelete}>Delete model</Btn>
        </div>
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
        <div className="flex gap-2">
          <Btn onClick={onDelete}>Delete model</Btn>
        </div>
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
