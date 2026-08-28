import { describe, it, expect, vi, beforeEach } from "vitest";
import { useState } from "react";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { NewWorkspaceModal, stageFor } from "./NewWorkspaceModal";
import { DISCONNECTED, useCloudStore, type CloudStatus, type CloudWorkspace } from "../lib/cloud";
import { mockTauri } from "../test/tauri";

function ws(over: Partial<CloudWorkspace> = {}): CloudWorkspace {
  return { id: "w1", name: "Acme", role: "owner", plan_status: "none", ...over };
}

function status(over: Partial<CloudStatus> = {}): CloudStatus {
  return {
    ...DISCONNECTED,
    configured: true,
    logged_in: true,
    base_url: "https://sync.humla.team",
    user: { id: "u1", email: "m@example.no", name: "Michael", verified: true },
    billing_enabled: true,
    seat_price_cents: 500,
    seat_currency: "usd",
    ...over,
  };
}

// The store is the modal's only input, so tests seed it directly rather than
// booting the app. `server` is what the next cloud_status refresh will answer
// with — the modal re-reads it after creating, which is how a fresh workspace
// reaches the trial stage.
function open(initial: CloudStatus, handlers: Record<string, (a: unknown) => unknown> = {}) {
  let server = initial;
  mockTauri({
    cloud_status: () => server,
    ...handlers,
  });
  useCloudStore.setState({ status: initial, ready: true });
  const onClose = vi.fn();
  const view = render(<NewWorkspaceModal open onClose={onClose} />);
  return {
    ...view,
    onClose,
    /** Advance what the server reports, as a webhook or a teammate would. */
    setServer: (next: CloudStatus) => {
      server = next;
    },
  };
}

beforeEach(() => {
  useCloudStore.setState({ status: DISCONNECTED, ready: true });
});

describe("stageFor", () => {
  const base = { configured: true, logged_in: true, billing_enabled: true };

  it("walks the states in the order the server resolves them", () => {
    expect(stageFor({ ...base, configured: false }, null)).toBe("connect");
    expect(stageFor({ ...base, logged_in: false }, null)).toBe("auth");
    expect(stageFor(base, null)).toBe("name");
    expect(stageFor(base, ws())).toBe("trial");
    expect(stageFor(base, ws({ plan_status: "trialing" }))).toBe("invite");
    expect(stageFor(base, ws({ plan_status: "active" }))).toBe("invite");
  });

  it("keeps a past-due workspace on the billing stage", () => {
    // Read-only for everyone in it, so it is not "done" — but see the checkout
    // test below: it must be routed to the Portal, never a second Checkout.
    expect(stageFor(base, ws({ plan_status: "past_due" }))).toBe("trial");
  });

  it("skips billing entirely on a self-hosted server", () => {
    // Nothing bills, so a workspace works the moment it exists — waiting on a
    // plan that will never go live would strand the flow forever.
    expect(stageFor({ ...base, billing_enabled: false }, ws())).toBe("invite");
  });
});

