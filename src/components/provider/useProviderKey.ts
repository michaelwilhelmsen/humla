import { useEffect, useState } from "react";
import { ipc } from "../../lib/ipc";
import type { CloudTranscribeProvider } from "../../lib/transcribeDefault";

// "A provider with a Keychain slot" and "a provider whose stored key proves it
// was chosen" are the same set, so this aliases the canonical definition
// instead of re-spelling it. The name stays because ProviderKeyCard and
// Settings → Transcription already import it.
export type KeyProvider = CloudTranscribeProvider;

// Shared per-provider API-key mechanics: stored-sentinel load, save, test.
// Presentation and commit semantics stay with the consumer — settings'
// ProviderKeyCard renders rows; the onboarding steps wrap `test()` to write
// their provider config when it passes (their "commit point"). The backend
// only ever reports a sentinel, never the key, so drafts are write-only.
export function useProviderKey(provider: KeyProvider) {
  const [hasKey, setHasKey] = useState(false);
  const [draft, setDraft] = useState("");
  const [testing, setTesting] = useState(false);
  const [result, setResult] = useState<
    null | { ok: true } | { ok: false; message: string }
  >(null);

  // Provider change = a different Keychain slot: reset the surface and
  // re-read the sentinel.
  useEffect(() => {
    let cancelled = false;
    setDraft("");
    setResult(null);
    setHasKey(false);
    ipc
      .getProviderKey(provider)
      .then((v) => {
        if (!cancelled) setHasKey(v !== null);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [provider]);

  async function save() {
    const key = draft.trim();
    if (!key) return;
    try {
      await ipc.setProviderKey(provider, key);
      setDraft("");
      setHasKey(true);
      setResult(null);
    } catch (e) {
      setResult({ ok: false, message: String(e) });
    }
  }

  // Resolves with the verdict so consumers can hang commit logic on a pass.
  async function test(): Promise<boolean> {
    setTesting(true);
    setResult(null);
    try {
      const r = await ipc.testProviderKey(provider);
      if (r.ok) {
        setResult({ ok: true });
        return true;
      }
      setResult({
        ok: false,
        message: `${r.status}: ${r.error ?? "unknown error"}`,
      });
      return false;
    } catch (e) {
      setResult({ ok: false, message: String(e) });
      return false;
    } finally {
      setTesting(false);
    }
  }

  return { hasKey, draft, setDraft, testing, result, save, test };
}
