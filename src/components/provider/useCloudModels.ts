import { useEffect, useState } from "react";
import { ipc } from "../../lib/ipc";
import { ANTHROPIC_MODELS, SUMMARY_MODELS } from "../../pages/settings/types";

type CloudProvider = "openai" | "anthropic";

const LIST: Record<CloudProvider, () => Promise<string[]>> = {
  openai: () => ipc.openaiListModels(),
  anthropic: () => ipc.anthropicListModels(),
};

const FALLBACK: Record<CloudProvider, string[]> = {
  openai: SUMMARY_MODELS,
  anthropic: ANTHROPIC_MODELS,
};

/** Model options for a cloud picker. The live listing needs a stored key, so
 *  without one — and whenever the call fails, or the picker isn't on screen —
 *  the shipped list stands in. The currently stored value is always among the
 *  options, so a model the listing doesn't return never leaves it blank. */
export function useCloudModels(
  provider: CloudProvider,
  hasKey: boolean,
  selected: string,
  { enabled = true }: { enabled?: boolean } = {},
) {
  const [live, setLive] = useState<string[] | null>(null);

  useEffect(() => {
    setLive(null);
    if (!enabled || !hasKey) return;
    let cancelled = false;
    LIST[provider]()
      .then((ids) => !cancelled && setLive(ids.length ? ids : null))
      .catch(() => !cancelled && setLive(null));
    return () => {
      cancelled = true;
    };
  }, [provider, hasKey, enabled]);

  const ids = live ?? FALLBACK[provider];
  const withSelected = selected && !ids.includes(selected) ? [selected, ...ids] : ids;
  return withSelected.map((m) => ({ value: m, label: m }));
}
