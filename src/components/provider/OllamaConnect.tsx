import { useCallback, useEffect, useState } from "react";
import { ipc } from "../../lib/ipc";
import { Select } from "../../pages/settings/components/Select";

// Local-LLM (Ollama / LM Studio / llama-server) connection surface: probes
// the server's model listing and keeps polling while mounted, so installing
// Ollama or pulling a model shows up here without a retry button. Base URL
// and model stay controlled props — settings and onboarding each own their
// persistence; this component owns reachability + the model list.
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
  // null = first probe still in flight (shows neither state).
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
    void probe();
    const timer = window.setInterval(() => void probe(), pollMs);
    return () => window.clearInterval(timer);
  }, [probe, pollMs]);

  const modelMissing =
    reachable === true &&
    installed !== null &&
    model !== "" &&
    !installed.includes(model);

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
                ...(installed ?? []).map((m) => ({ value: m, label: m })),
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
