import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { renderApp } from "../test/app";
import { makeNote } from "../test/fixtures";
import { mockLayoutBox } from "../test/layout";
import { useRecordingStore } from "../lib/store";

// Moving a note out of the active workspace takes it out of every list this
// view belongs to, so the view leaves with it and says where the note went.

beforeAll(() => mockLayoutBox());
beforeEach(() => {
  localStorage.clear();
  useRecordingStore.setState({ flashes: [] });
});

const ws = (id: string, name: string) => ({ id, name, role: "owner", plan_status: "active" });

const cloud = (active: string | null) => {
  const workspaces = [ws("ws1", "Screenpartner"), ws("ws2", "Acme")];
  return {
    configured: true,
    logged_in: true,
    base_url: "https://sync.humla.team",
    user: { id: "u1", email: "m@example.no", name: "Michael", verified: true },
    current_workspace: workspaces.find((w) => w.id === active) ?? null,
    workspaces,
    billing_enabled: true,
    seat_price_cents: 500,
    seat_currency: "usd",
  };
};

function openNote(
  note: ReturnType<typeof makeNote>,
  active: string | null,
  setWorkspace: (args: unknown) => unknown = vi.fn(() => null),
) {
  renderApp("/note/n1", {
    notes_list: () => [note],
    notes_get: () => note,
    note_timeline: () => [],
    cloud_status: () => cloud(active),
    notes_set_workspace: setWorkspace,
  });
  return setWorkspace;
}

describe("moving a note to another workspace", () => {
  it("leaves the note and flashes where it went", async () => {
    const setWorkspace = openNote(
      makeNote({ id: "n1", title: "Weekly sync", workspace_id: "ws1" }),
      "ws1",
    );

    await userEvent.click(await screen.findByRole("button", { name: "Workspace" }));
    await userEvent.click(await screen.findByRole("menuitemradio", { name: "Acme" }));

    expect(setWorkspace).toHaveBeenCalledWith({ id: "n1", workspaceId: "ws2", workspaceName: "Acme" });
    expect(await screen.findByText("Moved “Weekly sync” to Acme")).toBeInTheDocument();
    await waitFor(() => expect(screen.getByTestId("location")).toHaveTextContent("/all-notes"));
  });

  it("names Personal when the note leaves for this device", async () => {
    openNote(makeNote({ id: "n1", title: "Weekly sync", workspace_id: "ws1" }), "ws1");

    await userEvent.click(await screen.findByRole("button", { name: "Workspace" }));
    await userEvent.click(
      await screen.findByRole("menuitemradio", { name: "Personal (this device)" }),
    );

    expect(await screen.findByText("Moved “Weekly sync” to Personal")).toBeInTheDocument();
    await waitFor(() => expect(screen.getByTestId("location")).toHaveTextContent("/all-notes"));
  });

  // A deep-linked note from another workspace moved INTO the active one joins
  // the list on screen, so there is nothing to leave.
  it("stays when the note moves into the active workspace", async () => {
    openNote(makeNote({ id: "n1", title: "Weekly sync", workspace_id: "ws2" }), "ws1");

    await userEvent.click(await screen.findByRole("button", { name: "Workspace" }));
    await userEvent.click(await screen.findByRole("menuitemradio", { name: "Screenpartner" }));

    expect(await screen.findByRole("button", { name: "Workspace" })).toHaveTextContent(
      "Screenpartner",
    );
    expect(screen.getByTestId("location")).toHaveTextContent("/note/n1");
    expect(screen.queryByText(/^Moved /)).toBeNull();
  });
});

describe("a move the backend refuses", () => {
  it("stays on the note and says why", async () => {
    const refuse = vi.fn(() => {
      throw "Notes older than 30 days can't be moved to Acme.";
    });
    openNote(makeNote({ id: "n1", title: "Weekly sync", workspace_id: "ws1" }), "ws1", refuse);

    await userEvent.click(await screen.findByRole("button", { name: "Workspace" }));
    await userEvent.click(await screen.findByRole("menuitemradio", { name: "Acme" }));

    expect(await screen.findByText("Notes older than 30 days can't be moved to Acme.")).toBeInTheDocument();
    expect(screen.getByTestId("location")).toHaveTextContent("/note/n1");
  });
});
