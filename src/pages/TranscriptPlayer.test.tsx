import { describe, it, expect, beforeAll, beforeEach, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { TranscriptPlayer } from "./Note";
import { mockTauri } from "../test/tauri";
import { mockLayoutBox } from "../test/layout";
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
  // @tanstack/react-virtual measures rows off the layout box, which jsdom
  // reports as zero — no rows means no word spans to click.
  mockLayoutBox();
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
    canTranscribe: false,
    ...over,
  };
}

function renderPlayer(props: {
  sessions: NoteSession[];
  timeline: TimelineEntry[];
  fallbackPlaybackUrl: string | null;
  audioAvailable?: boolean;
  keepAudio?: boolean;
  disabled?: boolean;
  setTimeline?: React.Dispatch<React.SetStateAction<TimelineEntry[]>>;
}) {
  return render(
    <TranscriptPlayer
      noteId="note-1"
      timeline={props.timeline}
      setTimeline={props.setTimeline ?? vi.fn()}
      sessions={props.sessions}
      fallbackPlaybackUrl={props.fallbackPlaybackUrl}
      audioAvailable={props.audioAvailable ?? true}
      keepAudio={props.keepAudio ?? true}
      disabled={props.disabled ?? false}
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

// #24: with keep_audio off there is no playback.wav, but timeline.jsonl is
// still written — so the styled reader (speaker pills, session dividers,
// rename) must survive without anything to play.
describe("TranscriptPlayer without audio (#24)", () => {
  beforeEach(() => {
    mockTauri({ note_session_playback_path: () => null });
  });

  const timeline = [
    entry({
      sessionId: "s1",
      sessionIndex: 0,
      label: "Michael",
      text: "hello",
      words: [{ text: "hello", start_ms: 2000, end_ms: 3000 }],
    }),
  ];

  it("renders the reader with no <audio> element and points at the setting", async () => {
    const { container } = renderPlayer({
      sessions: [session({ id: "s1", hasPlayback: false })],
      timeline,
      fallbackPlaybackUrl: null,
      audioAvailable: false,
      keepAudio: false,
    });

    // The transcript still reads, with its labels.
    expect(await screen.findByText("hello")).toBeInTheDocument();
    // No player: an <audio> with no source is the "jarring" thing to avoid.
    expect(audioEl(container)).toBeNull();
    expect(
      screen.getByText(/audio not stored on this device/i),
    ).toBeInTheDocument();
  });

  it("clicking a word is inert rather than throwing when there is no player", async () => {
    renderPlayer({
      sessions: [session({ id: "s1", hasPlayback: false })],
      timeline,
      fallbackPlaybackUrl: null,
      audioAvailable: false,
      keepAudio: false,
    });
    const word = await screen.findByText("hello");
    expect(() => fireEvent.click(word)).not.toThrow();
  });

  it("says the audio is simply gone when retention is on (an old note)", async () => {
    renderPlayer({
      sessions: [session({ id: "s1", hasPlayback: false })],
      timeline,
      fallbackPlaybackUrl: null,
      audioAvailable: false,
      keepAudio: true,
    });
    // Don't send a user to a setting that is already on.
    expect(await screen.findByText(/no audio saved/i)).toBeInTheDocument();
    expect(
      screen.queryByText(/audio not stored on this device/i),
    ).not.toBeInTheDocument();
  });
});

// #170: a free-text edit used to write `note.transcript` directly and touch no
// timeline, so on a timeline-backed note the edit was invisible in the styled
// reader (which renders from the timeline) while summary, chat and embeddings
// read the edited string. Editing is now per-turn and lands in the timeline;
// the transcript is re-derived from it by the backend.
describe("TranscriptPlayer per-turn editing (#170)", () => {
  const calls: { cmd: string; args: unknown }[] = [];

  beforeEach(() => {
    calls.length = 0;
    const record = (cmd: string) => (args: unknown) => {
      calls.push({ cmd, args });
      return null;
    };
    mockTauri({
      note_session_playback_path: () => null,
      note_timeline_set_chunk_text: record("note_timeline_set_chunk_text"),
      cloud_upload_note_sessions: record("cloud_upload_note_sessions"),
    });
  });

  // One rendered turn spanning two timeline entries — the case a
  // single-index command would have forced into two non-atomic calls.
  const turn = [
    entry({
      sessionId: "s1",
      sessionIndex: 0,
      label: "Michael",
      text: "so where did we",
      start_ms: 0,
      end_ms: 1000,
      chunkIdx: 0,
      words: [{ text: "so", start_ms: 0, end_ms: 1000 }],
    }),
    entry({
      sessionId: "s1",
      sessionIndex: 0,
      label: "Michael",
      text: "land on the freeze",
      start_ms: 1000,
      end_ms: 2000,
      chunkIdx: 1,
      words: [{ text: "land", start_ms: 1000, end_ms: 2000 }],
    }),
  ];

  async function openTurnEditor() {
    const edit = await screen.findByRole("button", { name: /edit this turn/i });
    // mouseDown, not click: the affordance opens on mousedown so an already-open
    // textarea's blur can't re-render the row out from under the gesture.
    fireEvent.mouseDown(edit);
    return screen.getByRole("textbox") as HTMLTextAreaElement;
  }

  it("sends the whole turn's chunk indices in one call and re-uploads the timeline", async () => {
    const setTimeline = vi.fn();
    renderPlayer({
      sessions: [session({ id: "s1", hasPlayback: false })],
      timeline: turn,
      fallbackPlaybackUrl: null,
      audioAvailable: false,
      setTimeline,
    });

    const ta = await openTurnEditor();
    fireEvent.change(ta, { target: { value: "so where did we land on the freeze?" } });
    fireEvent.blur(ta);

    await waitFor(() =>
      expect(calls.filter((c) => c.cmd === "note_timeline_set_chunk_text")).toHaveLength(1),
    );
    const call = calls.find((c) => c.cmd === "note_timeline_set_chunk_text")!;
    expect(call.args).toMatchObject({
      noteId: "note-1",
      sessionId: "s1",
      chunkIdxs: [0, 1],
      newText: "so where did we land on the freeze?",
    });
    // The optimistic local update keeps the reader showing the edit before
    // `transcript_replaced` lands.
    expect(setTimeline).toHaveBeenCalled();
    // A workspace note's rewritten timeline is pushed; Personal short-circuits
    // in the backend.
    await waitFor(() =>
      expect(calls.some((c) => c.cmd === "cloud_upload_note_sessions")).toBe(true),
    );
  });

  it("enter commits, since a turn is one transcript line", async () => {
    renderPlayer({
      sessions: [session({ id: "s1", hasPlayback: false })],
      timeline: turn,
      fallbackPlaybackUrl: null,
      audioAvailable: false,
    });

    const ta = await openTurnEditor();
    fireEvent.change(ta, { target: { value: "committed by enter" } });
    fireEvent.keyDown(ta, { key: "Enter" });

    await waitFor(() =>
      expect(
        calls.find((c) => c.cmd === "note_timeline_set_chunk_text")?.args,
      ).toMatchObject({ newText: "committed by enter" }),
    );
    expect(screen.queryByRole("textbox")).toBeNull();
  });

  it("opening another turn commits the one already open", async () => {
    // The pencil opens on mousedown with the default prevented, so the open
    // turn never blurs — without an explicit commit the typed edit would be
    // silently dropped when its textarea unmounts.
    renderPlayer({
      sessions: [session({ id: "s1", hasPlayback: false })],
      timeline: [
        ...turn,
        entry({
          sessionId: "s1",
          sessionIndex: 0,
          label: "Hege",
          text: "next Friday",
          start_ms: 2000,
          end_ms: 3000,
          chunkIdx: 2,
        }),
      ],
      fallbackPlaybackUrl: null,
      audioAvailable: false,
    });

    const pencils = await screen.findAllByRole("button", { name: /edit this turn/i });
    fireEvent.mouseDown(pencils[0]);
    fireEvent.change(screen.getByRole("textbox"), { target: { value: "typed but not blurred" } });
    // The open turn hides its own pencil, so what's left is the other turn's.
    const remaining = screen.getAllByRole("button", { name: /edit this turn/i });
    expect(remaining).toHaveLength(1);
    fireEvent.mouseDown(remaining[0]);

    await waitFor(() =>
      expect(
        calls.find((c) => c.cmd === "note_timeline_set_chunk_text")?.args,
      ).toMatchObject({ chunkIdxs: [0, 1], newText: "typed but not blurred" }),
    );
    // …and the second turn is now the one open.
    expect((screen.getByRole("textbox") as HTMLTextAreaElement).value).toBe("next Friday");
  });

  it("escape cancels without writing anything", async () => {
    renderPlayer({
      sessions: [session({ id: "s1", hasPlayback: false })],
      timeline: turn,
      fallbackPlaybackUrl: null,
      audioAvailable: false,
    });

    const ta = await openTurnEditor();
    fireEvent.change(ta, { target: { value: "discarded" } });
    fireEvent.keyDown(ta, { key: "Escape" });

    expect(screen.queryByRole("textbox")).toBeNull();
    expect(calls.some((c) => c.cmd === "note_timeline_set_chunk_text")).toBe(false);
  });

  it("an unchanged turn writes nothing", async () => {
    renderPlayer({
      sessions: [session({ id: "s1", hasPlayback: false })],
      timeline: turn,
      fallbackPlaybackUrl: null,
      audioAvailable: false,
    });

    const ta = await openTurnEditor();
    expect(ta.value).toBe("so where did we land on the freeze");
    fireEvent.blur(ta);
    expect(calls.some((c) => c.cmd === "note_timeline_set_chunk_text")).toBe(false);
  });

  it("offers no editing while a recording is in flight", async () => {
    renderPlayer({
      sessions: [session({ id: "s1", hasPlayback: false })],
      timeline: turn,
      fallbackPlaybackUrl: null,
      audioAvailable: false,
      disabled: true,
    });

    // The turn still reads (its words render as click-to-seek spans).
    expect(await screen.findByText("so")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /edit this turn/i })).toBeNull();
  });

  it("no longer offers a whole-transcript textarea", async () => {
    renderPlayer({
      sessions: [session({ id: "s1", hasPlayback: false })],
      timeline: turn,
      fallbackPlaybackUrl: null,
      audioAvailable: false,
    });

    expect(await screen.findByText("so")).toBeInTheDocument();
    // The panel-wide `Edit` affordance is gone — it wrote the derived copy.
    expect(screen.queryByRole("button", { name: /^edit$/i })).toBeNull();
    expect(screen.queryByRole("textbox")).toBeNull();
  });
});
