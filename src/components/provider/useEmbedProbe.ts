import { useCallback, useEffect, useState } from "react";
import { ipc } from "../../lib/ipc";

// Whether the local embedder actually answers, by embedding one word (#179).
//
// A model listing cannot answer this: a server can list a model and serve no
// `/v1/embeddings` route (mlx_lm.server), and a name that isn't the loaded
// embedder is a 400 the listing looks fine through. One shot per (url, model),
// debounced — the user types into those fields, and a probe can load a model
// into memory, so this must not poll the way the reachability probe does.
export function useEmbedProbe(
  baseUrl: string,
  model: string,
  { enabled = true, debounceMs = 700 }: { enabled?: boolean; debounceMs?: number } = {},
) {
  const [dims, setDims] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [checking, setChecking] = useState(false);

  const probe = useCallback(async () => {
    setChecking(true);
    try {
      const d = await ipc.localLlmEmbedProbe(baseUrl, model);
      setDims(d);
      setError(null);
    } catch (e) {
      setDims(null);
      // A Tauri command rejects with its plain error string; a thrown Error
      // would otherwise reach the user with an "Error: " prefix glued on.
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setChecking(false);
    }
  }, [baseUrl, model]);

  useEffect(() => {
    if (!enabled || !baseUrl || !model) {
      setDims(null);
      setError(null);
      setChecking(false);
      return;
    }
    const timer = window.setTimeout(() => void probe(), debounceMs);
    return () => window.clearTimeout(timer);
  }, [enabled, baseUrl, model, debounceMs, probe]);

  return { dims, error, checking, recheck: probe };
}
