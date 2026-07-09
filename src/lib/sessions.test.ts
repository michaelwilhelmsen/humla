import { describe, it, expect } from "vitest";
import {
  formatDuration,
  formatSessionCaption,
  groupTimeline,
  resolveActivePill,
  type TimelineGroup,
} from "./sessions";
import type { NoteSession, TimelineEntry } from "./ipc";

function entry(p: Partial<TimelineEntry>): TimelineEntry {
  return {
    start_ms: 0,
    end_ms: 1000,
    label: "",
    text: "",
    words: [],
    sessionId: "s1",
    sessionIndex: 1,
    chunkIdx: 0,
    ...p,
  };
}

function session(p: Partial<NoteSession>): NoteSession {
  return {
    id: "s1",
    index: 1,
    startedAt: "",
    durationMs: 0,
    streams: [],
    hasPlayback: true,
    ...p,
  };
}

describe("formatDuration", () => {
  it("formats sub-hour as m:ss", () => {
    expect(formatDuration(0)).toBe("0:00");
    expect(formatDuration(5_000)).toBe("0:05");
    expect(formatDuration(312_000)).toBe("5:12");
  });
  it("formats past an hour as h:mm:ss", () => {
    expect(formatDuration(3_661_000)).toBe("1:01:01");
  });
});

describe("formatSessionCaption", () => {
  it("falls back to Recording N with no metadata (legacy session)", () => {
    expect(formatSessionCaption(session({ index: 2 }))).toBe("Recording 2");
  });
  it("includes duration when known", () => {
    expect(formatSessionCaption(session({ durationMs: 312_000 }))).toContain("5:12");
  });
});

describe("groupTimeline", () => {
  it("merges consecutive same-speaker entries within a session", () => {
    const groups = groupTimeline([
      entry({ label: "Speaker 1", text: "hello", chunkIdx: 0 }),
      entry({ label: "Speaker 1", text: "there", chunkIdx: 1 }),
      entry({ label: "Speaker 2", text: "hi", chunkIdx: 2 }),
    ]);
    expect(groups).toHaveLength(2);
    expect(groups[0].text).toBe("hello there");
    expect(groups[0].indices).toEqual([0, 1]);
    expect(groups[1].label).toBe("Speaker 2");
  });

  it("breaks groups at a session boundary even for the same label", () => {
    // Both sessions locally start their speaker numbering; the merged
    // timeline carries offset labels, but even identical labels must not
    // merge across a session boundary.
    const groups = groupTimeline([
      entry({ label: "Speaker 1", text: "take one", sessionId: "s1", sessionIndex: 1 }),
      entry({ label: "Speaker 1", text: "take two", sessionId: "s2", sessionIndex: 2 }),
    ]);
    expect(groups).toHaveLength(2);
    expect(groups[0].sessionIndex).toBe(1);
    expect(groups[1].sessionIndex).toBe(2);
  });

  it("marks the first group of each session for dividers", () => {
    const groups = groupTimeline([
      entry({ label: "Speaker 1", text: "a", sessionId: "s1", sessionIndex: 1 }),
      entry({ label: "Speaker 2", text: "b", sessionId: "s1", sessionIndex: 1 }),
      entry({ label: "Speaker 3", text: "c", sessionId: "s2", sessionIndex: 2 }),
    ]);
    expect(groups.map((g) => g.firstInSession)).toEqual([true, false, true]);
  });

  it("reader fix: every session's text survives in the merged document", () => {
    // The field-report bug was that only the last take's text rendered.
    // The merged timeline must surface BOTH takes.
    const groups = groupTimeline([
      entry({ label: "Speaker 1", text: "first take words", sessionId: "s1", sessionIndex: 1 }),
      entry({ label: "Speaker 2", text: "second take words", sessionId: "s2", sessionIndex: 2 }),
    ]);
    const allText = groups.map((g: TimelineGroup) => g.text).join(" ");
    expect(allText).toContain("first take words");
    expect(allText).toContain("second take words");
  });
});

describe("resolveActivePill", () => {
  const sessions = [session({ id: "s1", index: 1 }), session({ id: "s2", index: 2 })];

  it("follows the playhead's session while playing (overrides scroll)", () => {
    expect(
      resolveActivePill({
        playing: true,
        playheadSessionId: "s2",
        topVisibleSessionId: "s1",
        sessions,
      }),
    ).toBe("s2");
  });

  it("follows the topmost visible session divider when idle", () => {
    expect(
      resolveActivePill({
        playing: false,
        playheadSessionId: "s2",
        topVisibleSessionId: "s1",
        sessions,
      }),
    ).toBe("s1");
  });

  it("falls back to the first session when nothing else is known", () => {
    expect(
      resolveActivePill({
        playing: false,
        playheadSessionId: null,
        topVisibleSessionId: null,
        sessions,
      }),
    ).toBe("s1");
  });
});
