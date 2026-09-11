import type { Step } from "./ipc";

/**
 * What each named step of a chain is called. One map, because the stop and the
 * replay draw from one `Step` vocabulary — a user who stops a recording and a
 * user who presses Transcribe are watching the same work. It is shared with the
 * note grid's activity line for the same reason: the bar and the card must not
 * name the same step differently. Labels stay short: the row they sit in has
 * only a few pixels of slack.
 */
export const STEP_LABELS: Record<Step, string> = {
  transcribing: "Transcribing",
  saving_audio: "Saving audio",
  diarizing: "Identifying speakers",
  writing_playback: "Writing playback",
  matching_speakers: "Matching speakers",
};
