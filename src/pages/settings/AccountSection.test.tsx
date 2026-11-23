import { describe, it, expect } from "vitest";
import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { renderApp } from "../../test/app";

// Fixtures for the cloud_status IPC command — the section's five states all
// branch off this one payload.
function signedOutConfigured() {
  return {
    configured: true,
    logged_in: false,
    base_url: "https://sync.example.com",
    user: null,
    current_workspace: null,
    workspaces: [],
    billing_enabled: false,
  };
}

function signedIn(role: "owner" | "member", billing = true) {
  const ws = { id: "w1", name: "Acme", role, plan_status: "active" };
  return {
    configured: true,
    logged_in: true,
    base_url: "https://sync.humla.team",
    user: { id: "u1", email: "m@example.no", name: "Michael", verified: true },
    current_workspace: ws,
    workspaces: [ws],
    billing_enabled: billing,
  };
}

const MEMBERS = [
  { id: "u1", email: "m@example.no", name: "Michael", role: "owner" },
  { id: "u2", email: "ola@example.no", name: "Ola", role: "member" },
];

async function openAccount(status: unknown) {
  renderApp("/settings?tab=account", {
    cloud_status: () => status,
    cloud_workspace_members: () => MEMBERS,
  });
  return await screen.findByRole("dialog", { name: /settings/i });
}

