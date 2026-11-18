import { beforeAll, describe, expect, it } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import App from "../App";
import { makeNote } from "../test/fixtures";
import { mockLayoutBox } from "../test/layout";
import { mockTauri } from "../test/tauri";
import { NO_POLICY } from "../lib/orgPolicy";
import type { Note } from "../lib/ipc";

beforeAll(() => mockLayoutBox());

function open(note: Note) {
  mockTauri(
    {
      notes_list: () => [note],
      notes_get: () => note,
      note_timeline: () => [],
      org_status: () => ({ policy: NO_POLICY, orgs: [], governed: { w1: "Northwind" } }),
    },
    { events: true },
  );
  render(
    <MemoryRouter initialEntries={[`/note/${note.id}`]}>
      <App />
    </MemoryRouter>,
  );
}

const titleBox = () => screen.getByPlaceholderText("New note") as HTMLTextAreaElement;

describe("a note in a governed department", () => {
  it("says who manages it", async () => {
    open(makeNote({ id: "n1", title: "Case", workspace_id: "w1" }));
    expect(await screen.findByText("Managed by Northwind")).toBeInTheDocument();
  });

  it("says nothing in Personal or in a workspace outside the organisation", async () => {
    for (const workspace_id of ["", "w2"]) {
      open(makeNote({ id: "n1", title: "Case", workspace_id }));
      await waitFor(() => expect(titleBox()).toHaveValue("Case"));
      expect(screen.queryByText(/Managed by/)).not.toBeInTheDocument();
      document.body.innerHTML = "";
    }
  });
});
