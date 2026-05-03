// Shared types and option lists for the Settings page.
//
// Most of the constants here are options users can pick from a Select.
// They live next to the page rather than in a global module because
// they're tightly coupled to how the form is shaped — moving them out
// would create a layer of indirection without buying us reuse.

import type { DiarizeModelStatus, LocalWhisperModelStatus, SettingsKey } from "../../lib/ipc";
import { SUMMARY_PRESETS, presetPromptForLang } from "../../lib/presets";
import type { Theme } from "../../lib/theme";

export type EditableKey = Exclude<SettingsKey, "theme">;

export type Provider = "openai" | "local";

export const DEFAULTS: Record<EditableKey, string> = {
  language: "no",
  transcribe_provider: "openai",
  transcribe_model: "whisper-1",
  whisper_preset: "quality",
  local_whisper_model: "large-v3-turbo-q5",
  local_whisper_use_gpu: "true",
  final_pass: "true",
  default_summary_preset: "meeting",
  custom_vocabulary: "",
  summary_model: "gpt-5.4-mini",
  summary_prompt: SUMMARY_PRESETS[0].prompt_no,
  summary_provider: "openai",
  local_llm_base_url: "http://localhost:11434/v1",
  local_llm_model: "",
  local_llm_think: "false",
};

export const PROVIDERS_BASE = [{ value: "openai", label: "OpenAI" }];
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

export const TRANSCRIBE_MODELS = [
  "whisper-1",
  "gpt-4o-mini-transcribe",
  "gpt-4o-transcribe",
  "gpt-4o-transcribe-diarize",
];

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

export type LocalState = {
  // List of all known models with their per-id download status. Sourced
  // from the backend registry; the UI filters by language and surfaces
  // download / delete / select per row.
  models: LocalWhisperModelStatus[];
  // Per-model download progress while a download is in flight. Keyed by
  // model id so two simultaneous downloads (rare) wouldn't fight.
  downloading: Record<string, { received: number; total: number | null }>;
  error: string | null;
  flash: string | null;
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

export type LlmModelsState = {
  list: string[] | null;
  loading: boolean;
  error: string | null;
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
