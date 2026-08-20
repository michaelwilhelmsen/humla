import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { RecordingSessions } from "./RecordingSessions";
import type { NoteSession } from "../lib/ipc";

function session(p: Partial<NoteSession>): NoteSession {
  return {
    id: "s1",
    index: 1,
    startedAt: "",
    durationMs: 0,
    streams: [],
    hasPlayback: true,
    canTranscribe: false,
    canRetranscribe: false,
    ...p,
  };
}

const TWO = [
  session({ id: "s1", index: 1, durationMs: 60_000 }),
  session({ id: "s2", index: 2, durationMs: 120_000 }),
];

describe("RecordingSessions carousel", () => {
  it("renders nothing for a single session (parity with today)", () => {
    const { container } = render(
      <RecordingSessions sessions={[session({})]} activeId="s1" onSelect={vi.fn()} />,
    );
    expect(container).toBeEmptyDOMElement();
  });

  it("renders a numbered pill per session under a Recording sessions eyebrow", () => {
    render(<RecordingSessions sessions={TWO} activeId="s1" onSelect={vi.fn()} />);
    expect(screen.getByText(/recording sessions/i)).toBeTruthy();
    expect(screen.getByRole("button", { name: /recording session 1/i })).toBeTruthy();
    expect(screen.getByRole("button", { name: /recording session 2/i })).toBeTruthy();
  });

  it("marks the active pill via aria-pressed", () => {
    render(<RecordingSessions sessions={TWO} activeId="s2" onSelect={vi.fn()} />);
    const p1 = screen.getByRole("button", { name: /recording session 1/i });
    const p2 = screen.getByRole("button", { name: /recording session 2/i });
    expect(p1.getAttribute("aria-pressed")).toBe("false");
    expect(p2.getAttribute("aria-pressed")).toBe("true");
  });

  it("calls onSelect with the session id when a pill is clicked", async () => {
    const user = userEvent.setup();
    const onSelect = vi.fn();
    render(<RecordingSessions sessions={TWO} activeId="s1" onSelect={onSelect} />);
    await user.click(screen.getByRole("button", { name: /recording session 2/i }));
    expect(onSelect).toHaveBeenCalledWith("s2");
  });

  it("is read-only: exposes no delete affordance (v1)", () => {
    render(<RecordingSessions sessions={TWO} activeId="s1" onSelect={vi.fn()} />);
    expect(screen.queryByRole("button", { name: /delete/i })).toBeNull();
  });
});
