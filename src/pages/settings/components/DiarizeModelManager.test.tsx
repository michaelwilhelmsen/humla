import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { DiarizeModelManager } from "./DiarizeModelManager";
import { EMPTY_DIARIZE_STATE } from "../types";
import type { DiarizeModelStatus } from "../../../lib/ipc";

function renderWith(status: Partial<DiarizeModelStatus>) {
  const onDownload = vi.fn();
  render(
    <DiarizeModelManager
      state={{
        ...EMPTY_DIARIZE_STATE,
        status: {
          downloaded: false,
          needsWarmUp: false,
          warmUpError: null,
          sizeBytes: 1,
          path: "/m",
          ...status,
        },
      }}
      cost="The model is 193 MB."
      onDownload={onDownload}
      onDelete={() => {}}
    />,
  );
  return { onDownload };
}

describe("DiarizeModelManager", () => {
  it("offers a download only when the files are missing", () => {
    renderWith({ path: null, sizeBytes: null });
    expect(screen.getByText(/Not downloaded\. The model is 193 MB\./)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Download model" })).toBeInTheDocument();
  });

  it("says a model awaiting its warm-up is being prepared, not missing", async () => {
    const { onDownload } = renderWith({ needsWarmUp: true });
    expect(screen.getByText(/being prepared for this version of macOS/)).toBeInTheDocument();
    expect(screen.queryByText(/Not downloaded/)).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Prepare now" }));
    expect(onDownload).toHaveBeenCalled();
  });

  it("names a failed warm-up and offers to try again", () => {
    renderWith({ needsWarmUp: true, warmUpError: "The model couldn’t be compiled" });
    expect(
      screen.getByText(
        "Couldn’t be prepared for this version of macOS: The model couldn’t be compiled",
      ),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Try again" })).toBeInTheDocument();
  });
});
