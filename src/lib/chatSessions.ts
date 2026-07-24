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
