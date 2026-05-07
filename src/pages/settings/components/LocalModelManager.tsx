import type { LocalWhisperModelStatus } from "../../../lib/ipc";
import type { LocalState } from "../types";
import { Btn } from "./Btn";
import { formatBytes } from "./format";

export function LocalModelManager({
  state,
  activeId,
  language,
  onDownload,
  onDelete,
  onSelect,
}: {
  state: LocalState;
  activeId: string;
  language: string;
  onDownload: (id: string) => void;
  onDelete: (id: string) => void;
  onSelect: (id: string) => void;
}) {
  const primaries = state.models.filter((m) => m.kind === "multilingual");
  // Show an addon row when its language matches the user's global default.
  // It's also always shown when already downloaded so the user can delete
  // it after switching languages.
  const addons = state.models.filter(
    (m) => m.kind === "language_specific" && (m.specificLanguage === language || m.downloaded),
  );

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-col gap-3">
        <p className="text-xs text-[var(--color-text-muted)]">
          Pick a model to use for transcription. The first download is
          auto-selected; afterwards switch with the radio button. All
          models run on-device via Metal.
        </p>
        {primaries.map((m) => (
          <ModelRow
            key={m.id}
            model={m}
            progress={state.downloading[m.id]}
            isActive={m.id === activeId}
            showRadio
            onDownload={onDownload}
            onDelete={onDelete}
            onSelect={onSelect}
          />
        ))}
      </div>
      {addons.length > 0 && (
        <div className="flex flex-col gap-3">
          <div className="flex flex-col gap-1">
            <p className="text-xs uppercase tracking-wide text-[var(--color-text-muted)]">
              Add-ons
            </p>
            <p className="text-xs text-[var(--color-text-muted)]">
              Specialised models for specific languages. When downloaded,
              the addon is used automatically for matching recordings —
              your active primary still handles every other language.
            </p>
          </div>
          {addons.map((m) => (
            <ModelRow
              key={m.id}
              model={m}
              progress={state.downloading[m.id]}
              isActive={false}
              showRadio={false}
              onDownload={onDownload}
              onDelete={onDelete}
              onSelect={onSelect}
            />
          ))}
        </div>
      )}
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

function ModelRow({
  model,
  progress,
  isActive,
  showRadio,
  onDownload,
  onDelete,
  onSelect,
}: {
  model: LocalWhisperModelStatus;
  progress: { received: number; total: number | null } | undefined;
  isActive: boolean;
  showRadio: boolean;
  onDownload: (id: string) => void;
  onDelete: (id: string) => void;
  onSelect: (id: string) => void;
}) {
  return (
    <div className="flex flex-col gap-2 px-3 py-2 rounded-md border border-[var(--color-line)]">
      <div className="flex items-start gap-2">
        {showRadio ? (
          <input
            type="radio"
            name="local_whisper_model"
            checked={isActive}
            disabled={!model.downloaded}
            onChange={() => onSelect(model.id)}
            className="mt-1"
            aria-label={`Use ${model.label}`}
          />
        ) : (
          <span className="mt-1 w-3.5" aria-hidden />
        )}
        <div className="flex-1 flex flex-col gap-0.5">
          <div className="flex items-center gap-2 text-sm">
            <span className="font-medium">{model.label}</span>
            {model.kind === "language_specific" && model.specificLanguage && (
              <span className="text-xs px-1.5 py-0.5 rounded bg-[var(--color-pill-hover)] text-[var(--color-text-muted)]">
                {model.specificLanguage} auto
              </span>
            )}
            {isActive && model.downloaded && (
              <span className="text-xs text-[var(--color-text-muted)]">
                · active
              </span>
            )}
          </div>
          <p className="text-xs text-[var(--color-text-muted)]">
            {model.description}
          </p>
          <p className="text-xs text-[var(--color-text-muted)]">
            {model.downloaded
              ? `Downloaded${
                  model.sizeBytes ? ` · ${formatBytes(model.sizeBytes)}` : ""
                }`
              : `Not downloaded · ~${formatBytes(model.sizeBytesHint)}`}
          </p>
        </div>
        <div className="flex gap-2">
          {!model.downloaded && !progress && (
            <Btn onClick={() => onDownload(model.id)}>Download</Btn>
          )}
          {model.downloaded && !progress && (
            <Btn onClick={() => onDelete(model.id)}>Delete</Btn>
          )}
        </div>
      </div>
      {progress && (
        <div className="flex flex-col gap-1 mt-1">
          <div className="text-xs text-[var(--color-text-muted)]">
            Downloading
            {progress.total
              ? ` ${formatBytes(progress.received)} / ${formatBytes(progress.total)}`
              : ` ${formatBytes(progress.received)}`}
            …
          </div>
          <div className="h-1 rounded bg-[var(--color-pill-hover)] overflow-hidden">
            <div
              className="h-full bg-[var(--color-text-muted)] transition-[width] duration-150"
              style={{
                width:
                  progress.total === null
                    ? "30%"
                    : `${Math.min(100, (progress.received / progress.total) * 100)}%`,
              }}
            />
          </div>
        </div>
      )}
    </div>
  );
}