describe("the create-workspace sheet", () => {
  it("sells Humla Cloud before asking for anything, when no server is set", async () => {
    open({ ...DISCONNECTED, configured: false });
    expect(await screen.findByRole("heading", { name: /create a team workspace/i })).toBeInTheDocument();
    // The complaint this replaces: a bare textbox that explained nothing.
    expect(screen.getByText(/per seat, per month/i)).toBeInTheDocument();
    expect(screen.getByText(/14-day free trial/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /use humla cloud/i })).toBeInTheDocument();
  });

  it("asks for an account first when signed out, instead of sending you to Settings", async () => {
    open(status({ logged_in: false, user: null }));
    expect(await screen.findByRole("heading", { name: /create your account/i })).toBeInTheDocument();
    expect(screen.getByLabelText(/email/i)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: /already have an account/i }));
    expect(screen.getByRole("heading", { name: /^sign in$/i })).toBeInTheDocument();
  });

  it("prices the workspace on the naming step, using the server's own seat price", async () => {
    open(status({ seat_price_cents: 700, seat_currency: "usd" }));
    expect(await screen.findByRole("heading", { name: /name your workspace/i })).toBeInTheDocument();
    // Not hardcoded $5: the server advertises the price, and a stale number in
    // the UI would be a promise the checkout doesn't keep.
    expect(screen.getByText("$7")).toBeInTheDocument();
  });

  it("does not create anything when the name field merely loses focus", async () => {
    const create = vi.fn();
    open(status(), { cloud_create_workspace: create });
    const input = await screen.findByLabelText(/workspace name/i);
    await userEvent.type(input, "Acme");
    await userEvent.tab();
    expect(create).not.toHaveBeenCalled();
  });

  it("carries a new workspace straight into its trial rather than leaving it read-only", async () => {
    const created = ws({ id: "w9", name: "Acme" });
    const view = open(status(), {
      cloud_create_workspace: () => created,
      cloud_billing_checkout: () => "https://checkout.stripe.test/x",
    });
    view.setServer(status({ current_workspace: created, workspaces: [created] }));

    await userEvent.type(await screen.findByLabelText(/workspace name/i), "Acme{Enter}");

    // The dead end in the bug report was landing here with only a pointer to
    // Settings; the sheet now offers the trial itself.
    expect(await screen.findByRole("heading", { name: /start your free trial/i })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /start free trial/i })).toBeInTheDocument();
  });

  it("tells checkout which surface it came from", async () => {
    // `source` rides onto the Stripe subscription so a trial start can be
    // attributed. The server allowlists it and DROPS anything unrecognised
    // rather than failing the checkout, so an absent value breaks nothing and
    // reports nothing — which is exactly how it went unsent for a month.
    const created = ws({ id: "w9", name: "Acme" });
    const checkouts: unknown[] = [];
    const view = open(status(), {
      cloud_create_workspace: () => created,
      cloud_billing_checkout: (args) => {
        checkouts.push(args);
        return "https://checkout.stripe.test/x";
      },
    });
    view.setServer(status({ current_workspace: created, workspaces: [created] }));

    await userEvent.type(await screen.findByLabelText(/workspace name/i), "Acme{Enter}");
    await userEvent.click(await screen.findByRole("button", { name: /start free trial/i }));

    await waitFor(() => expect(checkouts).toHaveLength(1));
    expect(checkouts[0]).toMatchObject({ workspaceId: "w9", source: "new_workspace" });
  });

  it("separates a workspace rescued from a note's banner from one born here", async () => {
    // Same sheet, different funnel: the banner opens it on a workspace that was
    // stranded read-only, which is a different thing to have measured than the
    // create flow.
    const stranded = ws({ plan_status: "none" });
    const checkouts: unknown[] = [];
    mockTauri({
      cloud_status: () => status({ current_workspace: stranded, workspaces: [stranded] }),
      cloud_billing_checkout: (args) => {
        checkouts.push(args);
        return "https://checkout.stripe.test/x";
      },
    });
    useCloudStore.setState({
      status: status({ current_workspace: stranded, workspaces: [stranded] }),
      ready: true,
    });
    render(<NewWorkspaceModal open onClose={() => {}} workspaceId="w1" source="note_banner" />);

    await userEvent.click(await screen.findByRole("button", { name: /start free trial/i }));

    await waitFor(() => expect(checkouts).toHaveLength(1));
    expect(checkouts[0]).toMatchObject({ workspaceId: "w1", source: "note_banner" });
  });

  it("starts on naming even for someone who already owns workspaces", async () => {
    // "Create team workspace" means a new one — the sheet must not resume the existing
    // workspace's flow just because one is selected.
    const existing = ws({ plan_status: "active" });
    open(status({ current_workspace: existing, workspaces: [existing] }));
    expect(await screen.findByLabelText(/workspace name/i)).toBeInTheDocument();
  });

  // Reach the invite stage the way a user does: create, then have the server
  // report the plan live (a landed checkout, or a self-hosted server).
  async function reachInvite(
    plan: CloudWorkspace["plan_status"],
    handlers: Record<string, (a: unknown) => unknown> = {},
  ) {
    const created = ws({ id: "w9", name: "Acme" });
    const view = open(status(), { cloud_create_workspace: () => created, ...handlers });
    view.setServer(
      status({
        current_workspace: { ...created, plan_status: plan },
        workspaces: [{ ...created, plan_status: plan }],
      }),
    );
    await userEvent.type(await screen.findByLabelText(/workspace name/i), "Acme{Enter}");
    return view;
  }

  it("offers the first invite once the plan is live", async () => {
    await reachInvite("trialing", { cloud_invite_member: () => "invited" });

    expect(await screen.findByRole("heading", { name: /invite your team/i })).toBeInTheDocument();
    expect(screen.getByText(/trial started/i)).toBeInTheDocument();

    await userEvent.type(screen.getByLabelText(/teammate/i), "ola@example.no");
    await userEvent.click(screen.getByRole("button", { name: /^invite$/i }));

    // "invited" means they have no account yet — nothing happens until they sign
    // up AND verify, so the sheet must not claim they were added.
    expect(await screen.findByText(/invited by email/i)).toBeInTheDocument();
    expect(screen.queryByText(/^added$/i)).toBeNull();
  });

  it("says 'added' for someone who already had an account", async () => {
    await reachInvite("active", { cloud_invite_member: () => "added" });
    await userEvent.type(await screen.findByLabelText(/teammate/i), "ola@example.no");
    await userEvent.click(screen.getByRole("button", { name: /^invite$/i }));
    expect(await screen.findByText(/^added$/i)).toBeInTheDocument();
  });

  it("sends a past-due workspace to the Portal, never a second Checkout", async () => {
    const checkout = vi.fn();
    const portal = vi.fn(() => "https://billing.stripe.test/x");
    const due = ws({ plan_status: "past_due" });
    render(
      <>
        {(() => {
          mockTauri({
            cloud_status: () => status({ current_workspace: due, workspaces: [due] }),
            cloud_billing_checkout: checkout,
            cloud_billing_portal: portal,
          });
          useCloudStore.setState({
            status: status({ current_workspace: due, workspaces: [due] }),
            ready: true,
          });
          return null;
        })()}
        <NewWorkspaceModal open onClose={() => {}} workspaceId="w1" />
      </>,
    );

    await userEvent.click(await screen.findByRole("button", { name: /fix payment/i }));
    // A fresh Checkout on a live-but-unpaid subscription creates a SECOND
    // subscription and bills the owner twice.
    await waitFor(() => expect(portal).toHaveBeenCalled());
    expect(checkout).not.toHaveBeenCalled();
  });

  it("does not claim it created a workspace it only opened", async () => {
    // The read-only banner enters here on a workspace that has existed for a
    // while; "created" would be a claim about something that didn't just happen.
    const due = ws({ plan_status: "none" });
    mockTauri({ cloud_status: () => status({ current_workspace: due, workspaces: [due] }) });
    useCloudStore.setState({
      status: status({ current_workspace: due, workspaces: [due] }),
      ready: true,
    });
    render(<NewWorkspaceModal open onClose={() => {}} workspaceId="w1" />);

    expect(await screen.findByText("Acme")).toBeInTheDocument();
    expect(screen.queryByText(/^created$/i)).toBeNull();
  });

  it("says 'created' on the path where it did create one", async () => {
    const created = ws({ id: "w9", name: "Acme" });
    const view = open(status(), { cloud_create_workspace: () => created });
    view.setServer(status({ current_workspace: created, workspaces: [created] }));
    await userEvent.type(await screen.findByLabelText(/workspace name/i), "Acme{Enter}");
    expect(await screen.findByText(/^created$/i)).toBeInTheDocument();
  });

  it("opens straight on billing for an existing workspace, skipping the name step", async () => {
    const due = ws({ plan_status: "none" });
    mockTauri({ cloud_status: () => status({ current_workspace: due, workspaces: [due] }) });
    useCloudStore.setState({
      status: status({ current_workspace: due, workspaces: [due] }),
      ready: true,
    });
    render(<NewWorkspaceModal open onClose={() => {}} workspaceId="w1" />);

    expect(await screen.findByRole("heading", { name: /start your free trial/i })).toBeInTheDocument();
    expect(screen.queryByLabelText(/workspace name/i)).toBeNull();
  });

  it("quotes no price on the sign-in step of a server that bills nothing", async () => {
    // Reached by configuring a self-hosted server and then signing out. The
    // stage used to assert "$5 per seat/mo after a 14-day free trial" flatly,
    // which is the app inventing a charge nobody will make.
    open(status({ logged_in: false, user: null, billing_enabled: false, seat_price_cents: null }));
    expect(await screen.findByText(/bills nothing/i)).toBeInTheDocument();
    expect(screen.queryByText(/per seat\/mo/i)).toBeNull();
    expect(screen.queryByText(/14-day/i)).toBeNull();
  });

  it("uses the server's seat price on the sign-in step, not a hardcoded one", async () => {
    open(status({ logged_in: false, user: null, seat_price_cents: 700 }));
    expect(await screen.findByText(/\$7 per seat\/mo/i)).toBeInTheDocument();
  });

  it("promises no invoice seat on a self-hosted workspace", async () => {
    const created = ws({ id: "w9", name: "Local" });
    const view = open(status({ billing_enabled: false, seat_price_cents: null }), {
      cloud_create_workspace: () => created,
    });
    view.setServer(
      status({ billing_enabled: false, current_workspace: created, workspaces: [created] }),
    );
    await userEvent.type(await screen.findByLabelText(/workspace name/i), "Local{Enter}");
    expect(await screen.findByRole("heading", { name: /invite your team/i })).toBeInTheDocument();
    expect(screen.queryByText(/next invoice/i)).toBeNull();
  });

  it("resumes an abandoned checkout instead of creating a second workspace", async () => {
    // The funnel's one hole: dismissing the sheet mid-checkout and clicking
    // "Create team workspace" again used to build another workspace while the
    // first sat unsubscribed and read-only.
    const created = ws({ id: "w9", name: "Acme" });
    let server = status();
    mockTauri({
      cloud_status: () => server,
      cloud_create_workspace: () => created,
      cloud_billing_checkout: () => "https://checkout.stripe.test/x",
    });
    useCloudStore.setState({ status: server, ready: true });

    function Harness() {
      const [open, setOpen] = useState(true);
      return (
        <>
          <button onClick={() => setOpen(true)}>reopen</button>
          <NewWorkspaceModal open={open} onClose={() => setOpen(false)} />
        </>
      );
    }
    render(<Harness />);

    server = status({ current_workspace: created, workspaces: [created] });
    await userEvent.type(await screen.findByLabelText(/workspace name/i), "Acme{Enter}");
    await screen.findByRole("heading", { name: /start your free trial/i });

    // Walk away mid-checkout, then come back in.
    await userEvent.click(screen.getByRole("button", { name: /^later$/i }));
    await userEvent.click(screen.getByRole("button", { name: /reopen/i }));

    expect(await screen.findByRole("heading", { name: /start your free trial/i })).toBeInTheDocument();
    expect(screen.queryByLabelText(/workspace name/i)).toBeNull();

    // ...and it is not a trap: someone who really did want a second one can say so.
    await userEvent.click(screen.getByRole("button", { name: /create a different one/i }));
    expect(await screen.findByLabelText(/workspace name/i)).toBeInTheDocument();
  });

  it("starts fresh once the resumed workspace's plan went live", async () => {
    const created = ws({ id: "w9", name: "Acme" });
    let server = status();
    mockTauri({ cloud_status: () => server, cloud_create_workspace: () => created });
    useCloudStore.setState({ status: server, ready: true });

    function Harness() {
      const [open, setOpen] = useState(true);
      return (
        <>
          <button onClick={() => setOpen(true)}>reopen</button>
          <NewWorkspaceModal open={open} onClose={() => setOpen(false)} />
        </>
      );
    }
    render(<Harness />);

    server = status({ current_workspace: created, workspaces: [created] });
    await userEvent.type(await screen.findByLabelText(/workspace name/i), "Acme{Enter}");
    await screen.findByRole("heading", { name: /invite your team/i }).catch(() => null);
    await userEvent.click(screen.getByRole("button", { name: /^later$/i }));

    // Plan landed while the sheet was shut — there is nothing left to resume.
    const live = { ...created, plan_status: "trialing" as const };
    server = status({ current_workspace: live, workspaces: [live] });
    useCloudStore.setState({ status: server, ready: true });
    await userEvent.click(screen.getByRole("button", { name: /reopen/i }));
    expect(await screen.findByLabelText(/workspace name/i)).toBeInTheDocument();
  });

  it("goes straight to inviting on a self-hosted server, where nothing bills", async () => {
    const created = ws({ id: "w9", name: "Local" });
    const view = open(status({ billing_enabled: false, seat_price_cents: null }), {
      cloud_create_workspace: () => created,
    });
    view.setServer(
      status({ billing_enabled: false, current_workspace: created, workspaces: [created] }),
    );

    // No price on the naming step either — there is nothing to charge.
    expect(await screen.findByLabelText(/workspace name/i)).toBeInTheDocument();
    expect(screen.queryByText(/per seat, per month/i)).toBeNull();

    await userEvent.type(screen.getByLabelText(/workspace name/i), "Local{Enter}");
    expect(await screen.findByRole("heading", { name: /invite your team/i })).toBeInTheDocument();
  });
});
