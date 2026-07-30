// Suggestion source for the speaker-rename picker (#116 part 1).
//
// This is the cross-note identity strategy, not a convenience feature.
// ADR-0002 declines a Person entity, so there is no alias table and no
// read-time reconciliation: the rename IS the join key. That only works if
// people converge on one spelling, which is what this file exists to make
// happen at write time. Without it a library where the same person is spelled
// three ways answers every speaker-scoped query with a third of the truth — a
// wrong answer indistinguishable from a complete one.
//
// Pure functions over data the caller has already fetched: suggestions are
// filtered and ranked per keystroke in TS, never per-keystroke IPC.

/** One distinct label in the active workspace, with its usage counters. */
export type SpeakerLabelStat = {
  label: string;
  /** How many live notes carry this label. */
  note_count: number;
  /** `MAX(notes.updated_at)` over those notes, as the epoch integer the column stores. */
  last_used_at: number;
};

export type SpeakerSuggestion = {
  label: string;
  /** `used` = a label somewhere in the library; `member` = a workspace member never labelled. */
  kind: "used" | "member";
  /** Already a label on this note, so picking it merges rather than renames. */
  inNote: boolean;
};

/**
 * The two halves of the picker's suggestion source, as one value the strip is
 * handed. Neither half alone is right: most speakers are NOT workspace members
 * (a client's people are not your colleagues), but a member who joined a call
 * should be suggested before you've ever typed their name.
 */
export type SpeakerSuggestionSource = {
  /** Distinct labels in the active workspace, from `ipc.speakerLabelStats()`. */
  stats: SpeakerLabelStat[];
  /** Workspace member display names, from `ipc.speakerRoster()`. */
  roster: string[];
};

/** Rows shown at once. Small enough that the popover never covers the transcript. */
export const MAX_SUGGESTIONS = 8;

/**
 * The one comparison key for two labels. Case- AND diacritic-insensitive, and
 * it has to be done here: SQLite `LIKE` folds ASCII only, so `Åse` and `åse`
 * are different keys on both retrieval paths. In a Norwegian-market product the
 * case-variant split is the specific failure this picker prevents.
 */
export function foldLabel(label: string): string {
  return label
    .trim()
    .normalize("NFD")
    .replace(/\p{Diacritic}/gu, "")
    .toLowerCase();
}

/**
 * `Speaker 3` and `You` are what diarization emits before a human names anyone.
 * They are never suggested — converging on a placeholder is the opposite of the
 * point.
 */
export function isPlaceholderLabel(label: string): boolean {
  const folded = foldLabel(label);
  return folded === "you" || /^speaker \d+$/.test(folded);
}

/**
 * Where a query matched, lower is better: -1 = the whole label folds to the
 * query, 0 = start of the label, 1 = start of a later word, 2 = no match.
 *
 * A fold-equal label outranks everything so it can never be pushed past the
 * row cap by higher-count siblings — otherwise the one preselect exception
 * (`caseVariantTarget`) would silently stop firing for exactly the names most
 * likely to have them, e.g. `åse` behind `Åse Berg` and `Åsen`.
 */
const NO_MATCH = 2;
function matchRank(label: string, foldedQuery: string): number {
  if (!foldedQuery) return 0;
  const folded = foldLabel(label);
  if (folded === foldedQuery) return -1;
  if (folded.startsWith(foldedQuery)) return 0;
  // Word-start only: `tron` reaches `Hege Tronshaugen` (a surname is how you
  // would actually reach a full name) but `ege` does not reach `Hege`.
  for (let i = 1; i < folded.length; i++) {
    if (/[\s\-']/.test(folded[i - 1]) && folded.startsWith(foldedQuery, i)) return 1;
  }
  return NO_MATCH;
}

/**
 * The ranked suggestions for what the user has typed so far.
 *
 * Order: match position (an exact fold-equal label first, then a whole-label
 * match, then a later-word one), then note count, then recency. Recency
 * deliberately does NOT lead — on an empty query the list would reorder every
 * session and the muscle memory the picker exists to build would never form.
 */
export function suggestSpeakerLabels({
  query,
  stats,
  roster = [],
  inNoteLabels = [],
  renaming,
}: {
  query: string;
  /** Distinct labels in the workspace, from `speaker_label_stats`. */
  stats: SpeakerLabelStat[];
  /** Workspace member display names — suggestable before you've ever typed them. */
  roster?: string[];
  /** Labels on the note being edited; picking one is a merge. */
  inNoteLabels?: string[];
  /** The label being renamed. Offering it back is a no-op. */
  renaming?: string;
}): SpeakerSuggestion[] {
  const foldedQuery = foldLabel(query);
  const inNote = new Set(inNoteLabels.map(foldLabel));
  const skip = new Set(renaming ? [foldLabel(renaming)] : []);

  type Candidate = SpeakerSuggestion & { rank: number; count: number; recency: number };
  const byKey = new Map<string, Candidate>();

  const add = (label: string, kind: "used" | "member", count: number, recency: number) => {
    const key = foldLabel(label);
    if (!key || skip.has(key) || isPlaceholderLabel(label)) return;
    const rank = matchRank(label, foldedQuery);
    if (rank === NO_MATCH) return;
    const existing = byKey.get(key);
    // A member you HAVE labelled is a used label, not a stranger — whichever
    // source saw it first, `used` wins and the counters survive.
    if (existing) {
      if (kind === "used") {
        existing.kind = "used";
        existing.label = label;
        existing.count = Math.max(existing.count, count);
        existing.recency = Math.max(existing.recency, recency);
      }
      return;
    }
    byKey.set(key, { label, kind, inNote: inNote.has(key), rank, count, recency });
  };

  for (const s of stats) add(s.label, "used", s.note_count, s.last_used_at);
  for (const name of roster) add(name, "member", 0, 0);

  return [...byKey.values()]
    .sort(
      (a, b) =>
        a.rank - b.rank ||
        // Ties break on note count, then recency. This also puts every label
        // you've used ahead of every member you haven't, for free: a used label
        // has at least one note and a bare roster entry has none.
        b.count - a.count ||
        b.recency - a.recency ||
        a.label.localeCompare(b.label),
    )
    .slice(0, MAX_SUGGESTIONS)
    .map(({ label, kind, inNote }) => ({ label, kind, inNote }));
}

/**
 * The one label that should be preselected, or `null` for the normal case where
 * Enter commits exactly what was typed.
 *
 * The exception is a pure case/diacritic variant of an existing label: `åse`
 * next to `Åse` is never a deliberate second person, and letting it through is
 * precisely the split this picker exists to prevent. A mere prefix is NOT
 * preselected — `Hege` + Enter silently writing `Hege Tronshaugen` would
 * override a name the user may have meant literally, and a new person must
 * never be harder to enter than an existing one.
 */
export function caseVariantTarget(query: string, suggestions: SpeakerSuggestion[]): string | null {
  const typed = query.trim();
  if (!typed) return null;
  const folded = foldLabel(typed);
  const hit = suggestions.find((s) => foldLabel(s.label) === folded && s.label.trim() !== typed);
  return hit ? hit.label : null;
}
