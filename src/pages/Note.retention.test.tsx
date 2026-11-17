import { beforeAll, describe, expect, it, vi } from "vitest";
import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { emit } from "@tauri-apps/api/event";
import { MemoryRouter } from "react-router-dom";
import App from "../App";
import { LocationProbe } from "../test/app";
import { makeNote } from "../test/fixtures";
import { mockLayoutBox } from "../test/layout";
import { mockTauri } from "../test/tauri";
import { NO_POLICY } from "../lib/orgPolicy";
import type { Note } from "../lib/ipc";

beforeAll(() => mockLayoutBox());

function open(note: Note, retention: Record<string, number>, handlers: Record<string, (args: unknown) => unknown> = {}) {
  mockTauri(
    {
      notes_list: () => [note],
      notes_get: () => note,
      note_timeline: () => [],
      org_status: () => ({ policy: NO_POLICY, orgs: [], retention }),
      ...handlers,
    },
    { events: true },
  );
  render(
    <MemoryRouter initialEntries={[`/note/${note.id}`]}>
      <App />
      <LocationProbe />
    </MemoryRouter>,
  );
}

const titleBox = () => screen.getByPlaceholderText("New note") as HTMLTextAreaElement;

describe("a note under its organisation's retention", () => {
  it("says when it will be deleted", async () => {
    open(makeNote({ id: "n1", title: "Case", workspace_id: "w1" }), { w1: 30 });
    expect(await screen.findByText("Deleted after 30 days")).toBeInTheDocument();
  });

  it("says nothing in Personal or in a workspace outside the organisation", async () => {
    for (const workspace_id of ["", "w2"]) {
      open(makeNote({ id: "n1", title: "Case", workspace_id }), { w1: 30 });
      await waitFor(() => expect(titleBox()).toHaveValue("Case"));
      expect(screen.queryByText(/Deleted after/)).not.toBeInTheDocument();
      document.body.innerHTML = "";
    }
  });

  it("closes when purged, without saving what was typed", async () => {
    const update = vi.fn(() => null);
    open(makeNote({ id: "n1", title: "Case", workspace_id: "w1" }), { w1: 30 }, { notes_update: update });
    await waitFor(() => expect(titleBox()).toHaveValue("Case"));
    await userEvent.type(titleBox(), " notes");

    await act(async () => {
      await emit("notes_purged", ["n1"]);
    });

    await waitFor(() => expect(screen.getByTestId("location")).toHaveTextContent("/all-notes"));
    await new Promise((r) => setTimeout(r, 400));
    expect(update).not.toHaveBeenCalled();
  });

  it("stays open when another note is purged", async () => {
    open(makeNote({ id: "n1", title: "Case", workspace_id: "w1" }), { w1: 30 });
    await waitFor(() => expect(titleBox()).toHaveValue("Case"));
    await act(async () => {
      await emit("notes_purged", ["n2"]);
    });
    expect(screen.getByTestId("location")).toHaveTextContent("/note/n1");
  });
});
