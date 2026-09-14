import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { renderApp } from "../test/app";
import { makeNote } from "../test/fixtures";
import { mockLayoutBox } from "../test/layout";

// #191 — a note in a workspace says whether it is shared or private, and the
// chip is the place the answer changes. The consequence has to be ON the choice:
// private is not a hide, it deletes the copy every teammate already synced.

beforeAll(() => mockLayoutBox());
beforeEach(() => localStorage.clear());

const ws = (id: string) => ({ id, name: "Screenpartner", role: "owner", plan_status: "active" });

const cloud = (workspace: string | null) => ({
  configured: true,
  logged_in: true,
  base_url: "https://sync.humla.team",
  user: { id: "u1", email: "m@example.no", name: "Michael", verified: true },
  current_workspace: workspace ? ws(workspace) : null,
  workspaces: workspace ? [ws(workspace)] : [],
  billing_enabled: true,
  seat_price_cents: 500,
  seat_currency: "usd",
});

function openNote(note: ReturnType<typeof makeNote>, workspace: string | null, extra = {}) {
  renderApp("/note/n1", {
    notes_list: () => [note],
    notes_get: () => note,
    note_timeline: () => [],
    cloud_status: () => cloud(workspace),
    ...extra,
  });
}

describe("a note's visibility inside a workspace", () => {
  it("reads Shared, and offers Private with what it costs", async () => {
    openNote(makeNote({ id: "n1", title: "Weekly sync", workspace_id: "ws1" }), "ws1");

    await userEvent.click(await screen.findByRole("button", { name: "Visibility" }));
    expect(await screen.findByText("Private")).toBeInTheDocument();
    expect(screen.getByText(/removed from your teammates/i)).toBeInTheDocument();
  });

  it("flipping to private calls through with the note and the new value", async () => {
    const setPrivate = vi.fn(() => null);
    openNote(makeNote({ id: "n1", title: "Weekly sync", workspace_id: "ws1" }), "ws1", {
      notes_set_private: setPrivate,
    });

    await userEvent.click(await screen.findByRole("button", { name: "Visibility" }));
    await userEvent.click(await screen.findByText("Private"));

    expect(setPrivate).toHaveBeenCalledWith({ id: "n1", private: true });
    expect(await screen.findByRole("button", { name: "Visibility" })).toHaveTextContent("Private");
  });

  // A Personal note is private by definition; a second word for it beside the
  // workspace chip would only invite the question of what the difference is.
  it("is absent on a Personal note", async () => {
    openNote(makeNote({ id: "n1", title: "Weekly sync" }), null);

    expect(await screen.findByText("Weekly sync")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Visibility" })).toBeNull();
  });
});
