import { describe, it, expect, vi } from "vitest";
import { render, screen, within } from "@testing-library/react";
import { mockTauri } from "../../../test/tauri";
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

describe("VocabularySection with an organisation", () => {
  it("shows its terms as locked chips above the user's, with the marker once", async () => {
    mockTauri({
      org_status: () => ({
        policy: { org_name: null, reason: null, local_only: false, retention_days: 0, telemetry_off: false, audio_forbidden: false, rules: [] },
        orgs: [{ id: "o1", name: "Northwind", reason: "", vocabulary: ["Northwind", "Fjord"], workspaces: [] }],
      }),
    });
    renderWith("Tauri");
    const locked = await screen.findByRole("list", { name: "Organisation terms" });
    expect(within(locked).getAllByRole("listitem").map((li) => li.textContent)).toEqual(["Northwind", "Fjord"]);
    expect(within(locked).queryByRole("button")).toBeNull();
    expect(screen.getAllByRole("button", { name: /Managed by Northwind/ })).toHaveLength(1);
    const headings = screen.getAllByRole("heading", { level: 2 }).map((h) => h.textContent);
    expect(headings).toEqual(["From your organisation", "Your terms"]);
    expect(screen.getByRole("button", { name: "Remove Tauri" })).toBeInTheDocument();
  });

  it("shows nothing extra without one", async () => {
    mockTauri({
      org_status: () => ({
        policy: { org_name: null, reason: null, local_only: false, retention_days: 0, telemetry_off: false, audio_forbidden: false, rules: [] },
        orgs: [],
      }),
    });
    renderWith("Tauri");
    await new Promise((r) => setTimeout(r, 0));
    expect(screen.queryByRole("list", { name: "Organisation terms" })).toBeNull();
    expect(screen.getAllByRole("heading", { level: 2 }).map((h) => h.textContent)).toEqual(["Your terms"]);
  });
});
