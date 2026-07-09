import { describe, it, expect, beforeAll, beforeEach, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { TranscriptPlayer } from "./Note";
import { mockTauri } from "../test/tauri";
import type { NoteSession, TimelineEntry } from "../lib/ipc";

// TranscriptPlayer drives an <audio> element and @tanstack/react-virtual, neither
// of which jsdom implements. Stub the media element (play/pause/load, a settable
// currentTime, a ready readyState) and ResizeObserver (virtualizer measurement)
// so the component mounts and seeks are observable.
beforeAll(() => {
  const proto = window.HTMLMediaElement.prototype;
  proto.play = vi.fn(() => Promise.resolve());
  proto.pause = vi.fn();
  proto.load = vi.fn();
  Object.defineProperty(proto, "readyState", {
    configurable: true,
    get() {
      return 4; // HAVE_ENOUGH_DATA — always "loaded" in tests
    },
  });
  Object.defineProperty(proto, "currentTime", {
    configurable: true,
    get() {
      return (this as unknown as { __ct?: number }).__ct ?? 0;
    },
    set(v: number) {
      (this as unknown as { __ct?: number }).__ct = v;
    },
  });
  if (!("ResizeObserver" in window)) {
    (window as unknown as { ResizeObserver: unknown }).ResizeObserver = class {
      observe() {}
      unobserve() {}
      disconnect() {}
    };
  }
  // jsdom pins offsetWidth/offsetHeight to 0; @tanstack/react-virtual reads them
  // to size the scroll window and measure rows, so with zero height it renders
  // no rows (no word spans to click). Report a non-zero box so the timeline
  // groups render.
  Object.defineProperty(window.HTMLElement.prototype, "offsetHeight", {
    configurable: true,
    get() {
      return 600;
    },
  });
  Object.defineProperty(window.HTMLElement.prototype, "offsetWidth", {
    configurable: true,
    get() {
      return 400;
    },
  });
});

function entry(over: Partial<TimelineEntry>): TimelineEntry {
  return {
    start_ms: 0,
    end_ms: 1000,
    label: "",
    text: "",
    words: [],
    sessionId: "",
    sessionIndex: 0,
    chunkIdx: 0,
    ...over,
  };
}

function session(over: Partial<NoteSession>): NoteSession {
  return {
    id: "",
    index: 1,
    startedAt: "",
    durationMs: 1000,
    streams: ["mic"],
    hasPlayback: true,
    ...over,
  };
}

function renderPlayer(props: {
  sessions: NoteSession[];
  timeline: TimelineEntry[];
  fallbackPlaybackUrl: string | null;
}) {
  return render(
    <TranscriptPlayer
      noteId="note-1"
      timeline={props.timeline}
      setTimeline={vi.fn()}
      sessions={props.sessions}
      fallbackPlaybackUrl={props.fallbackPlaybackUrl}
      transcript="hello world"
      onChange={vi.fn()}
      disabled={false}
      bottomAligned={false}
    />,
  );
}

const audioEl = (c: HTMLElement) => c.querySelector("audio") as HTMLAudioElement;

describe("TranscriptPlayer session seek (BUG A/B)", () => {
  beforeEach(() => {
    // Both sessions lack a per-session playback file, so each resolves to the
    // SAME fallback url — the <audio> src never changes on switch, so no
    // loadeddata fires. This is the stranded-seek condition.
    mockTauri({ note_session_playback_path: () => null });
  });

  it("seeks immediately when the target session resolves to the already-loaded url", async () => {
    const timeline = [
      entry({ sessionId: "s1", sessionIndex: 0, text: "hello", start_ms: 0, end_ms: 1000, words: [{ text: "hello", start_ms: 0, end_ms: 1000 }] }),
      entry({ sessionId: "s2", sessionIndex: 1, text: "world", start_ms: 0, end_ms: 1000, words: [{ text: "world", start_ms: 5000, end_ms: 6000 }] }),
    ];
    const { container } = renderPlayer({
      sessions: [session({ id: "s1", index: 1 }), session({ id: "s2", index: 2 })],
      timeline,
      fallbackPlaybackUrl: "asset://legacy-playback.wav",
    });

    // Active session starts at s1. Click a word belonging to s2 — a different
    // session that resolves to the same fallback url.
    const word = await screen.findByText("world");
    fireEvent.click(word);

    // The seek is applied inline (no loadeddata ever fires because the src is
    // unchanged). Before the fix this stayed 0 and the seek stranded.
    const audio = audioEl(container);
    await waitFor(() => expect(audio.currentTime).toBe(5));
    expect(audio.play).toHaveBeenCalled();
  });

  it("legacy zero-session note: text renders and click-to-seek still works", async () => {
    // A legacy note: no sessions, timeline entries carry an unmatched sessionId.
    const timeline = [
      entry({ sessionId: "legacy", sessionIndex: 0, text: "hello", words: [{ text: "hello", start_ms: 2000, end_ms: 3000 }] }),
    ];
    const { container } = renderPlayer({
      sessions: [],
      timeline,
      fallbackPlaybackUrl: "asset://legacy-playback.wav",
    });

    // Reader text renders in full (the field-report regression class must stay
    // fixed even when no session matches the timeline).
    const word = await screen.findByText("hello");
    fireEvent.click(word);

    const audio = audioEl(container);
    await waitFor(() => expect(audio.currentTime).toBe(2));
  });
});
