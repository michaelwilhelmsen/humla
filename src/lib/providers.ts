// The one list of providers the frontend knows, mirroring `REGISTRY` in
// src-tauri/src/providers.rs — providers.test.ts reads that file and fails
// when the two drift. Which key cards to show, which ids may be sent over
// IPC and which providers can summarise all derive from here.

export const PROVIDERS = [
  { id: "openai", keychain: true, transcribe: true, summarize: true, chat: true },
  { id: "deepgram", keychain: true, transcribe: true, summarize: false, chat: false },
  { id: "groq", keychain: true, transcribe: true, summarize: false, chat: false },
  { id: "local", keychain: false, transcribe: true, summarize: true, chat: true },
] as const;

type Spec = (typeof PROVIDERS)[number];

export type Provider = Spec["id"];
/** Providers whose credentials live in the Keychain. */
export type KeyProvider = Extract<Spec, { keychain: true }>["id"];
export type SummaryProvider = Extract<Spec, { summarize: true }>["id"];

export const KEY_PROVIDERS: readonly KeyProvider[] = PROVIDERS.filter(
  (p): p is Extract<Spec, { keychain: true }> => p.keychain,
).map((p) => p.id);

export function isKeyProvider(p: Provider): p is KeyProvider {
  return (KEY_PROVIDERS as readonly Provider[]).includes(p);
}
