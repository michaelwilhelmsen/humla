import { describe, it, expect, beforeEach } from "vitest";
import { render, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { emit } from "@tauri-apps/api/event";
import { useGlobalShortcuts } from "./shortcuts";
import { useNotesStore, useRecordingStore } from "./store";
import { mockTauri } from "../test/tauri";
import { makeNote } from "../test/fixtures";

// The window-visible half of the global record hotkey (#21). The backend hands
// the trigger over rather than deciding, so this is where "one key that does
// what the screen implies" actually happens.

function Harness({ path }: { path: string }) {
  useGlobalShortcuts();
  return <div>{path}</div>;
}

function mount(path: string, handlers: Record<string, (a: unknown) => unknown> = {}) {
  const calls: Record<string, unknown[]> = {};
  const record = (cmd: string) => (args: unknown) => {
    (calls[cmd] ??= []).push(args);
    return handlers[cmd]?.(args) ?? null;
  };
  mockTauri(
    {
      recording_start: record("recording_start"),
      recording_stop: record("recording_stop"),
      notes_create: (args) => {
        (calls.notes_create ??= []).push(args);
        return makeNote({ id: "fresh" });
      },
    },
    { events: true },
  );
  // The hook reads the live URL, not the router's — `window.location` is what
  // the real app's address bar carries.
  window.history.replaceState({}, "", path);
  render(
    <MemoryRouter initialEntries={[path]}>
      <Harness path={path} />
    </MemoryRouter>,
  );
  return calls;
}

beforeEach(() => {
  useRecordingStore.setState({ status: { noteId: null, phase: "idle" }, errors: [] });
  useNotesStore.setState({ notes: [makeNote({ id: "n1" })] });
});

describe("global record hotkey with the window on screen", () => {
  it("records the open note", async () => {
    const calls = mount("/note/n1");
    await emit("menubar://toggle-record");
    await waitFor(() => expect(calls.recording_start).toEqual([{ noteId: "n1" }]));
    expect(calls.notes_create).toBeUndefined();
  });

  it("makes a note first when none is open", async () => {
    const calls = mount("/");
    await emit("menubar://toggle-record");
    await waitFor(() => expect(calls.recording_start).toEqual([{ noteId: "fresh" }]));
    expect(calls.notes_create).toHaveLength(1);
  });

  it("stops a recording in flight", async () => {
    useRecordingStore.setState({ status: { noteId: "n1", phase: "recording" } });
    const calls = mount("/note/n1");
    await emit("menubar://toggle-record");
    await waitFor(() => expect(calls.recording_stop).toHaveLength(1));
    expect(calls.recording_start).toBeUndefined();
  });

  // A failed start has to surface: the hotkey may have been pressed from
  // another app, so there is no button that visibly did nothing.
  it("reports a failed start as an error", async () => {
    const calls = mount("/note/n1", {
      recording_start: () => {
        throw new Error("Microphone permission required");
      },
    });
    await emit("menubar://toggle-record");
    await waitFor(() => expect(calls.recording_start).toHaveLength(1));
    await waitFor(() =>
      expect(useRecordingStore.getState().errors.map((e) => e.message).join(" ")).toMatch(
        /Microphone permission required/,
      ),
    );
  });
});
