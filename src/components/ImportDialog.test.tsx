import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import { ImportDialog } from "./ImportDialog";
import { mockTauri } from "../test/tauri";

describe("ImportDialog", () => {
  it("shows the trimmed language helper text (no wrong-language clause)", async () => {
    mockTauri({ settings_get: () => "en" });
    render(
      <ImportDialog
        path="/Users/me/meeting.m4a"
        onCancel={() => {}}
        onConfirm={vi.fn()}
      />,
    );

    expect(
      await screen.findByText(
        (text) =>
          text.startsWith("Pick the language spoken in this file.") &&
          text.trim().endsWith("afterward."),
      ),
    ).toBeInTheDocument();

    // The old trailing clause must be gone.
    expect(
      screen.queryByText(/avoids a wrong-language transcript/i),
    ).not.toBeInTheDocument();
  });
});
