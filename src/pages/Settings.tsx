import { useEffect, useState } from "react";
import { ipc, onLocalWhisperProgress, type LocalWhisperStatus, type SettingsKey } from "../lib/ipc";
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
  speechmatics_operating_point: "enhanced",
  speechmatics_region: "eu1",
  summary_model: "gpt-5.4-mini",
  summary_prompt: SUMMARY_PRESETS[0].prompt_no,
};

const PROVIDERS_BASE = [
  { value: "openai", label: "OpenAI" },
  { value: "speechmatics", label: "Speechmatics" },
];
const LOCAL_PROVIDER = { value: "local", label: "Local (Whisper turbo, on-device)" };

const SPEECHMATICS_OPS = [
  { value: "enhanced", label: "Enhanced (higher accuracy)" },
  { value: "standard", label: "Standard (faster)" },
];

const SPEECHMATICS_REGIONS = [
  { value: "eu1", label: "EU1 (Europe, self-serve)" },
  { value: "eu2", label: "EU2 (Europe, Enterprise)" },
  { value: "us1", label: "US1 (USA, self-serve)" },
  { value: "us2", label: "US2 (USA, Enterprise)" },
  { value: "au1", label: "AU1 (Australia)" },
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

type Provider = "openai" | "speechmatics" | "local";
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

export function Settings() {
  const [openaiKey, setOpenaiKey] = useState<KeyState>(EMPTY_KEY_STATE);
  const [smKey, setSmKey] = useState<KeyState>(EMPTY_KEY_STATE);
  const [local, setLocal] = useState<LocalState>(EMPTY_LOCAL_STATE);
  const [s, setS] = useState<Record<EditableKey, string>>(DEFAULTS);
  const theme = useThemeStore((t) => t.theme);
  const setThemePref = useThemeStore((t) => t.setTheme);

  useEffect(() => {
    (async () => {
      const [k1, k2, lw] = await Promise.all([
        ipc.getApiKey(),
        ipc.getSpeechmaticsKey(),
        ipc.localWhisperStatus(),
      ]);
      setOpenaiKey((p) => ({ ...p, hasKey: !!k1 }));
      setSmKey((p) => ({ ...p, hasKey: !!k2 }));
      setLocal((p) => ({ ...p, status: lw }));
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

  async function update(key: EditableKey, value: string) {
    setS((prev) => ({ ...prev, [key]: value }));
    await ipc.setSetting(key, value);
  }

  async function saveKey(provider: Provider) {
    const setter = provider === "openai" ? setOpenaiKey : setSmKey;
    const state = provider === "openai" ? openaiKey : smKey;
    if (!state.draft.trim()) return;
    if (provider === "openai") await ipc.setApiKey(state.draft.trim());
    else await ipc.setSpeechmaticsKey(state.draft.trim());
    setter({ draft: "", hasKey: true, testing: false, result: null });
  }

  async function testKey(provider: Provider) {
    const setter = provider === "openai" ? setOpenaiKey : setSmKey;
    setter((p) => ({ ...p, testing: true }));
    try {
      const r = provider === "openai" ? await ipc.testApiKey() : await ipc.testSpeechmaticsKey();
      const result = r.ok
        ? ({ ok: true } as const)
        : ({ ok: false, message: `${r.status}: ${r.error ?? "unknown error"}` } as const);
      setter((p) => ({ ...p, testing: false, result }));
    } catch (e) {
      setter((p) => ({ ...p, testing: false, result: { ok: false, message: String(e) } }));
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
              onSave={() => saveKey("openai")}
              onTest={() => testKey("openai")}
            />
          </Row>
          <Row label="Speechmatics">
            <ApiKeyField
              state={smKey}
              setState={setSmKey}
              placeholder="Speechmatics API key"
              onSave={() => saveKey("speechmatics")}
              onTest={() => testKey("speechmatics")}
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
          {provider === "speechmatics" && (
            <>
              <Row label="Region">
                <Select
                  value={s.speechmatics_region}
                  onChange={(v) => update("speechmatics_region", v)}
                  options={SPEECHMATICS_REGIONS}
                />
                <p className="text-xs text-[var(--color-text-muted)] mt-2">
                  A key from one region returns 401 on another. Self-serve keys
                  are typically EU1; check your Speechmatics portal if unsure.
                </p>
              </Row>
              <Row label="Operating point">
                <Select
                  value={s.speechmatics_operating_point}
                  onChange={(v) => update("speechmatics_operating_point", v)}
                  options={SPEECHMATICS_OPS}
                />
                <p className="text-xs text-[var(--color-text-muted)] mt-2">
                  Speechmatics processes each chunk as a batch job (submit → poll →
                  fetch). Expect a few seconds of extra latency vs. OpenAI.
                </p>
              </Row>
            </>
          )}
          <Row label="Local model">
            <LocalModelManager
              state={local}
              onDownload={downloadModel}
              onDelete={deleteModel}
            />
          </Row>
        </Section>

        <Section title="Summary">
          <Row label="Model">
            <Select
              value={s.summary_model}
              onChange={(v) => update("summary_model", v)}
              options={SUMMARY_MODELS.map((m) => ({ value: m, label: m }))}
            />
          </Row>
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
