// What counts as a *chosen* transcription default.
//
// `openai` is both a real provider and the fresh-install fallback, so the
// provider id alone cannot separate "the user picked OpenAI" from "nobody has
// picked anything yet". The stored API key is what separates them: no key, no
// choice. Two readers depend on that rule and must never drift —
//
//   - computeSetupStatus, deciding whether the pipeline is working or is in
//     the fresh-install "none" state, and
//   - the onboarding Transcription step, deciding which card to resume onto
//     (#149, where the step used to sidestep the ambiguity by excluding
//     `openai` outright and so could never resume the likeliest cloud pick).
//
// — so the rule lives here once rather than being spelled out at each of them.

import { ipc, type ProviderConfig, type TranscribeProvider } from "./ipc";

/** The providers whose credentials live in the Keychain — all but `local`. */
export type CloudTranscribeProvider = "openai" | "deepgram" | "groq";

export function isCloudProvider(p: TranscribeProvider): p is CloudTranscribeProvider {
  return p === "openai" || p === "deepgram" || p === "groq";
}

/**
 * The cloud provider this default represents an actual choice of, or null —
 * for a local default, for a cloud default whose key isn't stored, and for no
 * config at all.
 *
 * A failed Keychain read resolves to null: an unreadable key is not evidence
 * of a choice. Callers must not drive a write from that null (see #148 — a
 * `.catch(() => null)` is harmless when displayed and destructive when a write
 * hangs off it).
 */
export async function chosenCloudProvider(
  def: ProviderConfig | null | undefined,
): Promise<CloudTranscribeProvider | null> {
  if (!def || !isCloudProvider(def.provider)) return null;
  const provider = def.provider;
  const key = await ipc.getProviderKey(provider).catch(() => null);
  return key ? provider : null;
}
