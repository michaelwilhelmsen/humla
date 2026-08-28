import { beforeAll, beforeEach, describe, expect, it } from "vitest";
import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { renderApp } from "../test/app";
import { makeNote } from "../test/fixtures";
import { mockLayoutBox } from "../test/layout";

// The export sheet's team hint, wired end to end. The unit test next to the
// modal proves the CTA fires its callback; this proves the callback lands on a
// sheet that actually opens — the hint's whole point is that it does not hand
// someone off to Settings to go looking.

beforeAll(() => mockLayoutBox());
beforeEach(() => localStorage.clear());

function openPersonalNote() {
  const note = makeNote({ id: "n1", title: "Weekly sync" });
  renderApp("/note/n1", {
    notes_list: () => [note],
    notes_get: () => note,
    note_timeline: () => [],
    // Signed in, but on Personal — the only state the hint appears in.
    cloud_status: () => ({
      configured: true,
      logged_in: true,
      base_url: "https://sync.humla.team",
      user: { id: "u1", email: "m@example.no", name: "Michael", verified: true },
      current_workspace: null,
      workspaces: [],
      billing_enabled: true,
      seat_price_cents: 500,
      seat_currency: "usd",
    }),
  });
}

async function openExport() {
  await userEvent.click(await screen.findByRole("button", { name: /^more$/i }));
  await userEvent.click(await screen.findByText("Export…"));
}

describe("the export sheet's team hint", () => {
  it("opens the create sheet rather than sending the user to Settings", async () => {
    openPersonalNote();
    await openExport();

    await userEvent.click(await screen.findByRole("button", { name: "Create one" }));

    // The sheet lives in the toolbar, not inside the export modal that closed on
    // the way here — rendered in there it would have unmounted with it.
    expect(
      await screen.findByRole("dialog", { name: /create a team workspace/i }),
    ).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Export…" })).toBeNull();
  });

  it("stays out of the way once dismissed", async () => {
    openPersonalNote();
    await openExport();
    await userEvent.click(
      await screen.findByRole("button", { name: /dismiss team workspace hint/i }),
    );

    expect(screen.queryByRole("button", { name: "Create one" })).toBeNull();
    // Still an export sheet, just without the pitch.
    expect(screen.getByRole("button", { name: "Export…" })).toBeInTheDocument();
  });
});
