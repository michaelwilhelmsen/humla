// The one list of providers the frontend knows, mirroring `REGISTRY` in
// src-tauri/src/providers.rs — providers.test.ts reads that file and fails
// when the two drift. Which key cards to show, which ids may be sent over
// IPC and which providers can summarise all derive from here.

export const PROVIDERS = [
  { id: "openai", label: "OpenAI", keychain: true, summarize: true, chat: true, transcribe: true },
  { id: "anthropic", label: "Anthropic", keychain: true, summarize: true, chat: true, transcribe: false },
  { id: "deepgram", label: "Deepgram", keychain: true, summarize: false, chat: false, transcribe: true },
  { id: "groq", label: "Groq", keychain: true, summarize: false, chat: false, transcribe: true },
  { id: "local", label: "Local", keychain: false, summarize: true, chat: true, transcribe: true },
] as const;

type Spec = (typeof PROVIDERS)[number];

export type Provider = Spec["id"];
/** Providers whose credentials live in the Keychain. */
export type KeyProvider = Extract<Spec, { keychain: true }>["id"];
export type SummaryProvider = Extract<Spec, { summarize: true }>["id"];
export type ChatCapableProvider = Extract<Spec, { chat: true }>["id"];

export const SUMMARY_CAPABLE: readonly SummaryProvider[] = PROVIDERS.filter(
  (p): p is Extract<Spec, { summarize: true }> => p.summarize,
).map((p) => p.id);

export type TranscribeProvider = Extract<Spec, { transcribe: true }>["id"];
/** Keychain providers that can transcribe — the cloud STT picker's population. */
export type TranscribeKeyProvider = Extract<Spec, { keychain: true; transcribe: true }>["id"];

export const TRANSCRIBE_KEY_PROVIDERS: readonly TranscribeKeyProvider[] = PROVIDERS.filter(
  (p): p is Extract<Spec, { keychain: true; transcribe: true }> => p.keychain && p.transcribe,
).map((p) => p.id);

export const CHAT_CAPABLE: readonly ChatCapableProvider[] = PROVIDERS.filter(
  (p): p is Extract<Spec, { chat: true }> => p.chat,
).map((p) => p.id);

export const KEY_PROVIDERS: readonly KeyProvider[] = PROVIDERS.filter(
  (p): p is Extract<Spec, { keychain: true }> => p.keychain,
).map((p) => p.id);

export function providerLabel(p: Provider): string {
  return PROVIDERS.find((s) => s.id === p)!.label;
}

export function isTranscribeKeyProvider(p: Provider): p is TranscribeKeyProvider {
  return (TRANSCRIBE_KEY_PROVIDERS as readonly Provider[]).includes(p);
}

export function isKeyProvider(p: Provider): p is KeyProvider {
  return (KEY_PROVIDERS as readonly Provider[]).includes(p);
}
