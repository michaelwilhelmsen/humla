import { extractSpeakerLabels, renameSpeakerInTranscript } from "./speakers";
import { ipc, type Note } from "./ipc";
import { isPlaceholderLabel } from "./speakerSuggest";

// Rename a speaker across every note that names them (#116 part 2).
//
// ADR-0002 declines a Person entity, so there is no alias table: rewriting the
// transcripts is the ONLY repair for four spellings of one person. Renaming was
// per-note until now, which meant converging four spellings involved finding and
// opening four notes — the practical objection to derived-only identity. This is
// the primitive that closes it.
//
// Deliberately a loop over the primitives a single-note rename already uses,
// rather than a Rust sweep command. `db.rs` warns that three copies of the
// label-parse rule already exist (frontend, Rust, cloud indexer), pinned by a
// mirror test; a sweep would be a fourth. At the corpus this exists to clean up
// (tens of notes) the IPC cost is irrelevant.
//
// Undo comes free: `db::update_note` snapshots a revision whenever the
// transcript changes, so every touched note keeps a restorable pre-rename copy.

/**
 * The notes whose transcript carries `label` as a speaker turn.
 *
 * Counted from **transcript text**, not from `notes.speakers`: that column is
 * only written by `reindex_note` and can be stale, so counting off it could show
 * a number that differs from what actually gets rewritten. Counting off text
 * means the number on the button cannot be a lie.
 *
 * Trashed notes never appear here because `notes_list` filters `deleted_at`, so
 * the store never holds them — a note restored from the Trash keeps its old
 * label.
 */
export function notesWithSpeaker(notes: Note[], label: string): Note[] {
  return notes.filter((n) => extractSpeakerLabels(n.transcript).includes(label));
}

/**
 * Whether this label means a *different person* in every recording.
 *
 * `You` is whoever held the mic; `Speaker 1` is whoever the diarizer clustered
 * first. Neither is a name, so neither may be renamed across notes: in a
 * workspace that writes false attribution into a teammate's meeting — sweeping
 * `You` → "Michael" turned the speaker in Kurt's recording into Michael, when
 * `You` there was Kurt.
 *
 * A real name is the opposite, and that asymmetry is what makes the sweep sound
 * at all: Hege is the same Hege in everyone's notes, which is why converging her
 * four spellings across the library is a repair rather than a corruption.
 *
 * The one legitimate exception is the `You` action in Settings, which is scoped
 * to [`notesIRecorded`] — inside my own recordings, `You` really is me.
 */
export function isPerRecordingLabel(label: string): boolean {
  return isPlaceholderLabel(label);
}

/**
 * The notes this user recorded, which is the only scope in which a per-recording
 * label like `You` resolves to them.
 *
 * An empty `owner` counts as mine: that's a local-only or pre-sync note, and
 * every Personal note. With no signed-in user, owned notes are excluded rather
 * than assumed — a store left over from a previous session must not widen a
 * sweep.
 */
export function notesIRecorded(notes: Note[], myUserId: string | undefined): Note[] {
  return notes.filter((n) => !n.owner || n.owner === myUserId);
}

export type CrossNoteRenameOutcome = {
  /** Note ids whose transcript was rewritten and saved. */
  renamed: string[];
  /** Note ids whose transcript write failed; nothing changed for these. */
  failed: string[];
};

/**
 * Rewrite `oldLabel` → `newLabel` in every note that carries it.
 *
 * Optimistic: `onRewritten` fires for every affected note before the first write
 * is awaited, so every pill and list row flips at once and the writes run
 * behind. Failures are collected rather than thrown, because stopping at the
 * first one would leave a partial rewrite the user was never told about.
 *
 * **Transcript and timeline only — summaries are not rewritten.** The rename
 * rule is line-anchored (`^Speaker 1: `) precisely because a mid-sentence label
 * is ambiguous, and free substring replacement over model prose would corrupt
 * real words for a speaker called `Bo` or `Ada`. A summary is regenerable from
 * the corrected transcript.
 */
export async function renameSpeakerAcrossNotes({
  notes,
  oldLabel,
  newLabel,
  updateNote = ipc.updateNote,
  noteTimelineRename = ipc.noteTimelineRename,
  uploadNoteSessions = ipc.uploadNoteSessions,
  reindexNote = ipc.chatReindexNote,
  onRewritten,
}: {
  /** Candidates to scan — typically every note the store holds. */
  notes: Note[];
  oldLabel: string;
  newLabel: string;
  // The four writes, defaulted to the real IPC calls. Overridable so the loop
  // can be tested without a backend; no caller needs to pass them.
  updateNote?: (id: string, patch: { transcript: string }) => Promise<void>;
  noteTimelineRename?: (id: string, oldLabel: string, newLabel: string) => Promise<void>;
  uploadNoteSessions?: (id: string) => Promise<void>;
  reindexNote?: (id: string) => Promise<void>;
  /** Optimistic local update, with the already-rewritten transcript. */
  onRewritten: (id: string, transcript: string) => void;
}): Promise<CrossNoteRenameOutcome> {
  if (oldLabel === newLabel) return { renamed: [], failed: [] };

  const affected = notesWithSpeaker(notes, oldLabel).map((n) => ({
    id: n.id,
    transcript: renameSpeakerInTranscript(n.transcript, oldLabel, newLabel),
  }));

  // Every optimistic update first, synchronously: the UI must not rename one
  // pill per round-trip.
  for (const { id, transcript } of affected) onRewritten(id, transcript);

  const renamed: string[] = [];
  const failed: string[] = [];
  for (const { id, transcript } of affected) {
    try {
      // `notes_update` is what pings `sync.note_upserted`, so the rewrite
      // propagates instead of going dirty on this device only.
      await updateNote(id, { transcript });
      renamed.push(id);
    } catch {
      failed.push(id);
      continue; // Don't touch the timeline of a note whose transcript didn't save.
    }
    // Past this point the transcript — the source of truth — is already saved,
    // so these degrade playback labels or search freshness rather than undoing
    // the rename, and the note stays counted as renamed.
    //
    // One try EACH, deliberately: sharing a try meant a failed timeline rename
    // also skipped the re-upload and the reindex, so a note could silently drop
    // out of chat retrieval freshness while the toast said it was renamed.
    // Logged rather than silent, because "renamed" then means slightly less than
    // it says.
    await settle(`timeline rename for ${id}`, () => noteTimelineRename(id, oldLabel, newLabel));
    // Re-upload rewritten timelines for shared notes (#16); a no-op in Personal.
    await settle(`session upload for ${id}`, () => uploadNoteSessions(id));
    await settle(`reindex for ${id}`, () => reindexNote(id));
  }
  return { renamed, failed };
}

/** Run a non-critical follow-up write, logging rather than failing the rename. */
async function settle(what: string, run: () => Promise<void>): Promise<void> {
  try {
    await run();
  } catch (e) {
    console.warn(`cross-note rename: ${what} failed`, e);
  }
}

/** "Renamed in 12 notes", or the honest partial when some writes failed. */
export function renameOutcomeMessage(outcome: CrossNoteRenameOutcome): string {
  const { renamed, failed } = outcome;
  const total = renamed.length + failed.length;
  if (failed.length > 0) {
    return `Renamed in ${renamed.length} of ${total} — ${failed.length} failed`;
  }
  return `Renamed in ${renamed.length} ${renamed.length === 1 ? "note" : "notes"}`;
}
