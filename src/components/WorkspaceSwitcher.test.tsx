import { describe, it, expect, beforeEach } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { WorkspaceSwitcher } from "./WorkspaceSwitcher";
import {
  DISCONNECTED,
  useCloudStore,
  type CloudStatus,
  type CloudWorkspace,
} from "../lib/cloud";
import { mockTauri } from "../test/tauri";

function seed(over: Partial<CloudStatus> = {}) {
  const status = { ...DISCONNECTED, ...over };
  mockTauri({ cloud_status: () => status });
  useCloudStore.setState({ status, ready: true });
  return render(<WorkspaceSwitcher />);
}

beforeEach(() => {
  useCloudStore.setState({ status: DISCONNECTED, ready: true });
});

describe("the workspace switcher's create row", () => {
  it("offers 'Create team workspace' even when signed out", async () => {
    // It used to be swapped for a pointer at Settings, which meant the one
    // action the menu exists for was unavailable in the state most new users
    // are in.
    seed();
    await userEvent.click(screen.getByRole("button", { name: /personal/i }));
    expect(await screen.findByRole("menuitem", { name: /create team workspace/i })).toBeInTheDocument();
    expect(screen.queryByText(/sign in to sync/i)).toBeNull();
  });

  it("opens the create sheet instead of an inline text field", async () => {
    seed();
    await userEvent.click(screen.getByRole("button", { name: /personal/i }));
    await userEvent.click(await screen.findByRole("menuitem", { name: /create team workspace/i }));

    // The old row became an <input> that created a workspace on blur — no
    // pricing, no explanation, and committed by clicking away.
    expect(await screen.findByRole("dialog", { name: /create a team workspace/i })).toBeInTheDocument();
    expect(screen.queryByPlaceholderText(/^workspace name$/i)).toBeNull();
  });
});

describe("the ambient team hint", () => {
  const ACME: CloudWorkspace = { id: "w1", name: "Acme", role: "owner", plan_status: "trialing" };

  it("fills the switcher's empty pill slot on Personal", () => {
    // The gap this closes: someone who never opened Settings and never clicked
    // the switcher had nothing telling them team workspaces exist at all.
    seed();
    expect(screen.getByText("Add team")).toBeInTheDocument();
  });

  it("is replaced by the role pill once a workspace is active", () => {
    // Self-destructing is why there is no dismiss state to persist — having a
    // team is what removes the hint.
    seed({ logged_in: true, workspaces: [ACME], current_workspace: ACME });
    expect(screen.queryByText("Add team")).toBeNull();
    expect(screen.getByText("Owner")).toBeInTheDocument();
  });

  it("opens the create sheet without leaving the dropdown open behind it", async () => {
    seed();
    await userEvent.click(screen.getByText("Add team"));

    expect(await screen.findByRole("dialog", { name: /create a team workspace/i })).toBeInTheDocument();
    // Queried by TEXT, not by role: the open sheet aria-hides the rest of the
    // page, so a role query reports a leaked menu as absent either way.
    expect(screen.queryByText("Create team workspace")).toBeNull();
  });

  it("does not trip the dropdown open on its way to the sheet", () => {
    // The pill sits INSIDE the menu trigger, which opens on pointerdown — so a
    // click handler alone is too late and the menu flashes open underneath.
    // Fired as a bare pointerdown on purpose: a full click also opens the sheet,
    // which steals focus and makes Radix dismiss the menu on its own, hiding
    // whether the event was ever swallowed.
    seed();
    fireEvent.pointerDown(screen.getByText("Add team"), { button: 0 });
    expect(screen.queryByText("Create team workspace")).toBeNull();
  });

  it("tells checkout the trial came from the hint, not the menu row", async () => {
    // An ambient hint is worth keeping only if it converts, and it shares its
    // sheet with the deliberate "Create team workspace" row — so the two have
    // to be distinguishable once the subscription exists in Stripe.
    const checkouts: unknown[] = [];
    const created: CloudWorkspace = { id: "w9", name: "Acme", role: "owner", plan_status: "none" };
    const live = {
      ...DISCONNECTED,
      configured: true,
      logged_in: true,
      billing_enabled: true,
      user: { id: "u1", email: "m@example.no", name: "Michael", verified: true },
    };
    mockTauri({
      cloud_status: () => ({ ...live, current_workspace: created, workspaces: [created] }),
      cloud_create_workspace: () => created,
      cloud_billing_checkout: (args) => {
        checkouts.push(args);
        return "https://checkout.stripe.test/x";
      },
    });
    useCloudStore.setState({ status: live, ready: true });
    render(<WorkspaceSwitcher />);

    await userEvent.click(screen.getByText("Add team"));
    await userEvent.type(await screen.findByLabelText(/workspace name/i), "Acme{Enter}");
    await userEvent.click(await screen.findByRole("button", { name: /start free trial/i }));

    await waitFor(() => expect(checkouts).toHaveLength(1));
    expect(checkouts[0]).toMatchObject({ workspaceId: "w9", source: "team_hint_switcher" });
  });
});
