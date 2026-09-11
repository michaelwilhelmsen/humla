import { describe, it, expect } from "vitest";
import { noteActivity, type ActivitySlices } from "./noteActivity";
import type { RecordingStatus } from "./ipc";

const IDLE: RecordingStatus = { noteId: null, phase: "idle" };

function slices(over: Partial<ActivitySlices> = {}): ActivitySlices {
  return { status: IDLE, transcribing: {}, diarizing: {}, summarizing: {}, ...over };
}

describe("noteActivity", () => {
  it("times the live capture's phases and nothing else", () => {
    const phase = (p: RecordingStatus["phase"]) =>
      noteActivity("n1", slices({ status: { noteId: "n1", phase: p } }))?.timed;
    expect(phase("recording")).toBe(true);
    expect(phase("paused")).toBe(true);
    expect(phase("stopping")).toBe(true);
    expect(phase("importing")).toBe(false);
    expect(noteActivity("n1", slices({ summarizing: { n1: true } }))?.timed).toBe(false);
  });

  it("is null when nothing is happening", () => {
    expect(noteActivity("n1", slices())).toBeNull();
  });

  it("is null for a note that isn't the one with activity", () => {
    expect(
      noteActivity("n2", slices({ status: { noteId: "n1", phase: "recording" } })),
    ).toBeNull();
  });

  it("ignores an idle capture parked on this note", () => {
    expect(noteActivity("n1", slices({ status: { noteId: "n1", phase: "idle" } }))).toBeNull();
  });

  it("names each live phase", () => {
    const phase = (p: RecordingStatus["phase"]) =>
      noteActivity("n1", slices({ status: { noteId: "n1", phase: p } }));
    expect(phase("starting")?.label).toBe("Starting…");
    expect(phase("recording")?.label).toBe("Recording");
    expect(phase("paused")?.label).toBe("Paused");
    expect(phase("stopping")?.label).toBe("Finishing transcript…");
    expect(phase("importing")?.label).toBe("Transcribing audio…");
  });

  it("draws the capture itself in the record colour and the rest interactive", () => {
    const phase = (p: RecordingStatus["phase"]) =>
      noteActivity("n1", slices({ status: { noteId: "n1", phase: p } }));
    expect(phase("recording")?.color).toBe("var(--color-record)");
    expect(phase("paused")?.color).toBe("var(--color-record)");
    expect(phase("stopping")?.color).toBe("var(--color-interactive)");
    expect(phase("importing")?.color).toBe("var(--color-interactive)");
  });

  it("freezes a paused capture and animates every other phase", () => {
    const phase = (p: RecordingStatus["phase"]) =>
      noteActivity("n1", slices({ status: { noteId: "n1", phase: p } }));
    expect(phase("paused")?.animated).toBe(false);
    expect(phase("recording")?.animated).toBe(true);
    expect(phase("stopping")?.animated).toBe(true);
  });

  it("follows the post-stop chain's named step", () => {
    expect(
      noteActivity(
        "n1",
        slices({ status: { noteId: "n1", phase: "diarizing", step: "writing_playback" } }),
      )?.label,
    ).toBe("Writing playback…");
  });

  it("falls back to the old copy on a step-less diarizing phase", () => {
    expect(
      noteActivity("n1", slices({ status: { noteId: "n1", phase: "diarizing" } }))?.label,
    ).toBe("Identifying speakers…");
  });

  it("names a replay's step, defaulting to transcribing", () => {
    expect(
      noteActivity("n1", slices({ transcribing: { n1: { startedAt: 1 } } }))?.label,
    ).toBe("Transcribing…");
    expect(
      noteActivity(
        "n1",
        slices({ transcribing: { n1: { startedAt: 1, step: "matching_speakers" } } }),
      )?.label,
    ).toBe("Matching speakers…");
  });

  it("reports a re-diarize and a summary", () => {
    expect(noteActivity("n1", slices({ diarizing: { n1: true } }))?.label).toBe(
      "Identifying speakers…",
    );
    expect(noteActivity("n1", slices({ summarizing: { n1: true } }))?.label).toBe("Summarizing…");
  });

  it("prefers a live capture, then a replay, then a diarize, then a summary", () => {
    const all = slices({
      status: { noteId: "n1", phase: "recording" },
      transcribing: { n1: { startedAt: 1 } },
      diarizing: { n1: true },
      summarizing: { n1: true },
    });
    expect(noteActivity("n1", all)?.label).toBe("Recording");
    expect(noteActivity("n1", { ...all, status: IDLE })?.label).toBe("Transcribing…");
    expect(
      noteActivity("n1", { ...all, status: IDLE, transcribing: {} })?.label,
    ).toBe("Identifying speakers…");
    expect(
      noteActivity("n1", { ...all, status: IDLE, transcribing: {}, diarizing: {} })?.label,
    ).toBe("Summarizing…");
  });
});
