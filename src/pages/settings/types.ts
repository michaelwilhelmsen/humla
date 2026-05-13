// Shared types and option lists for the Settings page.
//
// Most of the constants here are options users can pick from a Select.
// They live next to the page rather than in a global module because
// they're tightly coupled to how the form is shaped — moving them out
// would create a layer of indirection without buying us reuse.

import type { DiarizeModelStatus, LocalWhisperModelStatus, SettingsKey } from "../../lib/ipc";
import { SUMMARY_PRESETS, presetPromptForLang } from "../../lib/presets";
import type { Theme } from "../../lib/theme";
import type { Palette } from "../../lib/palette";

export type EditableKey = Exclude<SettingsKey, "theme" | "palette">;

export type Provider = "openai" | "local" | "deepgram" | "groq";

export const DEFAULTS: Record<EditableKey, string> = {
  language: "no",
  default_summary_preset: "meeting",
  diarize_model: "community1",
  community1_threshold: "0.5",
  sortformer_silence_threshold: "0.5",
  sortformer_pred_threshold: "0.25",
  keep_audio: "false",
  custom_vocabulary: "",
  summary_model: "gpt-5.4-mini",
  summary_prompt: SUMMARY_PRESETS[0].prompt_no,
  summary_provider: "openai",
  local_llm_base_url: "http://localhost:11434/v1",
  local_llm_model: "",
  local_llm_think: "false",
  developer_mode: "false",
  silence_rms_threshold: "0.005",
};

export const PROVIDERS_BASE = [
  { value: "openai", label: "OpenAI" },
  { value: "deepgram", label: "Deepgram" },
  { value: "groq", label: "Groq (Whisper Large v3 Turbo)" },
];
export const LOCAL_PROVIDER = {
  value: "local",
  label: "Local (Whisper turbo, on-device)",
};

export const SUMMARY_PROVIDERS = [
  { value: "openai", label: "Cloud (OpenAI)" },
  { value: "local", label: "Local (any OpenAI-compatible server)" },
];

export const WHISPER_PRESETS = [
  { value: "fast", label: "Fast — lower latency, may drop borderline words" },
  { value: "balanced", label: "Balanced — good speed and accuracy" },
  { value: "quality", label: "Quality — slowest, best for meetings" },
];

export const THEMES: { value: Theme; label: string }[] = [
  { value: "system", label: "System" },
  { value: "light", label: "Light" },
  { value: "dark", label: "Dark" },
];

export const PALETTES: { value: Palette; label: string; description: string }[] = [
  { value: "warm", label: "Warm", description: "Paper-cream surfaces, white sidebar hover." },
  { value: "nothing", label: "Nothing", description: "Neutral grays, mono-line accents." },
];

export const TRANSCRIBE_MODELS = [
  "whisper-1",
  "gpt-4o-mini-transcribe",
  "gpt-4o-transcribe",
  "gpt-4o-transcribe-diarize",
];

export const DEEPGRAM_MODELS = ["nova-3", "nova-2", "base"];

export const GROQ_MODELS = ["whisper-large-v3-turbo"];

export const SUMMARY_MODELS = [
  "gpt-5.4-mini",
  "gpt-5.4",
  "gpt-5.4-nano",
  "gpt-5.5",
  "gpt-5",
  "gpt-5-mini",
  "gpt-5-nano",
  "o3",
];

export const inputClass =
  "w-full px-3 py-2 rounded-md text-sm bg-[var(--color-input-bg)] border border-[var(--color-line)] focus:border-[var(--color-text-muted)]";

export type KeyState = {
  draft: string;
  hasKey: boolean;
  testing: boolean;
  result: null | { ok: true } | { ok: false; message: string };
};

export const EMPTY_KEY_STATE: KeyState = {
  draft: "",
  hasKey: false,
  testing: false,
  result: null,
};

/// Flash message shape. `info` is a plain text toast; `suggest_language_override`
/// renders a one-click "Add as <language> override?" affordance after a
/// language-specific model is downloaded.
export type LocalFlash =
  | { kind: "info"; message: string }
  | {
      kind: "suggest_language_override";
      message: string;
      language: string;
      modelId: string;
    };

export type LocalState = {
  // List of all known models with their per-id download status. Sourced
  // from the backend registry; the UI filters by language and surfaces
  // download / delete / select per row.
  models: LocalWhisperModelStatus[];
  // Per-model download progress while a download is in flight. Keyed by
  // model id so two simultaneous downloads (rare) wouldn't fight.
  downloading: Record<string, { received: number; total: number | null }>;
  error: string | null;
  flash: LocalFlash | null;
};

export const EMPTY_LOCAL_STATE: LocalState = {
  models: [],
  downloading: {},
  error: null,
  flash: null,
};

export type DiarizeState = {
  status: DiarizeModelStatus | null;
  downloading: boolean;
  fraction: number;
  phase: "listing" | "downloading" | "compiling" | null;
  error: string | null;
  flash: string | null;
};

export const EMPTY_DIARIZE_STATE: DiarizeState = {
  status: null,
  downloading: false,
  fraction: 0,
  phase: null,
  error: null,
  flash: null,
};

// Discriminated so the Summary tab can render guidance tailored to the
// failure mode. "unreachable" means the local server isn't responding —
// usually Ollama not running. "other" is the catch-all (HTTP 500, parse
// failure, etc) where we just show the raw message.
export type LlmModelsError =
  | { kind: "unreachable"; baseUrl: string }
  | { kind: "other"; message: string };

export type LlmModelsState = {
  list: string[] | null;
  loading: boolean;
  error: LlmModelsError | null;
};

export const EMPTY_LLM_MODELS_STATE: LlmModelsState = {
  list: null,
  loading: false,
  error: null,
};

// Returns the value of whichever preset matches the prompt text for the
// current language, or "custom" if the user has typed something else.
export function detectActivePreset(prompt: string, lang: string): string {
  for (const p of SUMMARY_PRESETS) {
    if (presetPromptForLang(p, lang) === prompt) return p.value;
  }
  return "custom";
}
