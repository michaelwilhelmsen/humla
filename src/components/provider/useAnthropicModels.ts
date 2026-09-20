import { useEffect, useState } from "react";
import { ipc } from "../../lib/ipc";
import { ANTHROPIC_MODELS } from "../../pages/settings/types";

/** Model options for an Anthropic picker (#183). The live listing needs a
 *  stored key, so without one — and whenever the call fails, or the picker
 *  isn't on screen — the shipped list stands in. The currently stored value is always among the options, so a
 *  model the listing doesn't return never leaves the picker blank. */
export function useAnthropicModels(
  hasKey: boolean,
  selected: string,
  { enabled = true }: { enabled?: boolean } = {},
) {
  const [live, setLive] = useState<string[] | null>(null);

  useEffect(() => {
    if (!enabled || !hasKey) {
      setLive(null);
      return;
    }
    let cancelled = false;
    ipc
      .anthropicListModels()
      .then((ids) => !cancelled && setLive(ids.length ? ids : null))
      .catch(() => !cancelled && setLive(null));
    return () => {
      cancelled = true;
    };
  }, [hasKey, enabled]);

  const ids = live ?? ANTHROPIC_MODELS;
  const withSelected = selected && !ids.includes(selected) ? [selected, ...ids] : ids;
  return withSelected.map((m) => ({ value: m, label: m }));
}
