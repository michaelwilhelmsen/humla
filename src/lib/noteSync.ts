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
