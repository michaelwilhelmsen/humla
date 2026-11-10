import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { renderApp } from "../test/app";
import { makeNote } from "../test/fixtures";
import { mockLayoutBox } from "../test/layout";

// #191, #232 — a note in a workspace says who can read it, and the chip is the
// place that changes: everyone, its author alone, or its author plus chosen
// members. Only the author gets the controls; a member it is shared with sees
// the list.

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

const roster = () => [
  { id: "u1", email: "m@example.no", name: "Michael", role: "owner" },
  { id: "u-stian", email: "stian@example.no", name: "Stian Berg", role: "member" },
  { id: "u-petter", email: "petter@example.no", name: "Petter", role: "member" },
];

function openNote(note: ReturnType<typeof makeNote>, workspace: string | null, extra = {}) {
  renderApp("/note/n1", {
    notes_list: () => [note],
    notes_get: () => note,
    note_timeline: () => [],
    cloud_status: () => cloud(workspace),
    cloud_workspace_members: roster,
    ...extra,
  });
}

const chip = () => screen.findByRole("button", { name: "Who can read this note" });

describe("who can read a note inside a workspace", () => {
  it("names the workspace when everyone can, and says so in the popover", async () => {
    openNote(makeNote({ id: "n1", title: "Weekly sync", workspace_id: "ws1" }), "ws1");

    expect(await chip()).toHaveTextContent("Screenpartner");
    await userEvent.click(await chip());
    expect(await screen.findByRole("radio", { name: /Everyone in Screenpartner/ })).toHaveAttribute(
      "aria-checked",
      "true",
    );
  });

  it("restricting to people added calls through, and reads Only you", async () => {
    const setAccess = vi.fn(() => null);
    openNote(makeNote({ id: "n1", title: "Weekly sync", workspace_id: "ws1" }), "ws1", {
      notes_set_access: setAccess,
    });

    await userEvent.click(await chip());
    await userEvent.click(await screen.findByRole("radio", { name: /Only people added/ }));

    expect(setAccess).toHaveBeenCalledWith({ id: "n1", private: true, sharedWith: [] });
    expect(await chip()).toHaveTextContent("Only you");
  });

  it("adds a member found by name", async () => {
    const setAccess = vi.fn(() => null);
    openNote(makeNote({ id: "n1", title: "Board", workspace_id: "ws1", private: true }), "ws1", {
      notes_set_access: setAccess,
    });

    await userEvent.click(await chip());
    await userEvent.type(await screen.findByRole("combobox", { name: "Add people" }), "sti");
    const list = await screen.findByRole("listbox", { name: "Matching members" });
    await userEvent.click(within(list).getByRole("option", { name: "Stian Berg" }));

    expect(setAccess).toHaveBeenCalledWith({ id: "n1", private: true, sharedWith: ["u-stian"] });
    expect(await chip()).toHaveTextContent("Michael, Stian");
  });

  it("adds a member from the keyboard", async () => {
    const setAccess = vi.fn(() => null);
    openNote(makeNote({ id: "n1", title: "Board", workspace_id: "ws1", private: true }), "ws1", {
      notes_set_access: setAccess,
    });

    await userEvent.click(await chip());
    await userEvent.type(await screen.findByRole("combobox", { name: "Add people" }), "e");
    await userEvent.keyboard("{ArrowDown}{ArrowDown}{Enter}");

    expect(setAccess).toHaveBeenCalledWith({ id: "n1", private: true, sharedWith: ["u-stian"] });
  });

  it("removes a member", async () => {
    const setAccess = vi.fn(() => null);
    openNote(
      makeNote({ id: "n1", title: "Board", workspace_id: "ws1", private: true, shared_with: ["u-stian", "u-petter"] }),
      "ws1",
      { notes_set_access: setAccess },
    );

    expect(await chip()).toHaveTextContent("3 people");
    await userEvent.click(await chip());
    await userEvent.click(await screen.findByRole("button", { name: "Remove Stian Berg" }));

    expect(setAccess).toHaveBeenCalledWith({ id: "n1", private: true, sharedWith: ["u-petter"] });
  });

  // Not the reader's call on someone else's note — not even a workspace owner's.
  it("is absent on a teammate's note everyone can read", async () => {
    openNote(makeNote({ id: "n1", title: "Their sync", workspace_id: "ws1", owner: "u-stian" }), "ws1");

    expect(await screen.findByText("Their sync")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Who can read this note" })).toBeNull();
  });

  it("shows a member a note shared with them, without the controls", async () => {
    openNote(
      makeNote({ id: "n1", title: "Board", workspace_id: "ws1", owner: "u-stian", private: true, shared_with: ["u1"] }),
      "ws1",
    );

    expect(await chip()).toHaveTextContent("Stian, Michael");
    await userEvent.click(await chip());
    const people = await screen.findByRole("list", { name: "People with access" });
    expect(within(people).getByText(/Stian Berg/)).toBeInTheDocument();
    expect(within(people).getByText("(you)")).toBeInTheDocument();
    expect(screen.queryByRole("combobox", { name: "Add people" })).toBeNull();
    expect(screen.queryByRole("radio")).toBeNull();
    expect(screen.queryByRole("button", { name: /Remove/ })).toBeNull();
  });

  // A Personal note is private by definition.
  it("is absent on a Personal note", async () => {
    openNote(makeNote({ id: "n1", title: "Weekly sync" }), null);

    expect(await screen.findByText("Weekly sync")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Who can read this note" })).toBeNull();
  });
});
