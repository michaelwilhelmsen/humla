import { describe, it, expect, vi } from "vitest";
import { render, screen, act } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { LocalWhisperModelStatus } from "../../lib/ipc";
import { useDownloadStore } from "../../lib/store";
import { mockTauri } from "../../test/tauri";
import { ModelDownloadCard } from "./ModelDownloadCard";

function model(over: Partial<LocalWhisperModelStatus> = {}): LocalWhisperModelStatus {
  return {
    id: "large-v3-turbo-q5",
    label: "Large v3 Turbo (quantized)",
    description: "The recommended default for almost all use.",
    filename: "ggml-large-v3-turbo-q5_0.bin",
    sizeBytesHint: 547 * 1024 * 1024,
    kind: "multilingual",
    specificLanguage: null,
    downloaded: false,
    sizeBytes: null,
    path: null,
    ...over,
  };
}

function renderCard(
  m: LocalWhisperModelStatus,
  handlers: Parameters<typeof mockTauri>[0] = {},
  onChanged = vi.fn(),
) {
  mockTauri(handlers);
  useDownloadStore.getState().clear();
  render(<ModelDownloadCard model={m} onChanged={onChanged} />);
  return onChanged;
}

describe("ModelDownloadCard", () => {
  it("shows a downloaded model with its size and a Delete action", () => {
    renderCard(model({ downloaded: true, sizeBytes: 573_000_000 }));

    expect(screen.getByText("Large v3 Turbo (quantized)")).toBeInTheDocument();
    expect(screen.getByText(/downloaded/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /delete/i })).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /download/i }),
    ).not.toBeInTheDocument();
  });

  it("shows a missing model with its size hint and a Download action", () => {
    renderCard(model());

    expect(screen.getByText(/not downloaded/i)).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: /download/i }),
    ).toBeInTheDocument();
  });

  it("download shows event-driven progress and refetches on completion", async () => {
    const started: unknown[] = [];
    const onChanged = renderCard(model(), {
      local_whisper_download: (args) => {
        started.push(args);
        return null;
      },
    });

    await userEvent.click(screen.getByRole("button", { name: /download/i }));
    expect(started[0]).toMatchObject({ modelId: "large-v3-turbo-q5" });

    // Progress arrives via the global store (fed by the app's single
    // event listener), not the invoke promise.
    act(() => {
      useDownloadStore.getState().setProgress({
        modelId: "large-v3-turbo-q5",
        received: 273_500_000,
        total: 547_000_000,
      });
    });
    expect(await screen.findByText(/downloading — 50%/i)).toBeInTheDocument();
    expect(screen.getByRole("progressbar")).toHaveAttribute(
      "aria-valuenow",
      "50",
    );

    // Terminal event clears the slice → the card asks the parent to refetch
    // presence. This is what makes a download survive dialog close/reopen.
    expect(onChanged).not.toHaveBeenCalled();
    act(() => {
      useDownloadStore.getState().clear();
    });
    expect(onChanged).toHaveBeenCalledTimes(1);
  });

  it("any in-flight download disables other models' Download buttons", () => {
    renderCard(model({ id: "medium-q5", label: "Medium (quantized)" }));

    act(() => {
      useDownloadStore.getState().setProgress({
        modelId: "large-v3-turbo-q5",
        received: 1,
        total: 100,
      });
    });

    // Someone else is downloading — this card must not start a second one.
    expect(screen.getByRole("button", { name: /download/i })).toBeDisabled();
    expect(screen.queryByText(/downloading/i)).not.toBeInTheDocument();
  });
});
