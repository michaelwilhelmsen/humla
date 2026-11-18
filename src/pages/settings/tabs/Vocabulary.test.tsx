import { describe, it, expect, vi } from "vitest";
import { render, screen, within } from "@testing-library/react";
import { mockTauri } from "../../../test/tauri";
import userEvent from "@testing-library/user-event";
import { VocabularySection, VOCABULARY_LIMIT, parseVocabulary } from "./Vocabulary";
import { DEFAULTS, type EditableKey } from "../types";
import { DISCONNECTED, useCloudStore } from "../../../lib/cloud";

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

describe("VocabularySection for an organisation admin", () => {
  const status = {
    policy: { org_name: null, reason: null, local_only: false, retention_days: 0, telemetry_off: false, audio_forbidden: false, rules: [] },
    orgs: [{ id: "o1", name: "Northwind", reason: "", vocabulary: ["Northwind", "Fjord"], workspaces: ["w1"] }],
  };

  function signedIn(twoFactor = false) {
    useCloudStore.setState({
      status: {
        ...DISCONNECTED,
        configured: true,
        logged_in: true,
        user: { id: "u1", email: "a@example.no", name: "A", verified: true },
        two_factor: { available: true, enabled: twoFactor, recovery_codes_left: 0, required_by: [] },
      },
      ready: true,
    });
  }

  it("edits the organisation's terms on the server", async () => {
    signedIn();
    const edited = vi.fn(() => null);
    mockTauri({
      org_status: () => status,
      org_admin_orgs: () => [{ id: "o1", name: "Northwind", require_2fa: false }],
      org_vocabulary_edit: edited,
    });
    const update = renderWith("Tauri");
    const list = await screen.findByRole("list", { name: "Northwind terms" });
    expect(within(list).getAllByRole("listitem").map((li) => li.textContent)).toEqual(["Northwind", "Fjord"]);
    expect(screen.queryByRole("list", { name: "Organisation terms" })).toBeNull();

    await userEvent.click(screen.getByRole("button", { name: "Remove Fjord from Northwind" }));
    expect(edited).toHaveBeenCalledWith({ orgId: "o1", add: [], remove: ["Fjord"] });

    await userEvent.type(screen.getByLabelText("Add a term for Northwind"), "IPLOS, AI{Enter}");
    expect(edited).toHaveBeenCalledWith({ orgId: "o1", add: ["IPLOS"], remove: [] });
    expect(screen.getByLabelText("Add a term for Northwind")).toHaveValue("");
    expect(update).not.toHaveBeenCalled();
  });

  it("shows a refused edit and keeps what was typed", async () => {
    signedIn();
    mockTauri({
      org_status: () => status,
      org_admin_orgs: () => [{ id: "o1", name: "Northwind", require_2fa: false }],
      org_vocabulary_edit: () => {
        throw "Only Northwind's admins can change this.";
      },
    });
    renderWith("");
    await userEvent.type(await screen.findByLabelText("Add a term for Northwind"), "IPLOS{Enter}");
    expect(await screen.findByRole("alert")).toHaveTextContent("Only Northwind's admins can change this.");
    expect(screen.getByLabelText("Add a term for Northwind")).toHaveValue("IPLOS");
  });

  it("keeps a member's terms locked", async () => {
    signedIn();
    mockTauri({ org_status: () => status, org_admin_orgs: () => [] });
    renderWith("");
    expect(await screen.findByRole("list", { name: "Organisation terms" })).toBeInTheDocument();
    expect(screen.queryByLabelText("Add a term for Northwind")).toBeNull();
  });

  it("asks for two-factor authentication where the organisation requires it", async () => {
    signedIn(false);
    mockTauri({
      org_status: () => status,
      org_admin_orgs: () => [{ id: "o1", name: "Northwind", require_2fa: true }],
    });
    renderWith("");
    expect(
      await screen.findByText(/Northwind requires two-factor authentication to edit its terms/),
    ).toBeInTheDocument();
    expect(screen.queryByLabelText("Add a term for Northwind")).toBeNull();
    expect(screen.getByRole("list", { name: "Organisation terms" })).toBeInTheDocument();
  });
});
