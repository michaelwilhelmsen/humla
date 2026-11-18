import { describe, it, expect, beforeEach } from "vitest";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router-dom";
import { DISCONNECTED, useCloudStore, type CloudStatus, type OrgInvite } from "../lib/cloud";
import { mockTauri } from "../test/tauri";
import { renderApp } from "../test/app";
import { WorkspaceSwitcher } from "./WorkspaceSwitcher";

const INVITE: OrgInvite = {
  id: "i1",
  workspace_id: "ws-org",
  workspace_name: "Child welfare",
  organisation_name: "Bergen kommune",
  role: "member",
  invited_by_name: "Kari",
  created: "2026-11-18 10:00:00.000Z",
};

function signedIn(invites: OrgInvite[]): CloudStatus {
  return {
    ...DISCONNECTED,
    configured: true,
    logged_in: true,
    base_url: "https://sync.humla.team",
    user: { id: "u1", email: "m@example.no", name: "Michael", verified: true },
    invites,
  };
}

async function openInvites(handlers: Parameters<typeof mockTauri>[0] = {}, status = signedIn([INVITE])) {
  renderApp("/settings?tab=account", { cloud_status: () => status, ...handlers });
  const dialog = await screen.findByRole("dialog", { name: /settings/i });
  return await within(dialog).findByRole("group", { name: /invite to child welfare/i });
}

beforeEach(() => {
  useCloudStore.setState({ status: DISCONNECTED, ready: true });
});

describe("Settings → Account invitations", () => {
  it("shows the department, the organisation and who invited", async () => {
    const card = await openInvites();
    expect(within(card).getByText("Child welfare")).toBeInTheDocument();
    expect(within(card).getByText("Bergen kommune · Invited by Kari")).toBeInTheDocument();
    expect(within(card).getByRole("button", { name: "Accept" })).toBeInTheDocument();
    expect(within(card).getByRole("button", { name: "Decline" })).toBeInTheDocument();
  });

  it("says what joining means before it accepts", async () => {
    const accepts: unknown[] = [];
    const card = await openInvites({
      cloud_accept_invite: (args) => {
        accepts.push(args);
        return "ws-org";
      },
    });
    await userEvent.click(within(card).getByRole("button", { name: "Accept" }));

    expect(accepts).toHaveLength(0);
    expect(within(card).getByText(/bergen kommune's policies will apply to humla on this mac/i)).toBeInTheDocument();
    expect(within(card).getByText("On this Mac only")).toBeInTheDocument();
    expect(within(card).getByText("Off, and stored audio deleted")).toBeInTheDocument();

    await userEvent.click(within(card).getByRole("button", { name: /join child welfare/i }));
    expect(accepts).toEqual([{ inviteId: "i1" }]);
  });

  it("Cancel goes back without accepting", async () => {
    const accepts: unknown[] = [];
    const card = await openInvites({ cloud_accept_invite: (a) => accepts.push(a) });
    await userEvent.click(within(card).getByRole("button", { name: "Accept" }));
    await userEvent.click(within(card).getByRole("button", { name: "Cancel" }));
    expect(within(card).queryByText(/policies will apply/i)).toBeNull();
    expect(accepts).toHaveLength(0);
  });

  it("decline removes the invite", async () => {
    const declines: unknown[] = [];
    const card = await openInvites({
      cloud_decline_invite: (args) => {
        declines.push(args);
        return null;
      },
    });
    // The status the app refetches no longer lists it either.
    mockTauri({ cloud_status: () => signedIn([]), cloud_decline_invite: (a) => declines.push(a) });
    await userEvent.click(within(card).getByRole("button", { name: "Decline" }));
    expect(declines).toEqual([{ inviteId: "i1" }]);
    expect(screen.queryByRole("group", { name: /invite to child welfare/i })).toBeNull();
  });

  it("keeps the invite and points to two-factor set-up when the department requires it", async () => {
    const card = await openInvites({
      cloud_accept_invite: () => {
        throw {
          reason: "two_factor_required",
          message: "This department requires two-factor authentication. Set it up in Settings → Account, then accept again.",
        };
      },
    });
    await userEvent.click(within(card).getByRole("button", { name: "Accept" }));
    await userEvent.click(within(card).getByRole("button", { name: /join child welfare/i }));

    expect(await within(card).findByText(/requires two-factor authentication/i)).toBeInTheDocument();
    expect(within(card).getByRole("button", { name: "Accept" })).toBeInTheDocument();
    expect(useCloudStore.getState().status.invites).toEqual([INVITE]);
  });

  it("drops an invite that is no longer valid", async () => {
    const card = await openInvites({
      cloud_accept_invite: () => {
        throw { reason: "invite_expired", message: "This invite is no longer valid." };
      },
    });
    await userEvent.click(within(card).getByRole("button", { name: "Accept" }));
    await userEvent.click(within(card).getByRole("button", { name: /join child welfare/i }));
    expect(await screen.findByText("This invite is no longer valid.")).toBeInTheDocument();
    expect(screen.queryByRole("group", { name: /invite to child welfare/i })).toBeNull();
  });

  it("shows nothing without invites", async () => {
    renderApp("/settings?tab=account", { cloud_status: () => signedIn([]) });
    const dialog = await screen.findByRole("dialog", { name: /settings/i });
    await within(dialog).findByRole("heading", { name: /^account$/i });
    expect(within(dialog).queryByRole("heading", { name: /invitations/i })).toBeNull();
  });
});

describe("the workspace switcher's invitations", () => {
  function seed(status: CloudStatus, handlers: Parameters<typeof mockTauri>[0] = {}) {
    mockTauri({ cloud_status: () => status, ...handlers });
    useCloudStore.setState({ status, ready: true });
    return render(
      <MemoryRouter>
        <WorkspaceSwitcher />
      </MemoryRouter>,
    );
  }

  it("marks the trigger and lists each invite", async () => {
    seed(signedIn([INVITE]));
    expect(screen.getByLabelText("1 invitation")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: /personal/i }));
    expect(await screen.findByRole("menuitem", { name: /child welfare/i })).toBeInTheDocument();
  });

  it("opens the invite with Accept and Decline", async () => {
    seed(signedIn([INVITE]));
    await userEvent.click(screen.getByRole("button", { name: /personal/i }));
    await userEvent.click(await screen.findByRole("menuitem", { name: /child welfare/i }));
    const dialog = await screen.findByRole("dialog", { name: "Invitation" });
    expect(within(dialog).getByText("Bergen kommune · Invited by Kari")).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "Accept" })).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: "Decline" })).toBeInTheDocument();
  });

  it("shows no invitations signed out", () => {
    seed({ ...signedIn([INVITE]), logged_in: false });
    expect(screen.queryByLabelText(/invitation/)).toBeNull();
  });
});