describe("Account section", () => {
  it("signed out + unconfigured shows connect only — no stale Organization stub", async () => {
    renderApp("/settings?tab=account");
    const dialog = await screen.findByRole("dialog", { name: /settings/i });

    expect(
      await within(dialog).findByRole("heading", { name: /connect to sync/i }),
    ).toBeInTheDocument();
    // The old Organization tab's signed-out stub pointed at "the Account
    // tab" — redundant and stale now that sign-in lives in this section.
    expect(
      within(dialog).queryByText(/sign in from the/i),
    ).not.toBeInTheDocument();
  });

  it("pitches Humla Cloud with a pricing card whose CTA starts the connect flow", async () => {
    const configures: unknown[] = [];
    renderApp("/settings?tab=account", {
      cloud_configure: (args) => {
        configures.push(args);
        return null;
      },
    });
    const dialog = await screen.findByRole("dialog", { name: /settings/i });

    // The pitch: what it does, what it costs, and the trial.
    expect(
      await within(dialog).findByText(/share notes across your team/i),
    ).toBeInTheDocument();
    expect(within(dialog).getByText("$5")).toBeInTheDocument();
    expect(
      within(dialog).getByText(/per seat, per month/i),
    ).toBeInTheDocument();
    expect(
      within(dialog).getByText(/14-day free trial/i),
    ).toBeInTheDocument();

    await userEvent.click(
      within(dialog).getByRole("button", { name: /upgrade/i }),
    );

    // CTA points the app at the hosted server (sign-in/up renders next).
    expect(configures).toHaveLength(1);
    expect(String((configures[0] as { baseUrl?: string })?.baseUrl)).toMatch(
      /humla/i,
    );
  });

  it("configured + signed out shows the sign-in form with a signup mode", async () => {
    const dialog = await openAccount(signedOutConfigured());

    expect(
      await within(dialog).findByRole("heading", { name: /sign in/i }),
    ).toBeInTheDocument();
    expect(within(dialog).getByText("https://sync.example.com")).toBeInTheDocument();

    await userEvent.click(
      within(dialog).getByRole("button", { name: /need an account/i }),
    );
    expect(
      within(dialog).getByRole("heading", { name: /create account/i }),
    ).toBeInTheDocument();
    expect(within(dialog).getByPlaceholderText(/your name/i)).toBeInTheDocument();
    // Escape hatch for a mistyped server URL.
    expect(
      within(dialog).getByRole("button", { name: /change server/i }),
    ).toBeInTheDocument();
  });

  it("forgot password sends a reset email for the typed address", async () => {
    const resets: unknown[] = [];
    renderApp("/settings?tab=account", {
      cloud_status: () => signedOutConfigured(),
      cloud_request_password_reset: (args) => {
        resets.push(args);
        return null;
      },
    });
    const dialog = await screen.findByRole("dialog", { name: /settings/i });
    await within(dialog).findByRole("heading", { name: /sign in/i });

    const forgot = within(dialog).getByRole("button", {
      name: /forgot password/i,
    });

    // Without an email there's nothing to reset — the user gets a nudge,
    // not a silent no-op.
    await userEvent.click(forgot);
    expect(
      within(dialog).getByText(/enter your email/i),
    ).toBeInTheDocument();
    expect(resets).toHaveLength(0);

    await userEvent.type(
      within(dialog).getByPlaceholderText(/you@example.com/i),
      "m@example.no",
    );
    await userEvent.click(forgot);

    expect(resets).toHaveLength(1);
    expect(resets[0]).toMatchObject({ email: "m@example.no" });
    expect(
      await within(dialog).findByText(/reset email sent to m@example.no/i),
    ).toBeInTheDocument();
  });

  it("unverified account shows the warning with a working resend", async () => {
    const resends: unknown[] = [];
    const status = signedIn("owner");
    status.user.verified = false;
    renderApp("/settings?tab=account", {
      cloud_status: () => status,
      cloud_workspace_members: () => MEMBERS,
      cloud_resend_verification: () => {
        resends.push(true);
        return null;
      },
    });
    const dialog = await screen.findByRole("dialog", { name: /settings/i });

    expect(
      await within(dialog).findByText(/email not verified/i),
    ).toBeInTheDocument();
    await userEvent.click(
      within(dialog).getByRole("button", { name: /resend verification/i }),
    );

    expect(resends).toHaveLength(1);
    expect(
      await within(dialog).findByText(/verification email sent/i),
    ).toBeInTheDocument();
  });

  it("inviting a member sends the invite and confirms it", async () => {
    const invites: unknown[] = [];
    renderApp("/settings?tab=account", {
      cloud_status: () => signedIn("owner"),
      cloud_workspace_members: () => MEMBERS,
      cloud_invite_member: (args) => {
        invites.push(args);
        return "invited";
      },
    });
    const dialog = await screen.findByRole("dialog", { name: /settings/i });

    await userEvent.type(
      await within(dialog).findByPlaceholderText(/teammate@example.com/i),
      "kari@example.no",
    );
    await userEvent.click(
      within(dialog).getByRole("button", { name: /invite/i }),
    );

    expect(invites).toHaveLength(1);
    expect(invites[0]).toMatchObject({
      workspaceId: "w1",
      email: "kari@example.no",
    });
    // The notice must say an email went out — the server really sends one
    // (Usesend+SES), and the old copy left inviters thinking they had to
    // notify the teammate themselves.
    expect(
      await within(dialog).findByText(/invitation emailed to kari@example.no/i),
    ).toBeInTheDocument();
  });

  it("signed-in owner sees identity, workspace, billing, members, and delete", async () => {
    const dialog = await openAccount(signedIn("owner"));

    // Identity card + own row in the members roster both show the name and
    // email (roster load races the identity render, so never single-match).
    expect(
      (await within(dialog).findAllByText("Michael")).length,
    ).toBeGreaterThanOrEqual(1);
    expect(
      (await within(dialog).findAllByText("m@example.no")).length,
    ).toBeGreaterThanOrEqual(1);
    expect(
      within(dialog).getByRole("button", { name: /sign out/i }),
    ).toBeInTheDocument();

    // Workspace management (owner: rename input + save).
    expect(
      await within(dialog).findByDisplayValue("Acme"),
    ).toBeInTheDocument();
    // Billing surface (billing_enabled + active plan).
    expect(within(dialog).getByText(/^active$/i)).toBeInTheDocument();
    expect(
      within(dialog).getByRole("button", { name: /manage billing/i }),
    ).toBeInTheDocument();
    // Members roster with an invite path.
    expect(await within(dialog).findByText("Ola")).toBeInTheDocument();
    expect(
      within(dialog).getByRole("button", { name: /invite/i }),
    ).toBeInTheDocument();
    // Owner danger zone.
    expect(
      within(dialog).getByRole("button", { name: /delete workspace/i }),
    ).toBeInTheDocument();
    expect(
      within(dialog).queryByRole("button", { name: /leave workspace/i }),
    ).not.toBeInTheDocument();
  });

  it("owner on a past_due plan is offered Fix payment (portal), never checkout", async () => {
    const portals: unknown[] = [];
    const checkouts: unknown[] = [];
    const status = signedIn("owner");
    // Same ws object backs current_workspace and workspaces[0].
    status.current_workspace.plan_status = "past_due";
    renderApp("/settings?tab=account", {
      cloud_status: () => status,
      cloud_workspace_members: () => MEMBERS,
      cloud_billing_portal: (args) => {
        portals.push(args);
        return "https://billing.stripe.test/portal";
      },
      cloud_billing_checkout: (args) => {
        checkouts.push(args);
        return "https://checkout.stripe.test/session";
      },
    });
    const dialog = await screen.findByRole("dialog", { name: /settings/i });

    // The badge reads as a payment problem, not as "not subscribed".
    expect(
      await within(dialog).findByText(/payment past due/i),
    ).toBeInTheDocument();

    // The double-billing trap: a past_due workspace must NOT be offered a
    // fresh checkout (that would create a second subscription).
    expect(
      within(dialog).queryByRole("button", { name: /subscribe/i }),
    ).not.toBeInTheDocument();
    expect(
      within(dialog).queryByRole("button", { name: /free trial/i }),
    ).not.toBeInTheDocument();

    // Fix payment goes to the Customer Portal — the same call the "Manage
    // billing" button uses — and never touches checkout.
    await userEvent.click(
      within(dialog).getByRole("button", { name: /fix payment/i }),
    );
    expect(checkouts).toHaveLength(0);
    expect(portals).toHaveLength(1);
    expect(portals[0]).toMatchObject({ workspaceId: "w1" });
  });

  it("in Personal, an owner can manage a subscribed workspace's billing without opening it", async () => {
    // A trial user who went back to Personal has no open workspace, so the
    // workspace's own Billing panel never renders. They still have to be able
    // to cancel: this went unnoticed until a trial user couldn't find it.
    const portals: unknown[] = [];
    const checkouts: unknown[] = [];
    const status = signedIn("owner");
    status.current_workspace = null as never;
    status.workspaces = [
      { id: "w1", name: "Acme", role: "owner", plan_status: "trialing" },
      { id: "w2", name: "Unpaid", role: "owner", plan_status: "none" },
      { id: "w3", name: "Theirs", role: "member", plan_status: "active" },
    ];
    renderApp("/settings?tab=account", {
      cloud_status: () => status,
      cloud_billing_portal: (args) => {
        portals.push(args);
        return "https://billing.stripe.test/portal";
      },
      cloud_billing_checkout: (args) => {
        checkouts.push(args);
        return "https://checkout.stripe.test/session";
      },
    });
    const dialog = await screen.findByRole("dialog", { name: /settings/i });

    const manage = await within(dialog).findAllByRole("button", { name: /manage billing/i });
    // Only the owned workspace with a live subscription; not the unsubscribed
    // one (nothing to manage) nor one they don't own.
    expect(manage).toHaveLength(1);
    expect(manage[0]).toHaveAccessibleName(/acme/i);

    await userEvent.click(manage[0]);
    expect(portals).toEqual([expect.objectContaining({ workspaceId: "w1" })]);
    expect(checkouts).toHaveLength(0);
  });

  it("in Personal, a past_due owned workspace offers Fix payment through the portal", async () => {
    const status = signedIn("owner");
    status.current_workspace = null as never;
    status.workspaces = [{ id: "w1", name: "Acme", role: "owner", plan_status: "past_due" }];
    const dialog = await openAccount(status);

    expect(
      await within(dialog).findByRole("button", { name: /fix payment for acme/i }),
    ).toBeInTheDocument();
  });

  it("in Personal, a failed portal request shows its error under that workspace only", async () => {
    const status = signedIn("owner");
    status.current_workspace = null as never;
    status.workspaces = [
      { id: "w1", name: "Acme", role: "owner", plan_status: "trialing" },
      { id: "w2", name: "Beta", role: "owner", plan_status: "past_due" },
    ];
    renderApp("/settings?tab=account", {
      cloud_status: () => status,
      cloud_billing_portal: () => {
        throw new Error("portal down");
      },
    });
    const dialog = await screen.findByRole("dialog", { name: /settings/i });

    await userEvent.click(
      await within(dialog).findByRole("button", { name: /fix payment for beta/i }),
    );
    const errors = await within(dialog).findAllByText(/portal down/i);
    expect(errors).toHaveLength(1);
    // Rendered in Beta's row, next to its button.
    expect(errors[0].parentElement).toContainElement(
      within(dialog).getByRole("button", { name: /fix payment for beta/i }),
    );
  });

  it("in Personal, shows no billing section when the server doesn't bill", async () => {
    const status = signedIn("owner", false);
    status.current_workspace = null as never;
    const dialog = await openAccount(status);

    expect(await within(dialog).findByText(/your workspaces/i)).toBeInTheDocument();
    expect(
      within(dialog).queryByRole("button", { name: /manage billing/i }),
    ).not.toBeInTheDocument();
  });

  it("tags the checkout with the surface that started it", async () => {
    // The server records `source` on the Stripe subscription so a trial start
    // is attributable to the pane it came from. It ALLOWLISTS the value and
    // drops anything else without failing the checkout — so a missing or
    // misspelled source costs the datapoint and reports nothing. That is how
    // the field, shipped server-side on 2026-07-30, sat unsent: every checkout
    // still worked. This asserts the value leaves the app.
    const checkouts: unknown[] = [];
    const status = signedIn("owner");
    status.current_workspace.plan_status = "none";
    renderApp("/settings?tab=account", {
      cloud_status: () => status,
      cloud_workspace_members: () => MEMBERS,
      cloud_billing_checkout: (args) => {
        checkouts.push(args);
        return "https://checkout.stripe.test/session";
      },
    });
    const dialog = await screen.findByRole("dialog", { name: /settings/i });

    await userEvent.click(
      await within(dialog).findByRole("button", { name: /free trial/i }),
    );
    expect(checkouts).toHaveLength(1);
    expect(checkouts[0]).toMatchObject({
      workspaceId: "w1",
      source: "settings_organization",
    });
  });

  it("signed-in user can toggle workspace audio upload (default on)", async () => {
    const sets: unknown[] = [];
    renderApp("/settings?tab=account", {
      cloud_status: () => signedIn("owner"),
      cloud_workspace_members: () => MEMBERS,
      settings_set: (args) => {
        sets.push(args);
        return null;
      },
    });
    const dialog = await screen.findByRole("dialog", { name: /settings/i });

    const toggle = await within(dialog).findByRole("switch", {
      name: /upload recording audio/i,
    });
    // Default is on — the upload path only treats the literal "false" as off.
    expect(toggle).toHaveAttribute("aria-checked", "true");

    await userEvent.click(toggle);
    expect(sets).toContainEqual({ key: "sync_audio", value: "false" });
  });

  it("owner can change a member's role through the popover select", async () => {
    const roleChanges: unknown[] = [];
    renderApp("/settings?tab=account", {
      cloud_status: () => signedIn("owner"),
      cloud_workspace_members: () => MEMBERS,
      cloud_set_member_role: (args) => {
        roleChanges.push(args);
        return null;
      },
    });
    const dialog = await screen.findByRole("dialog", { name: /settings/i });

    // Ola's role select (owner rows render a static pill instead).
    const trigger = await within(dialog).findByRole("combobox", {
      name: /^member$/i,
    });
    await userEvent.click(trigger);
    await userEvent.click(
      within(dialog).getByRole("option", { name: /admin/i }),
    );

    expect(roleChanges).toHaveLength(1);
    expect(roleChanges[0]).toMatchObject({ userId: "u2", role: "admin" });
  });

  it("signed-in member gets a read-only workspace and leave instead of delete", async () => {
    const dialog = await openAccount(signedIn("member", false));

    // Name is plain text, not a rename input.
    expect(await within(dialog).findByText("Acme")).toBeInTheDocument();
    expect(
      within(dialog).queryByDisplayValue("Acme"),
    ).not.toBeInTheDocument();
    // No invite surface, no billing section (billing_enabled=false).
    expect(
      within(dialog).queryByRole("button", { name: /invite/i }),
    ).not.toBeInTheDocument();
    expect(
      within(dialog).queryByRole("button", { name: /manage billing/i }),
    ).not.toBeInTheDocument();
    // Member danger zone.
    expect(
      await within(dialog).findByRole("button", { name: /leave workspace/i }),
    ).toBeInTheDocument();
    expect(
      within(dialog).queryByRole("button", { name: /delete workspace/i }),
    ).not.toBeInTheDocument();
  });

  it("a managed server can't be changed from the sign-in form", async () => {
    const dialog = await openAccount({ ...signedOutConfigured(), server_managed: true });
    expect(await within(dialog).findByRole("button", { name: /^sign in$/i })).toBeInTheDocument();
    expect(within(dialog).getByText("https://sync.example.com")).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: /managed by this mac's configuration/i })).toBeInTheDocument();
    expect(within(dialog).queryByRole("button", { name: /change server/i })).not.toBeInTheDocument();
  });

  it("a managed server can't be disconnected once signed in", async () => {
    const dialog = await openAccount({ ...signedIn("member", false), server_managed: true });
    expect(await within(dialog).findByText("https://sync.humla.team")).toBeInTheDocument();
    expect(within(dialog).queryByRole("button", { name: /disconnect server/i })).not.toBeInTheDocument();
  });

  it("an unmanaged server can be changed", async () => {
    const dialog = await openAccount(signedOutConfigured());
    expect(await within(dialog).findByRole("button", { name: /change server/i })).toBeInTheDocument();
    expect(within(dialog).queryByRole("button", { name: /managed by/i })).not.toBeInTheDocument();
  });

  describe("Policies", () => {
    const NONE = {
      org_name: null,
      reason: null,
      local_only: false,
      retention_days: 0,
      telemetry_off: false,
      audio_forbidden: false,
      rules: [],
    };
    const northwind = {
      ...NONE,
      org_name: "Northwind",
      reason: "Nothing leaves this Mac.",
      local_only: true,
      retention_days: 30,
      rules: [
        { kind: "local_only", source: "Northwind", reason: "Nothing leaves this Mac." },
        { kind: "retention_days", source: "Northwind", reason: "Nothing leaves this Mac." },
      ],
    };
    const contoso = {
      ...NONE,
      org_name: "Contoso",
      telemetry_off: true,
      rules: [{ kind: "telemetry_off", source: "Contoso", reason: "" }],
    };
    const org = (id: string, name: string, workspaces: string[]) => ({ id, name, reason: "", vocabulary: [], workspaces });
    const enforced = {
      policy: NONE,
      orgs: [org("o1", "Northwind", ["w1"]), org("o2", "Contoso", ["w9"])],
      org_policies: [
        { org_id: "o1", workspaces: ["w1"], policy: northwind },
        { org_id: "o2", workspaces: ["w9"], policy: contoso },
      ],
    };

    it("lists each organisation's rules with the departments they apply to", async () => {
      renderApp("/settings?tab=account", {
        cloud_status: () => signedIn("member", false),
        cloud_workspace_members: () => MEMBERS,
        org_status: () => enforced,
      });
      const dialog = await screen.findByRole("dialog", { name: /settings/i });
      expect(await within(dialog).findByRole("heading", { name: "Policies" })).toBeInTheDocument();
      expect(within(dialog).getByText("Northwind · Acme")).toBeInTheDocument();
      expect(within(dialog).getByText("Contoso")).toBeInTheDocument();
      expect(within(dialog).getByText("On this Mac only")).toBeInTheDocument();
      expect(within(dialog).getByText("Deleted after 30 days")).toBeInTheDocument();
      expect(within(dialog).getByText("Send anonymous usage counters")).toBeInTheDocument();

      const badges = within(dialog).getAllByRole("button", { name: /managed by northwind/i });
      expect(badges).toHaveLength(2);
      expect(within(dialog).getAllByRole("button", { name: /managed by contoso/i })).toHaveLength(1);
      await userEvent.click(badges[0]);
      expect(await screen.findByText("Nothing leaves this Mac.")).toBeInTheDocument();
      expect(screen.getByText(/only an administrator can change it/i)).toBeInTheDocument();
    });

    it("names an organisation's departments behind 2FA, never every workspace", async () => {
      const lockedDept = { id: "wl", name: "Barnevern" };
      renderApp("/settings?tab=account", {
        cloud_status: () => ({ ...signedIn("member", false), locked_workspaces: [lockedDept] }),
        cloud_workspace_members: () => MEMBERS,
        org_status: () => ({
          policy: NONE,
          orgs: [org("o1", "Northwind", [])],
          org_policies: [{ org_id: "o1", workspaces: ["wl"], policy: northwind }],
        }),
      });
      const dialog = await screen.findByRole("dialog", { name: /settings/i });
      expect(await within(dialog).findByText("Northwind · Barnevern")).toBeInTheDocument();
      expect(within(dialog).queryByText(/Every workspace/)).toBeNull();
    });

    it("keeps them listed after sign-out", async () => {
      renderApp("/settings?tab=account", {
        cloud_status: () => signedOutConfigured(),
        org_status: () => enforced,
      });
      const dialog = await screen.findByRole("dialog", { name: /settings/i });
      expect(await within(dialog).findByText("On this Mac only")).toBeInTheDocument();
    });

    it("names this Mac's configuration as the source of a rule for every workspace", async () => {
      const managed = {
        ...NONE,
        org_name: "This Mac's configuration",
        telemetry_off: true,
        rules: [{ kind: "telemetry_off", source: "This Mac's configuration", reason: "" }],
      };
      renderApp("/settings?tab=account", {
        cloud_status: () => signedOutConfigured(),
        org_status: () => ({ policy: managed, managed }),
      });
      const dialog = await screen.findByRole("dialog", { name: /settings/i });
      expect(await within(dialog).findByText("Send anonymous usage counters")).toBeInTheDocument();
      expect(within(dialog).getByText("This Mac's configuration · Every workspace")).toBeInTheDocument();
      expect(within(dialog).getByRole("button", { name: /managed by this mac's configuration/i })).toBeInTheDocument();
    });

    it("is absent when no policy is on", async () => {
      const dialog = await openAccount(signedIn("member", false));
      expect(await within(dialog).findByText("Acme")).toBeInTheDocument();
      expect(within(dialog).queryByRole("heading", { name: "Policies" })).not.toBeInTheDocument();
    });
  });
});
