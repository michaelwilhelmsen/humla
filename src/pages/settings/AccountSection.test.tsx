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
    expect(within(dialog).getByText("$7")).toBeInTheDocument();
    expect(
      within(dialog).getByText(/month for the entire team/i),
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
      await within(dialog).findByText(/isn't verified yet/i),
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
    const trigger = await within(dialog).findByRole("button", {
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
});
