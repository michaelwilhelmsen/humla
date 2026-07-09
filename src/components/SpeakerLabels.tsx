import { useEffect, useMemo, useRef, useState } from "react";
import { Merge } from "lucide-react";
import { extractSpeakerLabels } from "../lib/speakers";

// Speaker chip strip shown above the transcript. Each unique speaker label
// renders as a colour-coded pill; clicking a pill renames it inline, and a
// dedicated merge affordance folds one label into another. A merge IS a
// rename whose target is an already-existing label (#23) — both actions
// surface through the single `onRename(oldLabel, newLabel)` callback, which
// the parent wires to the transcript rewrite + backend timeline rename.
//
// Extracted from Note.tsx so the strip can be unit-tested in isolation
// (see SpeakerLabels.test.tsx).

export const SPEAKER_COLORS = [
  "var(--color-interactive)",
  "var(--color-success)",
  "var(--color-warning)",
  "var(--color-speaker-4)",
];

export function speakerColorMap(labels: string[]): Map<string, string> {
  const map = new Map<string, string>();
  labels.forEach((label, i) => {
    map.set(label, SPEAKER_COLORS[i % SPEAKER_COLORS.length]);
  });
  return map;
}

export function SpeakerLabels({
  transcript,
  onRename,
  readOnly,
}: {
  transcript: string;
  // Both rename and merge report through here. For a merge, `newLabel` is
  // an already-existing label from the strip.
  onRename: (oldLabel: string, newLabel: string) => void;
  readOnly?: boolean;
}) {
  const labels = useMemo(() => extractSpeakerLabels(transcript), [transcript]);
  const colors = useMemo(() => speakerColorMap(labels), [labels]);
  // Only render the strip when there are 2+ unique speakers — solo
  // monologues don't need management UI. This also means the strip
  // disappears the moment a merge collapses the count back to 1.
  if (labels.length < 2) return null;
  return (
    <div className="flex flex-wrap gap-2 mb-4">
      {labels.map((label) => (
        <SpeakerChip
          key={label}
          label={label}
          color={colors.get(label) ?? SPEAKER_COLORS[0]}
          otherLabels={labels.filter((l) => l !== label)}
          colors={colors}
          onRename={(next) => onRename(label, next)}
          onMerge={(target) => onRename(label, target)}
          readOnly={readOnly}
        />
      ))}
    </div>
  );
}

function SpeakerChip({
  label,
  color,
  otherLabels,
  colors,
  onRename,
  onMerge,
  readOnly,
}: {
  label: string;
  color: string;
  otherLabels: string[];
  colors: Map<string, string>;
  onRename: (next: string) => void;
  onMerge: (target: string) => void;
  readOnly?: boolean;
}) {
  const [editing, setEditing] = useState(false);
  const [merging, setMerging] = useState(false);
  const [draft, setDraft] = useState(label);
  const inputRef = useRef<HTMLInputElement | null>(null);
  const wrapRef = useRef<HTMLSpanElement | null>(null);

  // Snap the draft back to the canonical label whenever the underlying
  // label changes (e.g. diarize replaced the transcript and our label
  // was re-derived).
  useEffect(() => {
    setDraft(label);
  }, [label]);

  useEffect(() => {
    if (editing) inputRef.current?.select();
  }, [editing]);

  // Dismiss the merge menu on outside click or Escape — the Tauri webview
  // blocks window.confirm, so the inline menu itself is the confirmation:
  // picking a target IS the commit, and there's no destructive default.
  useEffect(() => {
    if (!merging) return;
    function onDown(e: MouseEvent) {
      if (wrapRef.current && wrapRef.current.contains(e.target as Node)) return;
      setMerging(false);
    }
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") setMerging(false);
    }
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [merging]);

  function commit() {
    setEditing(false);
    const trimmed = draft.trim();
    if (!trimmed || trimmed === label) {
      setDraft(label);
      return;
    }
    onRename(trimmed);
  }

  // Read-only (viewer): a static, non-interactive pill — no click-to-rename,
  // no merge.
  if (readOnly) {
    return (
      <span className="nd-speaker-pill" style={{ background: color }}>
        {label}
      </span>
    );
  }

  if (editing) {
    // size= sets the visible character width; with monospace font this
    // makes the input width track the typed text. Floor at 3 so the
    // pill never collapses to nothing while the user is mid-edit.
    return (
      <input
        ref={inputRef}
        value={draft}
        onChange={(e) => setDraft(e.target.value)}
        onBlur={commit}
        size={Math.max(draft.length, 3)}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            commit();
          } else if (e.key === "Escape") {
            e.preventDefault();
            setDraft(label);
            setEditing(false);
          }
        }}
        className="nd-speaker-pill cursor-text outline-none"
        style={{ background: color }}
      />
    );
  }

  return (
    <span ref={wrapRef} className="relative inline-flex items-center gap-1">
      <button
        type="button"
        onClick={() => setEditing(true)}
        title="Click to rename — applies to every turn from this speaker"
        className="nd-speaker-pill cursor-pointer hover:opacity-90"
        style={{ background: color }}
      >
        {label}
      </button>
      {otherLabels.length > 0 && (
        <button
          type="button"
          aria-label={`Merge ${label} into another speaker`}
          aria-haspopup="menu"
          aria-expanded={merging}
          title="Merge this speaker into another"
          onClick={() => setMerging((v) => !v)}
          className="inline-flex items-center justify-center rounded-full p-1 text-[color:var(--color-text-muted)] hover:text-[color:var(--color-text)] hover:bg-[color:var(--color-pill-hover)] transition-colors"
        >
          <Merge size={13} strokeWidth={1.75} />
        </button>
      )}
      {merging && otherLabels.length > 0 && (
        <div
          role="menu"
          aria-label={`Merge ${label} into`}
          className="absolute left-0 top-full z-20 mt-1 min-w-[10rem] rounded-lg border border-[color:var(--color-line-visible)] bg-[color:var(--color-surface)] p-1 shadow-lg"
        >
          <div className="nd-label px-2 py-1">Merge into</div>
          {otherLabels.map((target) => (
            <button
              key={target}
              type="button"
              role="menuitem"
              aria-label={`Merge ${label} into ${target}`}
              onClick={() => {
                setMerging(false);
                onMerge(target);
              }}
              className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left font-[family-name:var(--font-mono)] text-[11px] tracking-[0.04em] text-[color:var(--color-text)] hover:bg-[color:var(--color-pill-hover)]"
            >
              <span
                className="inline-block h-2 w-2 shrink-0 rounded-full"
                style={{ background: colors.get(target) ?? SPEAKER_COLORS[0] }}
                aria-hidden
              />
              {target}
            </button>
          ))}
        </div>
      )}
    </span>
  );
}
