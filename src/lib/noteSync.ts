/**
 * Should a body that arrived from the store (i.e. from a cloud pull) replace the
 * one the open Note view is holding?
 *
 * A workspace note syncs in stages: the row lands, and the body follows once the
 * author's debounce and the outbox catch up. The Note view seeds its draft once
 * at mount, so mounting between those two stages used to leave it holding an
 * empty body for as long as the view stayed open — and typing into that blank
 * editor pushed the emptiness back up, where last-write-wins beat the real body.
 *
 * The rule: adopt a remote body only into an empty draft with nothing queued for
 * save. That fixes the blank note without ever overwriting an edit in flight —
 * `pendingBody` is the debounced save that hasn't fired yet, and a non-empty
 * local body means the user has content we must not discard.
 */
export function shouldAdoptRemoteBody(
  localBody: string | null | undefined,
  remoteBody: string | null | undefined,
  pendingBody: boolean,
): boolean {
  if (pendingBody) return false;
  if ((localBody ?? "").trim() !== "") return false;
  return (remoteBody ?? "").trim() !== "";
}

/**
 * May Humla overwrite this title with a generated one? (#90)
 *
 * MIRRORS `menubar::is_replaceable_title` in Rust — change both. The backend is
 * the authority (it re-checks under the DB lock before writing), but the open
 * Note view has to answer the same question locally to decide whether a title
 * arriving from the store may replace the one in its draft.
 *
 * Three titles are Humla's own: an empty one, a `Recording 19 Aug 14:32`
 * timestamp, and the `Imported audio` fallback for a file with no usable name.
 * Anything else — including a real filename stem — is the user's.
 */
const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

export function isReplaceableTitle(title: string | null | undefined): boolean {
  const t = (title ?? "").trim();
  if (t === "" || t === "Imported audio") return true;
  // `Recording <d> <Mon> <HH:MM>`, the shape `headless_note_title` writes. The
  // Rust side parses this with chrono, so the ranges are checked there — a
  // loose regex here would have this side adopt over `Recording 99 Zzz 99:99`,
  // a title the backend calls user-owned.
  const m = /^Recording (\d{1,2}) ([A-Z][a-z]{2}) (\d{2}):(\d{2})$/.exec(t);
  if (!m) return false;
  const [, day, month, hour, minute] = m;
  if (!MONTHS.includes(month)) return false;
  return (
    +day >= 1 && +day <= 31 && +hour >= 0 && +hour <= 23 && +minute >= 0 && +minute <= 59
  );
}

/**
 * Should a title that arrived from the store replace the one the open Note view
 * is holding? (#90)
 *
 * The automatic titler is a backend write: it lands in the store via
 * `notes_changed`, and without this the new title reaches the sidebar while the
 * open note's title box still shows `Recording 19 Aug 14:32`.
 *
 * Same guarded-adopt shape as `shouldAdoptRemoteBody`, and guarded twice for the
 * same reason: `pendingTitle` is a debounced save that hasn't fired yet, and a
 * user-owned draft title is content we must not discard. Regenerate-on-demand
 * does NOT come through here — it writes the returned title into the draft
 * directly, because there the user asked to lose what was in it.
 */
export function shouldAdoptRemoteTitle(
  localTitle: string | null | undefined,
  remoteTitle: string | null | undefined,
  pendingTitle: boolean,
): boolean {
  if (pendingTitle) return false;
  if (!isReplaceableTitle(localTitle)) return false;
  return (remoteTitle ?? "").trim() !== "";
}

/**
 * How much typed body a note needs before a title is worth asking a model for
 * (#90). Roughly a short paragraph — below this there is nothing to name, and a
 * two-word note titles itself better than any model would.
 */
export const TITLE_MIN_BODY_CHARS = 160;

/**
 * Should the open Note view ask for a title at its body-settled checkpoint? (#90)
 *
 * The post-stop recording chain titles anything that was recorded. This is the
 * other half — a note that is typed and never recorded — and it deliberately
 * stands down for anything the recording path owns:
 *
 * - a capture in flight, so no model call is ever made during a recording;
 * - a note that has a transcript, whose title is the post-stop chain's to write;
 * - a title that isn't ours, and a note the user can't edit anyway.
 *
 * The backend re-checks eligibility before spending anything, so this is about
 * not making a pointless IPC call, not about correctness.
 */
export function shouldRequestTitleForBody(input: {
  title: string | null | undefined;
  bodyText: string;
  hasTranscript: boolean;
  recording: boolean;
  readOnly: boolean;
}): boolean {
  if (input.readOnly || input.recording || input.hasTranscript) return false;
  if (!isReplaceableTitle(input.title)) return false;
  return input.bodyText.trim().length >= TITLE_MIN_BODY_CHARS;
}
