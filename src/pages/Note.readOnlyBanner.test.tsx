import { beforeAll, describe, expect, it } from "vitest";
import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { renderApp } from "../test/app";
import { makeNote } from "../test/fixtures";
import { mockLayoutBox } from "../test/layout";
import type { CloudRole, PlanStatus } from "../lib/cloud";

// The read-only banner on a workspace note used to name a Settings path and
// stop there — the dead end you reach by creating a workspace, since a new one
// is born unsubscribed. The owner can act from here now.

beforeAll(() => mockLayoutBox());

function open(role: CloudRole, plan: PlanStatus) {
  const note = makeNote({ id: "n1", title: "Weekly sync", workspace_id: "w1" });
  const ws = { id: "w1", name: "Acme", role, plan_status: plan };
  renderApp("/note/n1", {
    notes_list: () => [note],
    notes_get: () => note,
    note_timeline: () => [],
    cloud_status: () => ({
      configured: true,
      logged_in: true,
      base_url: "https://sync.humla.team",
      user: { id: "u1", email: "m@example.no", name: "Michael", verified: true },
      current_workspace: ws,
      workspaces: [ws],
      billing_enabled: true,
      seat_price_cents: 500,
      seat_currency: "usd",
    }),
    cloud_workspace_members: () => [
      { id: "u1", email: "m@example.no", name: "Michael", role: "owner" },
    ],
  });
}

describe("a note in a workspace whose plan isn't live", () => {
  it("lets the owner start the trial from the banner", async () => {
    open("owner", "none");
    const cta = await screen.findByRole("button", { name: /start free trial/i });
    await userEvent.click(cta);
    // The same sheet that creates workspaces, opened on this one — so the fix
    // is where the problem is, not two screens away.
    expect(await screen.findByRole("dialog", { name: /create a team workspace/i })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: /start your free trial/i })).toBeInTheDocument();
  });

  it("offers a past-due owner the payment fix, not a fresh subscription", async () => {
    open("owner", "past_due");
    expect(await screen.findByRole("button", { name: /fix payment/i })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /start free trial/i })).toBeNull();
  });

  it("names the failed payment for a past-due owner rather than a generic lock", async () => {
    open("owner", "past_due");
    expect(await screen.findByText(/payment for this workspace didn’t go through/i)).toBeInTheDocument();
  });

  it("asks a non-owner to talk to the owner instead of offering a control", async () => {
    open("member", "none");
    expect(await screen.findByText(/ask the workspace owner/i)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /start free trial/i })).toBeNull();
  });

  it("says nothing about billing to a viewer, whose lock is their role", async () => {
    open("viewer", "active");
    expect(await screen.findByText(/view-only/i)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /start free trial/i })).toBeNull();
  });
});
