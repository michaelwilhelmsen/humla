import { useEffect, useMemo, useState } from "react";
import { Merge, User } from "lucide-react";
import { extractSpeakerLabels } from "../lib/speakers";
import { Menu, MenuContent, MenuItem, MenuLabel, MenuTrigger } from "./ui/Menu";
import { Combobox, type ComboboxOption } from "./ui/Combobox";
import {
  caseVariantTarget,
  suggestSpeakerLabels,
  type SpeakerSuggestionSource,
} from "../lib/speakerSuggest";
import { isPerRecordingLabel } from "../lib/crossNoteRename";

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
  onRenameEverywhere,
  otherNotesWithLabel,
  readOnly,
  suggestions,
}: {
  transcript: string;
  // Both rename and merge report through here. For a merge, `newLabel` is
  // an already-existing label from the strip.
  onRename: (oldLabel: string, newLabel: string) => void;
  /**
   * Rename this speaker in every note that names them (#116 part 2), this one
   * included. Offered only for labels `otherNotesWithLabel` reports elsewhere.
   */
  onRenameEverywhere?: (oldLabel: string, newLabel: string) => void;
  /** Per label: how many OTHER notes carry it. Absent → no sweep is offered. */
  otherNotesWithLabel?: Record<string, number>;
  readOnly?: boolean;
  /** Omitted → the rename input is plain free text, exactly as before. */
  suggestions?: SpeakerSuggestionSource;
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
          onRenameEverywhere={
            onRenameEverywhere ? (next) => onRenameEverywhere(label, next) : undefined
          }
          otherNoteCount={otherNotesWithLabel?.[label] ?? 0}
          onMerge={(target) => onRename(label, target)}
          readOnly={readOnly}
          suggestions={suggestions}
          inNoteLabels={labels}
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
  onRenameEverywhere,
  otherNoteCount,
  onMerge,
  readOnly,
  suggestions,
  inNoteLabels,
}: {
  label: string;
  color: string;
  otherLabels: string[];
  colors: Map<string, string>;
  onRename: (next: string) => void;
  onRenameEverywhere?: (next: string) => void;
  /** How many OTHER notes carry this label. 0 → nothing to choose between. */
  otherNoteCount: number;
  onMerge: (target: string) => void;
  readOnly?: boolean;
  suggestions?: SpeakerSuggestionSource;
  inNoteLabels: string[];
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(label);
  // The name is chosen but the scope isn't yet (#116 part 2). Holding it here
  // rather than in a modal is what makes the choice itself the commit.
  const [pendingName, setPendingName] = useState<string | null>(null);

  // Snap the draft back to the canonical label whenever the underlying
  // label changes (e.g. diarize replaced the transcript and our label
  // was re-derived).
  useEffect(() => {
    setDraft(label);
    setPendingName(null);
  }, [label]);

  const ranked = useMemo(
    () =>
      suggestions
        ? suggestSpeakerLabels({
            query: draft,
            stats: suggestions.stats,
            roster: suggestions.roster,
            inNoteLabels,
            renaming: label,
          })
        : [],
    [suggestions, draft, inNoteLabels, label],
  );

  const options: ComboboxOption[] = ranked.map((s) => ({
    value: s.label,
    label: s.label,
    // The distinction is carried by form, not metadata — no note counts or
    // dates. Every row reads "@Name", so anything left in the input WITHOUT an
    // "@" is visibly a new label. That is the whole cue.
    content: (
      <>
        {s.kind === "member" && (
          // A member you've never labelled, marked so it doesn't read as a name
          // you already chose.
          <User size={12} strokeWidth={1.75} aria-hidden className="opacity-60" />
        )}
        <span>@{s.label}</span>
        {s.inNote && (
          // Picking this has a different consequence: it folds two pills into
          // one. Shown, not hidden — typing the name in full merges anyway.
          <span className="ml-auto text-[11px] opacity-60">Merge</span>
        )}
      </>
    ),
  }));

  // Enter normally commits exactly what was typed. The one exception is a pure
  // case/diacritic variant of a name that already exists — "åse" beside "Åse" is
  // never a deliberate second person.
  const preselect = caseVariantTarget(draft, ranked) ?? undefined;

  // A sweep is only ever offered for a real name. `Speaker 1` and `You` mean a
  // DIFFERENT person in every recording, so renaming them across notes writes
  // false attribution — in a workspace, into a teammate's meeting.
  const canSweep =
    !!onRenameEverywhere && otherNoteCount > 0 && !isPerRecordingLabel(label);

  function commit(next: string) {
    setEditing(false);
    const trimmed = next.trim();
    if (!trimmed || trimmed === label) {
      setDraft(label);
      return;
    }
    // With other notes carrying this label, the scope is a real question and the
    // answer is the commit. With none, asking would be a pointless extra click.
    if (canSweep) {
      setPendingName(trimmed);
      return;
    }
    onRename(trimmed);
  }

  function cancelScope() {
    setPendingName(null);
    setDraft(label);
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

  if (pendingName !== null) {
    // The name is chosen; the scope is the remaining question. Two choices, no
    // modal and no destructive default — picking one IS the commit, the same
    // reasoning the merge menu uses (#23). A modal with per-note checkboxes was
    // rejected as turning a data cleanup into a per-note editing session.
    //
    // A `Menu` rather than a `Popover`: this is a list of choices, so Radix's
    // arrow-key roving and `menuitem` semantics come free (CLAUDE.md). The pill
    // itself is the trigger, so the menu anchors where the rename happened.
    return (
      <Menu open onOpenChange={(next) => !next && cancelScope()}>
        <MenuTrigger asChild>
          <button type="button" className="nd-speaker-pill" style={{ background: color }}>
            {pendingName}
          </button>
        </MenuTrigger>
        <MenuContent
          aria-label={`Rename ${label} to ${pendingName}`}
          className="bg-[var(--color-surface)]"
        >
          <MenuLabel>
            {label} → {pendingName}
          </MenuLabel>
          <MenuItem
            onSelect={() => {
              setPendingName(null);
              onRename(pendingName);
            }}
          >
            Rename here only
          </MenuItem>
          <MenuItem
            onSelect={() => {
              setPendingName(null);
              onRenameEverywhere?.(pendingName);
            }}
          >
            {/* This note plus the others, because that is what the sweep
                actually rewrites. */}
            Rename in all {otherNoteCount + 1} notes
          </MenuItem>
        </MenuContent>
      </Menu>
    );
  }

  if (editing) {
    // The input replaces the pill in place, with the suggestion list anchored
    // below it (#116) — no new trigger, and free text keeps working by default
    // rather than by exception.
    //
    // size= sets the visible character width, so the input width tracks the
    // typed text. Floor at 3 so the pill never collapses to nothing mid-edit.
    return (
      <Combobox
        value={draft}
        onValueChange={setDraft}
        options={options}
        preselect={preselect}
        onCommit={commit}
        onCancel={() => {
          setDraft(label);
          setEditing(false);
        }}
        aria-label={`Rename ${label}`}
        listLabel="Speaker names you have used"
        size={Math.max(draft.length, 3)}
        className="nd-speaker-pill cursor-text outline-none"
        style={{ background: color }}
      />
    );
  }

  return (
    <span className="inline-flex items-center gap-1">
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
        // The inline menu itself is the confirmation — the Tauri webview blocks
        // window.confirm, so picking a target IS the commit and there's no
        // destructive default (#23). Focus-in, arrow roving, Escape-returns-
        // focus and dismissal all come from the shared `Menu` now (#114).
        <Menu>
          <MenuTrigger
            aria-label={`Merge ${label} into another speaker`}
            title="Merge this speaker into another"
            className="inline-flex items-center justify-center rounded-full p-1 text-[color:var(--color-text-muted)] hover:text-[color:var(--color-text)] hover:bg-[color:var(--color-pill-hover)] transition-colors"
          >
            <Merge size={13} strokeWidth={1.75} />
          </MenuTrigger>
          <MenuContent aria-label={`Merge ${label} into`} className="bg-[var(--color-surface)]">
            <MenuLabel>Merge into</MenuLabel>
            {otherLabels.map((target) => (
              <MenuItem
                key={target}
                aria-label={`Merge ${label} into ${target}`}
                onSelect={() => onMerge(target)}
                className="px-2 text-[11px] tracking-[0.04em] text-[color:var(--color-text)]"
              >
                <span
                  className="inline-block h-2 w-2 shrink-0 rounded-full"
                  style={{ background: colors.get(target) ?? SPEAKER_COLORS[0] }}
                  aria-hidden
                />
                {target}
              </MenuItem>
            ))}
          </MenuContent>
        </Menu>
      )}
    </span>
  );
}