describe("inviting into an organisation department", () => {
  const DEPT = { id: "w1", name: "Child welfare", role: "owner" as const, plan_status: "active" as const };
  const ORG_STATUS = {
    policy: { org_name: null, reason: null, local_only: false, retention_days: 0, telemetry_off: false, audio_forbidden: false, rules: [] },
    orgs: [{ id: "o1", name: "Bergen kommune", reason: "", vocabulary: [], workspaces: ["w1"] }],
  };
  const status = { ...signedIn([]), current_workspace: DEPT, workspaces: [DEPT] };
  const MEMBERS = [{ id: "u1", email: "m@example.no", name: "Michael", role: "owner" }];

  it("says the invite waits for their answer", async () => {
    renderApp("/settings?tab=account", {
      cloud_status: () => status,
      cloud_workspace_members: () => MEMBERS,
      cloud_invite_member: () => "pending",
    });
    const dialog = await screen.findByRole("dialog", { name: /settings/i });
    await userEvent.type(await within(dialog).findByPlaceholderText(/teammate@example.com/i), "ola@example.no");
    await userEvent.click(within(dialog).getByRole("button", { name: /^invite$/i }));
    expect(await within(dialog).findByText("Invite sent. They join when they accept.")).toBeInTheDocument();
    expect(within(dialog).queryByText(/^added /i)).toBeNull();
  });

  it("lists open invites with Withdraw", async () => {
    let open = [{ id: "i9", email: "ola@example.no", role: "member", invited_by_name: "Michael", created: "" }];
    const withdrawn: unknown[] = [];
    renderApp("/settings?tab=account", {
      cloud_status: () => status,
      cloud_workspace_members: () => MEMBERS,
      org_status: () => ORG_STATUS,
      cloud_pending_invites: () => open,
      cloud_withdraw_invite: (args) => {
        withdrawn.push(args);
        open = [];
        return null;
      },
    });
    const dialog = await screen.findByRole("dialog", { name: /settings/i });
    expect(await within(dialog).findByText("ola@example.no")).toBeInTheDocument();
    await userEvent.click(within(dialog).getByRole("button", { name: "Withdraw" }));
    expect(withdrawn).toEqual([{ inviteId: "i9" }]);
    await waitFor(() => expect(within(dialog).queryByText("ola@example.no")).toBeNull());
  });

  it("lists no open invites in a workspace outside an organisation", async () => {
    const asked: unknown[] = [];
    renderApp("/settings?tab=account", {
      cloud_status: () => status,
      cloud_workspace_members: () => MEMBERS,
      cloud_pending_invites: (a) => {
        asked.push(a);
        return [];
      },
    });
    const dialog = await screen.findByRole("dialog", { name: /settings/i });
    await within(dialog).findByText(/add member by email/i);
    expect(asked).toHaveLength(0);
  });
});

