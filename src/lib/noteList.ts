import { type Note } from "./ipc";

// Shared helpers for the note-list views (All notes, Folder). Kept here so the
// pages don't each carry their own copy.

// Relative timestamp for a list row: clock time today, else a short date.
export function formatMeetingTime(ts: number): string {
  const d = new Date(ts);
  const now = new Date();
  const sameDay =
    d.getFullYear() === now.getFullYear() &&
    d.getMonth() === now.getMonth() &&
    d.getDate() === now.getDate();
  if (sameDay) {
    return d.toLocaleTimeString(undefined, {
      hour: "2-digit",
      minute: "2-digit",
      hour12: false,
    });
  }
  const sameYear = d.getFullYear() === now.getFullYear();
  return d.toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
    ...(sameYear ? {} : { year: "numeric" }),
  });
}

// Bucket notes into Today / Yesterday / Earlier this week / Older, dropping
// empty buckets. Input should already be sorted newest-first.
export function groupByDate(notes: Note[]) {
  const today = new Date();
  today.setHours(0, 0, 0, 0);
  const yest = new Date(today);
  yest.setDate(today.getDate() - 1);
  const weekStart = new Date(today);
  weekStart.setDate(today.getDate() - 7);

  const groups: { label: string; items: Note[] }[] = [
    { label: "Today", items: [] },
    { label: "Yesterday", items: [] },
    { label: "Earlier this week", items: [] },
    { label: "Older", items: [] },
  ];
  for (const n of notes) {
    const d = new Date(n.updated_at);
    if (d >= today) groups[0].items.push(n);
    else if (d >= yest) groups[1].items.push(n);
    else if (d >= weekStart) groups[2].items.push(n);
    else groups[3].items.push(n);
  }
  return groups.filter((g) => g.items.length > 0);
}

// One-line snippet for a row. The body is Tiptap HTML, so strip tags via a
// detached element (textContent only — never inserted live, so no script
// runs); fall back to the transcript for pure voice memos.
export function notePreview(n: Note): string {
  let text = "";
  const html = n.body?.trim();
  if (html) {
    const el = document.createElement("div");
    el.innerHTML = html;
    text = el.textContent || "";
  }
  if (!text.trim() && n.transcript) text = n.transcript;
  return text.replace(/\s+/g, " ").trim();
}

// A note counts as "recorded" once it has any transcript text; "summarized"
// once a summary has been generated. Used by the All-notes filter chips.
export function isRecorded(n: Note): boolean {
  return n.transcript.trim().length > 0;
}

export function isSummarized(n: Note): boolean {
  return n.summary.trim().length > 0;
}
