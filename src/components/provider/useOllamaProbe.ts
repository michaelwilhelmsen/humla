import { useCallback, useEffect, useState } from "react";
import { ipc } from "../../lib/ipc";

// Shared local-LLM reachability: probes the server's model listing and keeps
// polling while enabled, so installing Ollama or pulling a model is detected
// without a retry button. Presentation stays with the consumer — settings
// renders OllamaConnect's rows, the onboarding wizard its staged cards.
//
// `enabled: false` parks the hook (state nulled, no polling) — for consumers
// that only probe while their local option is selected.
export function useOllamaProbe(
  baseUrl: string,
  { pollMs = 2000, enabled = true }: { pollMs?: number; enabled?: boolean } = {},
) {
  // null = no verdict yet (first probe in flight, or disabled).
  const [reachable, setReachable] = useState<boolean | null>(null);
  const [installed, setInstalled] = useState<string[] | null>(null);

  const probe = useCallback(async () => {
    try {
      const list = await ipc.localLlmListModels(baseUrl);
      setReachable(true);
      setInstalled(list);
    } catch {
      setReachable(false);
      setInstalled(null);
    }
  }, [baseUrl]);

  useEffect(() => {
    if (!enabled) {
      setReachable(null);
      setInstalled(null);
      return;
    }
    void probe();
    const timer = window.setInterval(() => void probe(), pollMs);
    return () => window.clearInterval(timer);
  }, [enabled, probe, pollMs]);

  return { reachable, installed };
}
