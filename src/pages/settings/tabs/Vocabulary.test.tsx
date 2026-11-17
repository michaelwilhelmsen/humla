import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { VocabularySection, VOCABULARY_LIMIT, parseVocabulary } from "./Vocabulary";
import { DEFAULTS, type EditableKey } from "../types";

function renderWith(stored: string) {
  const update = vi.fn(async () => {});
  render(
    <VocabularySection
      s={{ ...DEFAULTS, custom_vocabulary: stored } as Record<EditableKey, string>}
      update={update}
    />,
  );
  return update;
}

describe("parseVocabulary", () => {
  it("reads the stored comma- or newline-separated list", () => {
    expect(parseVocabulary("Tauri, Humla\nScreenCaptureKit,\n\n humla ")).toEqual([
      "Tauri",
      "Humla",
      "ScreenCaptureKit",
    ]);
  });
});

describe("VocabularySection", () => {
  it("shows each stored term as a removable chip", async () => {
    const update = renderWith("Tauri, Humla");
    await userEvent.click(screen.getByRole("button", { name: "Remove Tauri" }));
    expect(update).toHaveBeenCalledWith("custom_vocabulary", "Humla");
  });

  it("adds a term on Enter, and splits a pasted list", async () => {
    const update = renderWith("Tauri");
    await userEvent.type(screen.getByLabelText("Add a term"), "Hege Nilsen, Rana{Enter}");
    expect(update).toHaveBeenCalledWith("custom_vocabulary", "Tauri, Hege Nilsen, Rana");
  });

  it("won't add a term too short for any provider to use", async () => {
    const update = renderWith("");
    await userEvent.type(screen.getByLabelText("Add a term"), "AI");
    expect(screen.getByRole("button", { name: "Add" })).toBeDisabled();
    await userEvent.keyboard("{Enter}");
    expect(update).not.toHaveBeenCalled();
  });

  it("states the limit and stops adding at it", () => {
    const full = Array.from({ length: VOCABULARY_LIMIT }, (_, i) => `Term${i}`).join(", ");
    renderWith(full);
    expect(screen.getByText(`Limit: ${VOCABULARY_LIMIT}`)).toBeInTheDocument();
    expect(screen.getByLabelText("Add a term")).toBeDisabled();
  });
});
