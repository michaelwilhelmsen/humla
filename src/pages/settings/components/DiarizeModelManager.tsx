import type { DiarizeState } from "../types";
import { Btn } from "./Btn";
import { formatBytes } from "./format";

export function DiarizeModelManager({
  state,
  onDownload,
  onDelete,
}: {
  state: DiarizeState;
  onDownload: () => void;
  onDelete: () => void;
}) {
  if (state.downloading) {
    const pct = Math.min(100, state.fraction * 100);
    const phaseLabel =
      state.phase === "compiling"
        ? "Compiling for Apple Neural Engine"
        : state.phase === "listing"
        ? "Listing files"
        : "Downloading models";
    return (
      <div className="flex flex-col gap-2">
        <div className="text-sm">
          {phaseLabel}… {state.phase === "compiling" ? "" : `${pct.toFixed(0)}%`}
        </div>
        <div className="h-1.5 rounded bg-[var(--color-pill-hover)] overflow-hidden">
          <div
            className="h-full bg-[var(--color-text-muted)] transition-[width] duration-150"
            style={{ width: state.phase === "compiling" ? "100%" : `${pct}%` }}
          />
        </div>
      </div>
    );
  }

  if (state.status?.downloaded) {
    return (
      <div className="flex flex-col gap-2">
        <div className="text-sm">
          Downloaded — FluidAudio diarization (CoreML)
          {state.status.sizeBytes ? ` (${formatBytes(state.status.sizeBytes)})` : ""}
        </div>
        {state.status.path && (
          <div className="text-xs text-[var(--color-text-muted)] font-mono break-all">
            {state.status.path}
          </div>
        )}
        <div className="flex gap-2">
          <Btn onClick={onDelete}>Delete model</Btn>
        </div>
        {state.flash && (
          <p
            className="text-xs px-2 py-1 rounded bg-[var(--color-pill-hover)] inline-block break-all"
            role="status"
          >
            {state.flash}
          </p>
        )}
        {state.error && (
          <p className="text-sm text-red-600 dark:text-red-400 break-all">
            {state.error}
          </p>
        )}
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-2">
      <div className="text-sm">
        Not downloaded. The model is ~15 MB. First-time setup also compiles
        for the Apple Neural Engine, which takes 20-30 s.
      </div>
      <div className="flex gap-2">
        <Btn onClick={onDownload}>Download model</Btn>
      </div>
      {state.flash && (
        <p
          className="text-xs px-2 py-1 rounded bg-[var(--color-pill-hover)] inline-block break-all"
          role="status"
        >
          {state.flash}
        </p>
      )}
      {state.error && (
        <p className="text-sm text-red-600 dark:text-red-400 break-all">
          {state.error}
        </p>
      )}
    </div>
  );
}
