// Display helpers for the chat history popover (issue #62). Kept pure and
// framework-free so they unit-test without rendering. `relativeTime` is generic
// enough to hoist to a shared module if a second caller ever appears; for now
// the chat history list is its only consumer.

// A short, human relative timestamp: "just now" / "5m ago" / "3h ago" /
// "yesterday" / "4d ago", then a locale date past a week (with the year only
// when it differs). `now` is injectable for deterministic tests.
export function relativeTime(ts: number, now: number = Date.now()): string {
  const diffMs = Math.max(0, now - ts);
  const sec = Math.floor(diffMs / 1000);
  if (sec < 60) return "just now";
  const min = Math.floor(sec / 60);
  if (min < 60) return `${min}m ago`;
  const hr = Math.floor(min / 60);
  if (hr < 24) return `${hr}h ago`;

  // Beyond a day, count whole calendar days so "yesterday" tracks the date
  // boundary rather than a rolling 24h window.
  const then = new Date(ts);
  then.setHours(0, 0, 0, 0);
  const midnight = new Date(now);
  midnight.setHours(0, 0, 0, 0);
  const dayDiff = Math.round((midnight.getTime() - then.getTime()) / 86_400_000);
  if (dayDiff <= 1) return "yesterday";
  if (dayDiff < 7) return `${dayDiff}d ago`;

  const sameYear = new Date(ts).getFullYear() === new Date(now).getFullYear();
  return new Date(ts).toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
    ...(sameYear ? {} : { year: "numeric" }),
  });
}

// The title shown for a conversation row. Titles can be NULL/empty (legacy
// workspace threads, unsent conversations); fall back to a quiet, consistent
// label — the relative date rides alongside as the row's secondary line.
export function conversationTitle(meta: { title: string }): string {
  return meta.title.trim() || "New chat";
}

// Mirror of the backend's `GROUNDING_CHAR_BUDGET` (`chat/mod.rs`). Kept in sync
// by hand — it only drives an advisory hint, so a drift makes the warning
// slightly early or late, never wrong in a way that loses data.
export const GROUNDING_CHAR_BUDGET = 24_000;

// The backend measures the *assembled* reference block, which also carries the
// injection-posture preamble and the three `[Notes]` / `[Transcript]` /
// `[Summary]` headers. Budgeting for that keeps the estimate from missing a band
// of notes that truncate only once the scaffold is counted. Approximate on
// purpose — the wording isn't mirrored, just its rough size.
const SCAFFOLD_CHARS = 250;

/** How much note text fits before the backend starts trimming. */
export const GROUNDING_TEXT_BUDGET = GROUNDING_CHAR_BUDGET - SCAFFOLD_CHARS;

/**
 * Whether this note's reference block is likely to be truncated before the model
 * sees all of it (issue #80).
 *
 * The backend reports truncation authoritatively, but only *after* a turn — too
 * late to be useful. This estimates the same budget from the same three sources
 * so the composer can warn before the user spends a turn. An estimate, hence the
 * hint says "may": the budget is a fixed backend constant (not model-dependent),
 * but the scaffold allowance above is approximate.
 */
export function groundingLikelyTruncated(parts: {
  bodyText: string;
  transcript: string;
  summary: string;
}): boolean {
  const total = parts.bodyText.length + parts.transcript.length + parts.summary.length;
  return total > GROUNDING_TEXT_BUDGET;
}

// The colour tone for the usage meter by fraction of allowance consumed (#69):
// default below 70%, warning at 70–89%, danger at >=90% (and once used >= cap).
// A non-positive cap has no meaningful fraction → default (shouldn't happen for
// a metered workspace, but keeps the function total).
export type UsageTone = "default" | "warning" | "danger";
export function usageTone(used: number, cap: number): UsageTone {
  if (cap <= 0) return "default";
  const ratio = used / cap;
  if (used >= cap || ratio >= 0.9) return "danger";
  if (ratio >= 0.7) return "warning";
  return "default";
}

// Role-aware copy for the BYOK error taxonomy in the LIVE chat pane (#76): the
// workspace key was rejected / its OpenAI account is out of quota / the key is
// transiently unavailable. Owner gets a fix path (workspace settings); a member
// gets "ask {owner}". Returns null for any other reason (incl. the managed
// add-on's `quota_exhausted`, which keeps its own upsell copy) so the caller
// falls back to the server-mapped message string.
export function liveChatErrorCopy(
  reason: string | null | undefined,
  opts: { isOwner: boolean; ownerName: string },
): string | null {
  const { isOwner, ownerName } = opts;
  switch (reason) {
    case "byok_key_invalid":
      return isOwner
        ? "This workspace's OpenAI key was rejected. Re-enter it in Organization → Workspace chat."
        : `This workspace's OpenAI key was rejected — ask ${ownerName} to fix it.`;
    case "byok_provider_quota":
      return isOwner
        ? "This workspace's OpenAI account is out of quota. Check it in Organization → Workspace chat."
        : `This workspace's OpenAI account is out of quota — ask ${ownerName} to check it.`;
    case "byok_key_unavailable":
      return "Workspace chat is temporarily unavailable — try again shortly.";
    default:
      return null;
  }
}
