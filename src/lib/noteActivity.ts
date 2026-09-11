import { STEP_LABELS } from "./recordingSteps";
import type { RecordingState } from "./store";

/** What a note card's state line says instead of its static state, while
    something is happening to that note. */
export type NoteActivity = { label: string; color: string; animated: boolean };

/** The slices of the recording store an activity can be read from. */
export type ActivitySlices = Pick<
  RecordingState,
  "status" | "transcribing" | "diarizing" | "summarizing"
>;

const RECORD = "var(--color-record)";
const INTERACTIVE = "var(--color-interactive)";

/**
 * What is happening to one note right now, or null when nothing is.
 *
 * First match wins, coarsest first: a live capture is the note's whole state,
 * and the per-note background passes below it can only be true of a note that
 * is not being recorded. Red is the capture itself; everything after it is
 * work over audio that already exists.
 *
 * A paused capture is frozen rather than pulsing, the way the bar's timer is.
 * `titling` is deliberately absent — it is sub-second, and a label that flashes
 * is worse than none.
 */
export function noteActivity(noteId: string, s: ActivitySlices): NoteActivity | null {
  if (s.status.noteId === noteId) {
    switch (s.status.phase) {
      case "starting":
        return { label: "Starting…", color: RECORD, animated: true };
      case "recording":
        return { label: "Recording", color: RECORD, animated: true };
      case "paused":
        return { label: "Paused", color: RECORD, animated: false };
      case "stopping":
        return { label: "Finishing transcript…", color: INTERACTIVE, animated: true };
      case "diarizing":
        return {
          label: `${s.status.step ? STEP_LABELS[s.status.step] : "Identifying speakers"}…`,
          color: INTERACTIVE,
          animated: true,
        };
      case "importing":
        return { label: "Transcribing audio…", color: INTERACTIVE, animated: true };
    }
  }
  const run = s.transcribing[noteId];
  if (run) {
    return {
      label: `${STEP_LABELS[run.step ?? "transcribing"]}…`,
      color: INTERACTIVE,
      animated: true,
    };
  }
  if (s.diarizing[noteId]) {
    return { label: "Identifying speakers…", color: INTERACTIVE, animated: true };
  }
  if (s.summarizing[noteId]) {
    return { label: "Summarizing…", color: INTERACTIVE, animated: true };
  }
  return null;
}
