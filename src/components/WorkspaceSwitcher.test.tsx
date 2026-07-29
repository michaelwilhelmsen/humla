import { describe, it, expect, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { WorkspaceSwitcher } from "./WorkspaceSwitcher";
import { LocationProbe } from "../test/app";
import { mockTauri } from "../test/tauri";
import {
  DISCONNECTED,
  useCloudStore,
  type CloudStatus,
  type CloudWorkspace,
} from "../lib/cloud";

function status(over: Partial<CloudStatus> = {}): CloudStatus {
  return { ...DISCONNECTED, configured: true, ...over };
}

const ACME: CloudWorkspace = {
  id: "w1",
  name: "Acme",
  role: "owner",
  plan_status: "trialing",
};

function renderSwitcher() {
  return render(
    <MemoryRouter initialEntries={["/all-notes"]}>
      <WorkspaceSwitcher />
      <LocationProbe />
    </MemoryRouter>,
  );
}

const loc = () => screen.getByTestId("location").textContent;

beforeEach(() => {
  mockTauri();
  useCloudStore.setState({ status: status(), syncStatus: null });
});

describe("WorkspaceSwitcher team hint", () => {
  it("shows the Add team pill on Personal", () => {
    renderSwitcher();
    expect(screen.getByText("Add team")).toBeInTheDocument();
  });

  it("goes to Settings → Account without opening the dropdown", () => {
    renderSwitcher();
    fireEvent.click(screen.getByText("Add team"));
    expect(loc()).toBe("/settings?tab=account");
    // The click must not bubble to the trigger, or the menu would be left open
    // behind the navigation.
    expect(screen.queryByText("Workspaces")).not.toBeInTheDocument();
    expect(
      screen.queryByText("Sign in to sync & collaborate"),
    ).not.toBeInTheDocument();
  });

  it("is replaced by the role pill once a workspace is active", () => {
    useCloudStore.setState({
      status: status({
        logged_in: true,
        workspaces: [ACME],
        current_workspace: ACME,
      }),
    });
    renderSwitcher();
    expect(screen.queryByText("Add team")).not.toBeInTheDocument();
    expect(screen.getByText("Owner")).toBeInTheDocument();
  });
});
