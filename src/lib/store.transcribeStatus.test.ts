import { describe, it, expect } from "vitest";
import { emit } from "@tauri-apps/api/event";
import { bindBackendListeners, useRecordingStore } from "./store";
import { mockTauri } from "../test/tauri";

// #146: a replay's progress rides the per-note `transcribe_status`, so a run on
// one note must land on that note alone and never touch the live capture's
// slot or a diarize pass.
//
// One test, deliberately: the global setup's afterEach parks a fresh mockIPC,
// which drops the event registry `listen()` registered into — and
// `bindBackendListeners` binds once per module, so a second test would emit
// into nothing.

describe("transcribe_status progress", () => {
  it("measures one note's replay without touching the recording status", async () => {
    mockTauri({}, { events: true });
    bindBackendListeners();
    const recording = { noteId: "recording-note", phase: "recording" as const };
    useRecordingStore.setState({ status: recording, transcribing: {}, diarizing: {} });

    await emit("transcribe_status", { noteId: "replay-note", active: true });
    expect(Object.keys(useRecordingStore.getState().transcribing)).toEqual(["replay-note"]);

    await emit("transcribe_status", {
      noteId: "replay-note",
      active: true,
      step: "transcribing",
      doneMs: 45_000,
      totalMs: 180_000,
      take: 2,
      takes: 3,
    });
    const run = useRecordingStore.getState().transcribing["replay-note"];
    expect(run).toMatchObject({
      step: "transcribing",
      doneMs: 45_000,
      totalMs: 180_000,
      take: 2,
      takes: 3,
    });
    // Set on the bracket and kept across every measure — it is what picks one
    // run when two notes replay at once.
    expect(run.startedAt).toBeGreaterThan(0);

    expect(useRecordingStore.getState().status).toEqual(recording);
    expect(useRecordingStore.getState().diarizing).toEqual({});

    // The diarize half of the same take, named as one of the two passes a
    // hybrid take costs. Its own channel still, and the position it reports is
    // where the next take resumes.
    await emit("transcribe_status", {
      noteId: "replay-note",
      active: true,
      step: "diarizing",
      index: 1,
      count: 2,
      doneMs: 120_000,
      totalMs: 180_000,
      take: 2,
      takes: 3,
    });
    expect(useRecordingStore.getState().transcribing["replay-note"]).toMatchObject({
      step: "diarizing",
      index: 1,
      count: 2,
      doneMs: 120_000,
    });
    expect(useRecordingStore.getState().status).toEqual(recording);
    expect(useRecordingStore.getState().diarizing).toEqual({});

    // The run's last step, which is note-wide rather than per take.
    await emit("transcribe_status", {
      noteId: "replay-note",
      active: true,
      step: "matching_speakers",
      doneMs: 180_000,
      totalMs: 180_000,
      take: 3,
      takes: 3,
    });
    const unify = useRecordingStore.getState().transcribing["replay-note"];
    expect(unify).toMatchObject({ step: "matching_speakers", doneMs: 180_000 });
    // The previous step's counter does NOT survive into this one: a step that
    // counts nothing must not wear "1/2" borrowed from the one before it.
    expect(unify.index).toBeUndefined();
    expect(unify.count).toBeUndefined();
    // And the run is still the same run, so the pill doesn't change slots.
    expect(unify.startedAt).toBe(run.startedAt);
    expect(useRecordingStore.getState().status).toEqual(recording);

    await emit("transcribe_status", { noteId: "replay-note", active: false });
    expect(useRecordingStore.getState().transcribing).toEqual({});
    expect(useRecordingStore.getState().status).toEqual(recording);
  });
});
