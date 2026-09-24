import { ProgressTrack } from "../../../components/ui/ProgressTrack";
import { downloadProgress } from "../../../lib/diarizeEngine";
import type { DiarizeState } from "../types";
import { Btn } from "./Btn";
import { formatBytes } from "./format";

export function DiarizeModelManager({
  state,
  cost,
  onDownload,
  onDelete,
}: {
  state: DiarizeState;
  /** What downloading it costs, said before the user starts. */
  cost: string;
  onDownload: () => void;
  onDelete: () => void;
}) {
  if (state.downloading) {
    const { label, value } = downloadProgress(state.phase, state.fraction);
    return (
      <div className="flex flex-col gap-2">
        <div className="text-sm">{label}</div>
        <ProgressTrack value={value} label={label} />
      </div>
    );
  }

  if (state.status?.downloaded) {
    return (
      <div className="flex flex-col gap-2">
        <div className="text-sm">
          Downloaded
          {state.status.sizeBytes ? ` (${formatBytes(state.status.sizeBytes)})` : ""}
        </div>
        {state.status.path && (
          <div className="nd-code text-[var(--color-text-muted)] break-all">
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
      <div className="text-sm">Not downloaded. {cost}</div>
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
