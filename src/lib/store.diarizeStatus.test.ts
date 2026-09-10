import { describe, it, expect } from "vitest";
import { emit } from "@tauri-apps/api/event";
import { bindBackendListeners, useRecordingStore } from "./store";
import { mockTauri } from "../test/tauri";

// #187: `recording_status` describes the live capture and nothing else. A
// re-diarize the user pressed on another note reports on `diarize_status`, so
// it can't blank the running recording's bar, timer, stop button or tray.
//
// One test, deliberately: the global setup's afterEach parks a fresh mockIPC,
// which drops the event registry `listen()` registered into — and
// `bindBackendListeners` binds once per module, so a second test would emit
// into nothing.

describe("diarize_status", () => {
  it("is per-note and never touches the recording status", async () => {
    mockTauri({}, { events: true });
    bindBackendListeners();
    const recording = { noteId: "recording-note", phase: "recording" as const };
    useRecordingStore.setState({ status: recording, diarizing: {} });

    await emit("diarize_status", { noteId: "other-note", active: true });
    expect(useRecordingStore.getState().diarizing).toEqual({ "other-note": true });
    expect(useRecordingStore.getState().status).toEqual(recording);

    // A second note's pass is independent of the first's.
    await emit("diarize_status", { noteId: "third-note", active: true });
    await emit("diarize_status", { noteId: "other-note", active: false });
    expect(useRecordingStore.getState().diarizing).toEqual({ "third-note": true });
    expect(useRecordingStore.getState().status).toEqual(recording);
  });
});
