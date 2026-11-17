import { describe, it, expect } from "vitest";
import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { renderApp } from "../../test/app";
import type { TwoFactorStatus, WorkspaceRef } from "../../lib/cloud";

// Settings → Account → Two-factor authentication. Turning it on links an
// authenticator app and hands over recovery codes once; with it on, the app can
// be replaced, the codes renewed, and it can be turned off — unless a workspace
// the user is in requires it.

const OFF: TwoFactorStatus = { available: true, enabled: false, recovery_codes_left: 0, required_by: [] };
const ON: TwoFactorStatus = { available: true, enabled: true, recovery_codes_left: 10, required_by: [] };

function signedIn(two_factor: TwoFactorStatus, locked_workspaces: WorkspaceRef[] = []) {
  return {
    configured: true,
    logged_in: true,
    base_url: "https://sync.humla.team",
    user: { id: "u1", email: "m@example.no", name: "Michael", verified: true },
    current_workspace: null,
    workspaces: [],
    billing_enabled: false,
    two_factor,
    locked_workspaces,
  };
}

const CODES = Array.from({ length: 10 }, (_, i) => `AAAAA-1111${i}`);

async function openAccount(handlers: Record<string, (args: unknown) => unknown>) {
  renderApp("/settings?tab=account", handlers);
  const dialog = await screen.findByRole("dialog", { name: /settings/i });
  await within(dialog).findByRole("heading", { name: /two-factor authentication/i });
  return dialog;
}

describe("Settings → two-factor authentication", () => {
  it("is turned on with the password, the app's first code, then shows the recovery codes once", async () => {
    let status = signedIn(OFF);
    const setups: unknown[] = [];
    const enables: unknown[] = [];
    const dialog = await openAccount({
      cloud_status: () => status,
      cloud_two_factor_setup: (args) => {
        setups.push(args);
        return {
          secret: "JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP",
          otpauth_uri: "otpauth://totp/Humla:m%40example.no?secret=JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP",
          qr: { size: 21, path: "M0 0h7v1h-7z" },
        };
      },
      cloud_two_factor_enable: (args) => {
        enables.push(args);
        status = signedIn({ ...ON });
        return CODES;
      },
    });

    await userEvent.click(within(dialog).getByRole("button", { name: /turn on/i }));
    const flow = await screen.findByRole("dialog", { name: /turn on two-factor authentication/i });
    await userEvent.type(within(flow).getByLabelText(/^password$/i), "hunter22{Enter}");

    expect(await within(flow).findByRole("img", { name: /qr code/i })).toBeInTheDocument();
    expect(within(flow).getByText("JBSW Y3DP EHPK 3PXP JBSW Y3DP EHPK 3PXP")).toBeInTheDocument();
    await userEvent.type(within(flow).getByLabelText(/authentication code/i), "123456{Enter}");

    expect(await within(flow).findByText(CODES[0])).toBeInTheDocument();
    expect(within(flow).getByText(CODES[9])).toBeInTheDocument();
    await userEvent.click(within(flow).getByRole("button", { name: /^done$/i }));

    expect(setups).toEqual([{ password: "hunter22", code: null, recoveryCode: null }]);
    expect(enables).toEqual([{ code: "123456" }]);
    expect(screen.queryByRole("dialog", { name: /turn on two-factor/i })).not.toBeInTheDocument();
    expect(await within(dialog).findByText(/10 left/i)).toBeInTheDocument();
  });

  it("is not offered by a server without it", async () => {
    renderApp("/settings?tab=account", {
      cloud_status: () => signedIn({ ...OFF, available: false }),
    });
    const dialog = await screen.findByRole("dialog", { name: /settings/i });
    await within(dialog).findByText("Michael");
    expect(within(dialog).queryByRole("heading", { name: /two-factor/i })).not.toBeInTheDocument();
  });

  it("names the workspaces waiting for it", async () => {
    const dialog = await openAccount({
      cloud_status: () => signedIn(OFF, [{ id: "w9", name: "Secure Co" }]),
    });
    expect(within(dialog).getByText(/secure co requires two-factor authentication/i)).toBeInTheDocument();
  });

  it("can't be turned off while a workspace requires it, and says which", async () => {
    const dialog = await openAccount({
      cloud_status: () => signedIn({ ...ON, required_by: [{ id: "w1", name: "Acme" }] }),
    });
    expect(within(dialog).getByText(/acme requires it/i)).toBeInTheDocument();
    expect(within(dialog).getByRole("button", { name: /turn off/i })).toBeDisabled();
  });

  it("replaces the authenticator after the old one's code, or a recovery code", async () => {
    const setups: unknown[] = [];
    const dialog = await openAccount({
      cloud_status: () => signedIn(ON),
      cloud_two_factor_setup: (args) => {
        setups.push(args);
        return { secret: "JBSWY3DPEHPK3PXP", otpauth_uri: "otpauth://x", qr: null };
      },
    });

    await userEvent.click(within(dialog).getByRole("button", { name: /replace/i }));
    const flow = await screen.findByRole("dialog", { name: /replace your authenticator app/i });
    await userEvent.type(within(flow).getByLabelText(/^password$/i), "hunter22");
    await userEvent.click(within(flow).getByRole("button", { name: /use a recovery code instead/i }));
    await userEvent.type(within(flow).getByLabelText(/recovery code/i), "ABCDE-12345{Enter}");

    expect(await within(flow).findByText("JBSW Y3DP EHPK 3PXP")).toBeInTheDocument();
    expect(setups).toEqual([{ password: "hunter22", code: null, recoveryCode: "ABCDE-12345" }]);
  });

  // A re-render of the dialog must leave focus where the user is typing.
  it("keeps the cursor in the code field when the code is wrong", async () => {
    const dialog = await openAccount({
      cloud_status: () => signedIn(ON),
      cloud_two_factor_disable: () => {
        throw { reason: "invalid_code", message: "That code isn't right." };
      },
    });

    await userEvent.click(within(dialog).getByRole("button", { name: /turn off/i }));
    const flow = await screen.findByRole("dialog", { name: /turn off two-factor authentication/i });
    await userEvent.type(within(flow).getByLabelText(/^password$/i), "hunter22");
    const code = within(flow).getByLabelText(/authentication code/i);
    await userEvent.type(code, "000000{Enter}");

    expect(await within(flow).findByText("That code isn't right.")).toBeInTheDocument();
    expect(code).toHaveFocus();
  });

  it("is turned off with the password and a code", async () => {
    let status = signedIn(ON);
    const disables: unknown[] = [];
    const dialog = await openAccount({
      cloud_status: () => status,
      cloud_two_factor_disable: (args) => {
        disables.push(args);
        status = signedIn(OFF);
        return null;
      },
    });

    await userEvent.click(within(dialog).getByRole("button", { name: /turn off/i }));
    const flow = await screen.findByRole("dialog", { name: /turn off two-factor authentication/i });
    await userEvent.type(within(flow).getByLabelText(/^password$/i), "hunter22");
    await userEvent.type(within(flow).getByLabelText(/authentication code/i), "654321{Enter}");

    expect(await within(dialog).findByRole("button", { name: /turn on/i })).toBeInTheDocument();
    expect(disables).toEqual([{ password: "hunter22", code: "654321", recoveryCode: null }]);
  });
});
