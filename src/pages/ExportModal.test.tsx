import { describe, it, expect, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { mockTauri } from "../test/tauri";
import { ExportModal } from "./ExportModal";
import type { ExportSpec, Note } from "../lib/ipc";

function makeNote(over: Partial<Note> = {}): Note {
  return {
    id: "n1",
    title: "Weekly Sync",
    body: "<p>Some typed notes</p>",
    transcript: "You: hello\nSpeaker 1: hi there",
    summary: "- Ship v1",
    audio_path: null,
    summary_preset: "meeting",
    folder_id: null,
    language: "en",
    summary_provider: "",
    expected_speakers: null,
    detected_language: null,
    created_at: 0,
    updated_at: 0,
    owner: "",
    workspace_id: "",
    ...over,
  };
}

// Wires the native save panel to a fixed path and records the export invoke.
function setup(over: Partial<Note> = {}, savePath: string | null = "/Users/me/weekly-sync.md") {
  const exportSpy = vi.fn();
  mockTauri({
    "plugin:dialog|save": () => savePath,
    export_note: (args) => {
      exportSpy(args);
      return undefined;
    },
  });
  const onClose = vi.fn();
  render(<ExportModal note={makeNote(over)} open onClose={onClose} />);
  return { exportSpy, onClose };
}

describe("ExportModal", () => {
  it("defaults to Summary + Transcript checked, Notes off", () => {
    setup();
    const summary = screen.getByRole("checkbox", { name: "Summary" });
    const transcript = screen.getByRole("checkbox", { name: "Transcript" });
    const notes = screen.getByRole("checkbox", { name: "Notes" });
    expect(summary).toBeChecked();
    expect(transcript).toBeChecked();
    expect(notes).not.toBeChecked();
  });

  it("exports with the chosen path and default spec", async () => {
    const { exportSpy, onClose } = setup();
    await userEvent.click(screen.getByRole("button", { name: "Export…" }));
    await waitFor(() => expect(exportSpy).toHaveBeenCalledTimes(1));
    expect(exportSpy).toHaveBeenCalledWith({
      noteId: "n1",
      spec: {
        path: "/Users/me/weekly-sync.md",
        format: "markdown",
        includeSummary: true,
        includeTranscript: true,
        includeNotes: false,
        includeSpeakerLabels: true,
      },
    });
    await waitFor(() => expect(onClose).toHaveBeenCalled());
  });

  it("carries the speaker-labels toggle and format into the spec", async () => {
    const { exportSpy } = setup();
    await userEvent.click(
      screen.getByRole("switch", { name: "Include speaker labels in transcript" }),
    );
    await userEvent.click(screen.getByRole("radio", { name: "Plain text" }));
    await userEvent.click(screen.getByRole("button", { name: "Export…" }));
    await waitFor(() => expect(exportSpy).toHaveBeenCalledTimes(1));
    const spec = (exportSpy.mock.calls[0]![0] as { spec: ExportSpec }).spec;
    expect(spec.includeSpeakerLabels).toBe(false);
    expect(spec.format).toBe("txt");
  });

  it("selecting Notes includes it in the spec", async () => {
    const { exportSpy } = setup();
    await userEvent.click(screen.getByRole("checkbox", { name: "Notes" }));
    await userEvent.click(screen.getByRole("button", { name: "Export…" }));
    await waitFor(() => expect(exportSpy).toHaveBeenCalledTimes(1));
    expect((exportSpy.mock.calls[0]![0] as { spec: ExportSpec }).spec.includeNotes).toBe(true);
  });

  it("does not export when the save panel is cancelled", async () => {
    const { exportSpy, onClose } = setup({}, null);
    await userEvent.click(screen.getByRole("button", { name: "Export…" }));
    // Give the async handler a chance to run.
    await new Promise((r) => setTimeout(r, 0));
    expect(exportSpy).not.toHaveBeenCalled();
    expect(onClose).not.toHaveBeenCalled();
  });

  it("disables empty content and the export button when nothing is exportable", () => {
    setup({ summary: "", transcript: "", body: "<p></p>" });
    expect(screen.getByRole("checkbox", { name: /Summary/ })).toBeDisabled();
    expect(screen.getByRole("checkbox", { name: /Transcript/ })).toBeDisabled();
    expect(screen.getByRole("checkbox", { name: /Notes/ })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Export…" })).toBeDisabled();
  });
});
