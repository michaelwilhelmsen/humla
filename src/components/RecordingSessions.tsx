import type { NoteSession } from "../lib/ipc";
import { formatSessionCaption } from "../lib/sessions";

// Compact numbered-pill carousel for a note's recording sessions (#16),
// shown under a "Recording sessions" eyebrow above the transcript. Read-only
// in v1 — clicking a pill switches the player to that take (the parent
// handles scroll + seek-and-keep-playing). Matches the speaker-chip family
// (nd-label eyebrow + monospace pills). Hidden for single-session notes so
// the common case looks exactly as it did before this feature.
export function RecordingSessions({
  sessions,
  activeId,
  onSelect,
}: {
  sessions: NoteSession[];
  activeId: string | null;
  onSelect: (sessionId: string) => void;
}) {
  if (sessions.length < 2) return null;
  return (
    <div className="mb-4">
      <div className="nd-label mb-2">Recording sessions</div>
      <div className="flex flex-wrap items-center gap-1.5">
        {sessions.map((s) => {
          const active = s.id === activeId;
          return (
            <button
              key={s.id}
              type="button"
              aria-label={`Recording session ${s.index}`}
              aria-pressed={active}
              title={formatSessionCaption(s)}
              onClick={() => onSelect(s.id)}
              className={
                "inline-flex h-7 min-w-7 items-center justify-center rounded-md px-2 " +
                "font-[family-name:var(--font-mono)] text-[12px] tracking-[0.04em] " +
                "border transition-colors cursor-pointer " +
                (active
                  ? "border-[color:var(--color-interactive)] bg-[color:var(--color-pill-hover)] text-[color:var(--color-interactive)]"
                  : "border-[color:var(--color-line-visible)] text-[color:var(--color-text-muted)] hover:bg-[color:var(--color-pill-hover)] hover:text-[color:var(--color-text)]")
              }
            >
              {s.index}
            </button>
          );
        })}
      </div>
    </div>
  );
}
