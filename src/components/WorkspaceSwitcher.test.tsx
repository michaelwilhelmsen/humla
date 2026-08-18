import { describe, it, expect, beforeEach } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { WorkspaceSwitcher } from "./WorkspaceSwitcher";
import { DISCONNECTED, useCloudStore, type CloudStatus } from "../lib/cloud";
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
