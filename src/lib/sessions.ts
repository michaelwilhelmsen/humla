// Pure helpers for the recording-sessions feature (#16): formatting session
// metadata for the carousel, and grouping the merged (multi-session) timeline
// into render groups with session boundaries for dividers.

import type { NoteSession, TimelineEntry } from "./ipc";

/// Format a millisecond duration as `m:ss` (or `h:mm:ss` past an hour).
export function formatDuration(ms: number): string {
  const totalSec = Math.max(0, Math.round(ms / 1000));
  const h = Math.floor(totalSec / 3600);
  const m = Math.floor((totalSec % 3600) / 60);
  const s = totalSec % 60;
  const ss = String(s).padStart(2, "0");
  if (h > 0) return `${h}:${String(m).padStart(2, "0")}:${ss}`;
  return `${m}:${ss}`;
}

/// What a session is called wherever one is named — the reader's divider and
/// the caption fallback below.
///
/// Index 0 is the synthesized session that repair-on-open writes for
/// transcript text no take accounts for (#169). It never was a recording, so
/// it is named rather than numbered: "Recording 0" would promise a take there
/// is nothing to play.
export function sessionTitle(index: number): string {
  return index > 0 ? `Recording ${index}` : "Earlier transcript";
}

/// One-line caption for a session pill's tooltip / divider: local date-time
/// plus duration, falling back to the session's title when nothing is known
/// (e.g. a legacy flat session with no started_at / duration).
export function formatSessionCaption(session: NoteSession): string {
  const parts: string[] = [];
  if (session.startedAt) {
    const d = new Date(session.startedAt);
    if (!Number.isNaN(d.getTime())) {
      parts.push(
        d.toLocaleString(undefined, {
          month: "short",
          day: "numeric",
          hour: "2-digit",
          minute: "2-digit",
        }),
      );
    }
  }
  if (session.durationMs > 0) parts.push(formatDuration(session.durationMs));
  return parts.length > 0 ? parts.join(" · ") : sessionTitle(session.index);
}

export type TimelineGroup = {
  label: string;
  sessionId: string;
  sessionIndex: number;
  // True for the first group of each session in document order — the anchor
  // for a session divider (rendered only when the note has 2+ sessions).
  firstInSession: boolean;
  // Indices back into the flat (merged) timeline array, so per-chunk edit
  // IPCs can recover each entry's session id + local chunk index.
  indices: number[];
  startMs: number;
  endMs: number;
  text: string;
  words: { text: string; start_ms: number; end_ms: number }[];
  // Word count contributed by each constituent chunk, so the active-word
  // highlight can map (chunk, word-in-chunk) → flat position in `words`.
  wordCountByChunk: number[];
};

/// Collapse consecutive same-speaker timeline entries into render groups,
/// breaking a group at every speaker change AND every session boundary (so a
/// group never straddles two takes). Marks the first group of each session so
/// the reader can drop a divider there.
export function groupTimeline(timeline: TimelineEntry[]): TimelineGroup[] {
  const out: TimelineGroup[] = [];
  const seenSessions = new Set<string>();
  for (let i = 0; i < timeline.length; i++) {
    const e = timeline[i];
    const ws = e.words ?? [];
    const label = e.label || "";
    const last = out[out.length - 1];
    if (last && last.label === label && last.sessionId === e.sessionId) {
      last.indices.push(i);
      last.endMs = Math.max(last.endMs, e.endMs);
      // Skip empty entries when joining: a per-turn edit (#170) writes the new
      // text into the run's lowest entry and empties the rest (their indices
      // must survive, since a chunk index IS a line position), so joining
      // blindly would append a trailing space per emptied entry.
      last.text = last.text && e.text ? `${last.text} ${e.text}` : last.text || e.text;
      last.words.push(...ws);
      last.wordCountByChunk.push(ws.length);
    } else {
      const firstInSession = !seenSessions.has(e.sessionId);
      seenSessions.add(e.sessionId);
      out.push({
        label,
        sessionId: e.sessionId,
        sessionIndex: e.sessionIndex,
        firstInSession,
        indices: [i],
        startMs: e.startMs,
        endMs: e.endMs,
        text: e.text,
        words: [...ws],
        wordCountByChunk: [ws.length],
      });
    }
  }
  return out;
}

/// Which pill is visually "active": the session being played while playing;
/// otherwise the topmost session divider currently in view (scroll
/// orientation). Falls back to the first session so a pill is always lit.
export function resolveActivePill(opts: {
  playing: boolean;
  playheadSessionId: string | null;
  topVisibleSessionId: string | null;
  sessions: NoteSession[];
}): string | null {
  const { playing, playheadSessionId, topVisibleSessionId, sessions } = opts;
  if (playing && playheadSessionId) return playheadSessionId;
  if (topVisibleSessionId) return topVisibleSessionId;
  return sessions[0]?.id ?? null;
}

/// Whether opening this note should ask the workspace for its per-session
/// assets. Shared notes only — a Personal note's takes never left the device.
///
/// **A missing timeline counts, not just missing audio.** The gate used to be
/// "no local playback", which quietly excluded the case this exists for: the
/// legacy single-file fallback writes a flat `playback.wav` and no timeline at
/// all, so one trip through it left the note looking "already local" forever
/// and the per-session pull — the only thing that fetches `timeline.jsonl` —
/// was never attempted again. Without word timings the note falls back to the
/// plain reader, which shows no speaker labels, so a teammate's meeting read as
/// a single anonymous wall of text.
export function needsSessionPull(opts: {
  shared: boolean;
  hasLocalPlayback: boolean;
  timelineEntries: number;
}): boolean {
  const { shared, hasLocalPlayback, timelineEntries } = opts;
  if (!shared) return false;
  return !hasLocalPlayback || timelineEntries === 0;
}
