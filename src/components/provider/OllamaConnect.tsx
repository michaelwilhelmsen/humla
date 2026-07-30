import { useEffect, useRef } from "react";
import { Select } from "../ui/Select";
import { completionModels } from "../../lib/localModels";
import { useOllamaProbe } from "./useOllamaProbe";

// Local-LLM (Ollama / LM Studio / llama-server) connection surface for the
// settings dialog: reachability + model list via the shared useOllamaProbe
// hook, rendered as kit rows. Base URL and model stay controlled props —
// consumers own their persistence.
export function OllamaConnect({
  baseUrl,
  onBaseUrlChange,
  model,
  onModelChange,
  pollMs = 2000,
}: {
  baseUrl: string;
  onBaseUrlChange: (url: string) => void;
  model: string;
  onModelChange: (model: string) => void;
  pollMs?: number;
}) {
  const { reachable, installed } = useOllamaProbe(baseUrl, { pollMs });

  // This is a completion-model picker (chat + summary), so embedding-only
  // models (e.g. embeddinggemma, pulled for semantic search #48) must never be
  // offered or auto-selected — Ollama 400s "does not support chat" for them.
  const models = completionModels(installed);

  // Empty selection self-heals on contact: with no model set the dropdown
  // used to LOOK fine (HTML default fallback) while summaries failed with
  // "model not configured". A stored-but-missing model is NOT auto-switched
  // — the warning below tells the user instead. Refs so each poll's fresh
  // `installed` array doesn't need the callbacks in deps.
  const modelRef = useRef(model);
  modelRef.current = model;
  const onModelChangeRef = useRef(onModelChange);
  onModelChangeRef.current = onModelChange;
  useEffect(() => {
    const usable = completionModels(installed);
    if (usable.length > 0 && !modelRef.current) {
      onModelChangeRef.current(usable[0]);
    }
  }, [installed]);

  const modelMissing =
    reachable === true &&
    installed !== null &&
    model !== "" &&
    !models.includes(model);

  return (
    <div className="py-3.5 flex flex-col gap-3">
      <div className="flex items-center justify-between gap-6">
        <div className="min-w-0">
          <div className="text-sm">Server</div>
          <p className="text-xs text-[var(--color-text-muted)] mt-0.5">
            Ollama's default is http://localhost:11434 — change it for LM
            Studio or a remote llama-server.
          </p>
        </div>
        <div className="shrink-0 flex items-center gap-2">
          <input
            type="url"
            value={baseUrl}
            onChange={(e) => onBaseUrlChange(e.target.value)}
            aria-label="Local LLM server URL"
            className="w-56 text-sm px-3 py-1.5 rounded-md border border-[var(--color-line-visible)] bg-[var(--color-surface)] focus:border-[var(--color-text-muted)] transition-colors"
          />
          {reachable === true && (
            <span className="text-xs text-[var(--color-success)] whitespace-nowrap">
              Connected
            </span>
          )}
        </div>
      </div>

      {reachable === false && (
        <p className="text-xs text-[var(--color-text-muted)]">
          Waiting for the server… Install Ollama from ollama.com (or start
          your server) and it's detected automatically.
        </p>
      )}

      {reachable === true && (
        <div className="flex items-center justify-between gap-6">
          <div className="min-w-0">
            <div className="text-sm">Model</div>
            <p className="text-xs text-[var(--color-text-muted)] mt-0.5">
              Installed on the server; pull new ones with{" "}
              <code>ollama pull</code> and they appear here.
            </p>
          </div>
          <div className="shrink-0">
            <Select
              value={model}
              onChange={onModelChange}
              options={[
                ...(model === "" || modelMissing
                  ? [{ value: model, label: model === "" ? "Choose a model…" : model }]
                  : []),
                ...models.map((m) => ({ value: m, label: m })),
              ]}
            />
          </div>
        </div>
      )}

      {modelMissing && (
        <p className="text-xs text-[var(--color-warning)]">
          "{model}" isn't installed on this server anymore — pick another
          model or <code>ollama pull {model}</code>.
        </p>
      )}
    </div>
  );
}
