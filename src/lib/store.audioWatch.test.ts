import { describe, it, expect } from "vitest";
import { useRecordingStore } from "./store";

// The capture's clock lives in the store because the two things that display it
// — the note view's bar and the compact pill in `Layout` — mount and unmount
// independently. What the phases do to it is therefore the whole contract.
describe("syncAudioWatch's clock", () => {
  function start(noteId = "n1") {
    useRecordingStore.setState({ status: { noteId: null, phase: "idle" } });
    useRecordingStore.getState().syncAudioWatch("recording", noteId);
    useRecordingStore.setState({ status: { noteId, phase: "recording" } });
  }

  it("banks and freezes the active time through the stop", () => {
    start();
    const { activeSince } = useRecordingStore.getState();
    expect(activeSince).not.toBeNull();
    useRecordingStore.getState().syncAudioWatch("stopping", "n1");
    const s = useRecordingStore.getState();
    expect(s.activeSince).toBeNull();
    expect(s.activeAccumMs).toBeGreaterThanOrEqual(0);
    expect(s.micHeard).toBe(false);
    expect(s.micLevel).toBe(0);
  });

  it("keeps the banked reading rather than re-banking on a repeated stop phase", () => {
    start();
    useRecordingStore.setState({ activeAccumMs: 65_000, activeSince: null });
    for (const phase of ["stopping", "diarizing"] as const) {
      useRecordingStore.getState().syncAudioWatch(phase, "n1");
      expect(useRecordingStore.getState().activeAccumMs).toBe(65_000);
    }
  });

  it("clears the accumulated time on idle", () => {
    start();
    useRecordingStore.setState({ activeAccumMs: 65_000, activeSince: null });
    useRecordingStore.getState().syncAudioWatch("idle", null);
    expect(useRecordingStore.getState().activeAccumMs).toBe(0);
    expect(useRecordingStore.getState().activeSince).toBeNull();
  });

  it("banks on pause and restarts on resume without losing the earlier segment", () => {
    start();
    useRecordingStore.setState({ activeSince: Date.now() - 30_000, activeAccumMs: 0 });
    useRecordingStore.getState().syncAudioWatch("paused", "n1");
    useRecordingStore.setState({ status: { noteId: "n1", phase: "paused" } });
    const banked = useRecordingStore.getState().activeAccumMs;
    expect(banked).toBeGreaterThanOrEqual(30_000);
    useRecordingStore.getState().syncAudioWatch("recording", "n1");
    expect(useRecordingStore.getState().activeAccumMs).toBe(banked);
    expect(useRecordingStore.getState().activeSince).not.toBeNull();
  });
});
