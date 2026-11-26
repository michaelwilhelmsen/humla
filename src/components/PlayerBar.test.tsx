import { describe, expect, it, vi } from "vitest";
import { createRef } from "react";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { PlayerBar, nextPlaybackRate } from "./PlayerBar";
import type { NoteSession } from "../lib/ipc";

function session(id: string, index: number): NoteSession {
  return {
    id,
    index,
    startedAt: "",
    durationMs: 60_000,
    streams: ["mic"],
    hasPlayback: true,
    canTranscribe: false,
    canRetranscribe: false,
  };
}

function renderBar(sessions: NoteSession[]) {
  const audio = document.createElement("audio");
  const play = vi.fn(() => Promise.resolve());
  Object.defineProperty(audio, "play", { value: play });
  const ref = createRef<HTMLAudioElement>() as { current: HTMLAudioElement | null };
  ref.current = audio;
  const onSelectSession = vi.fn();
  render(
    <PlayerBar
      audioRef={ref}
      src="asset://take.m4a"
      sessions={sessions}
      activeSessionId={sessions[0]?.id ?? null}
      onSelectSession={onSelectSession}
    />,
  );
  return { audio, play, onSelectSession };
}

describe("PlayerBar", () => {
  it("steps through the speeds and comes back round", () => {
    expect(nextPlaybackRate(1)).toBe(1.25);
    expect(nextPlaybackRate(2)).toBe(0.75);
    expect(nextPlaybackRate(0.75)).toBe(1);
  });

  it("plays the element it is given", async () => {
    const { play } = renderBar([session("s1", 1)]);
    await userEvent.click(screen.getByRole("button", { name: "Play" }));
    expect(play).toHaveBeenCalled();
  });

  it("sets the speed on the element", async () => {
    const { audio } = renderBar([session("s1", 1)]);
    await userEvent.click(screen.getByRole("button", { name: "Speed 1×" }));
    expect(audio.playbackRate).toBe(1.25);
    expect(screen.getByRole("button", { name: "Speed 1.25×" })).toBeInTheDocument();
  });

  it("offers a choice of recording only when there is more than one", () => {
    renderBar([session("s1", 1)]);
    expect(screen.queryByRole("combobox", { name: "Recording" })).toBeNull();
  });

  it("names each recording in the choice", () => {
    renderBar([session("s1", 1), session("s2", 2)]);
    expect(screen.getByRole("combobox", { name: "Recording" })).toHaveTextContent("Recording 1");
  });
});
