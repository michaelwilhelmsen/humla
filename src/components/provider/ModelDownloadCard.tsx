import { useEffect, useRef } from "react";
import { ipc, type LocalWhisperModelStatus } from "../../lib/ipc";
import { useDownloadStore } from "../../lib/store";
import { formatBytes } from "../../pages/settings/components/format";
import { Btn } from "../../pages/settings/components/Btn";

// Per-model download/delete row for local Whisper models. Progress,
// completion, and failure all come from useDownloadStore — the global slice
// fed by the app's single local_whisper_progress listener — NEVER from the
// download invoke's promise, which dies with whatever mount started it. A
// download started here keeps reporting after the settings dialog closes
// and reopens, and one started in onboarding shows up here too.
//
// Optional surfaces let consumers compose without forking the row:
// - selected/onSelect + tag: the settings model list's default-model radio
// - onDownload/onDelete: orchestrating handlers (settings' handler promotes
//   the first multilingual download to default and fires the
//   suggest-override flash) — omitted, the card talks to IPC directly
// - warning: consumer-supplied caution line (e.g. selected-but-not-on-disk)
export function ModelDownloadCard({
  model,
  onChanged,
  tag,
  selected,
  onSelect,
  warning,
  onDownload,
  onDelete,
}: {
  model: LocalWhisperModelStatus;
  onChanged?: () => void;
  tag?: string;
  selected?: boolean;
  onSelect?: () => void;
  warning?: string;
  onDownload?: (modelId: string) => void;
  onDelete?: (modelId: string) => void;
}) {
  const active = useDownloadStore((s) => s.active);
  const mine = active?.modelId === model.id ? active : null;

  // Terminal transition: this model WAS downloading and the slice cleared
  // (completion or failure — the store handles both). Tell the parent to
  // refetch presence; we can't know which outcome without refetching anyway.
  const wasMine = useRef(false);
  useEffect(() => {
    if (mine) {
      wasMine.current = true;
    } else if (wasMine.current) {
      wasMine.current = false;
      onChanged?.();
    }
  }, [mine, onChanged]);

  async function download() {
    if (onDownload) {
      onDownload(model.id);
      return;
    }
    // Fire-and-forget by design; the store carries the progress story. The
    // catch keeps an immediate spawn failure from becoming an unhandled
    // rejection — the backend also emits a download-error event for it.
    try {
      await ipc.localWhisperDownload(model.id);
    } catch {
      onChanged?.();
    }
  }

  async function deleteModel() {
    if (onDelete) {
      onDelete(model.id);
      return;
    }
    await ipc.localWhisperDelete(model.id);
    onChanged?.();
  }

  const pct =
    mine && mine.total ? Math.round((mine.received / mine.total) * 100) : null;

  return (
    <div className="py-3.5 flex items-start gap-3">
      {onSelect && (
        <input
          type="radio"
          checked={!!selected}
          disabled={!model.downloaded}
          onChange={onSelect}
          className="mt-1 shrink-0"
          aria-label={`Use ${model.label}`}
        />
      )}
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2 text-sm">
          <span>{model.label}</span>
          {tag && (
            <span className="text-xs px-1.5 py-0.5 rounded bg-[var(--color-pill-hover)] text-[var(--color-text-muted)]">
              {tag}
            </span>
          )}
          {selected && model.downloaded && (
            <span className="text-xs text-[var(--color-text-muted)]">
              · active
            </span>
          )}
        </div>
        <p className="text-xs text-[var(--color-text-muted)] mt-0.5">
          {model.description}
        </p>
        <p className="text-xs text-[var(--color-text-muted)] mt-1">
          {mine
            ? `Downloading — ${pct !== null ? `${pct}%` : formatBytes(mine.received)}`
            : model.downloaded
              ? `Downloaded · ${formatBytes(model.sizeBytes ?? model.sizeBytesHint)}`
              : `Not downloaded · ~${formatBytes(model.sizeBytesHint)}`}
        </p>
        {warning && !mine && (
          <p className="text-xs text-[var(--color-warning)] mt-1">{warning}</p>
        )}
        {mine && (
          <div
            role="progressbar"
            aria-valuenow={pct ?? undefined}
            aria-valuemin={0}
            aria-valuemax={100}
            className="mt-2 h-1 w-48 rounded-full bg-[var(--color-pill-hover)] overflow-hidden"
          >
            <div
              className="h-full bg-[var(--color-accent)] transition-[width]"
              style={{ width: `${pct ?? 0}%` }}
            />
          </div>
        )}
      </div>
      <div className="shrink-0">
        {model.downloaded ? (
          <Btn onClick={deleteModel}>Delete</Btn>
        ) : (
          // One download at a time app-wide (the backend serialises them
          // anyway); any active download disables every other card's button.
          <Btn onClick={download} disabled={active !== null}>
            {mine ? "Downloading…" : "Download"}
          </Btn>
        )}
      </div>
    </div>
  );
}
