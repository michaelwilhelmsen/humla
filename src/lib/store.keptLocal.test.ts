import { describe, expect, it } from "vitest";
import { act } from "@testing-library/react";
import { emit } from "@tauri-apps/api/event";
import { mockTauri } from "../test/tauri";
import { bindBackendListeners, keptLocalMessage, useRecordingStore } from "./store";

describe("transcription kept on this Mac", () => {
  it("says so once, in one line", async () => {
    mockTauri({}, { events: true });
    bindBackendListeners();
    await act(async () => {
      await emit("transcription_kept_local", { noteId: "n1", source: "Northwind" });
    });
    const flashes = useRecordingStore.getState().flashes.map((f) => f.message);
    expect(flashes).toEqual(["Transcription stays on this Mac for Northwind."]);
    expect(keptLocalMessage("")).toBe("Transcription stays on this Mac for your organisation.");
  });
});
