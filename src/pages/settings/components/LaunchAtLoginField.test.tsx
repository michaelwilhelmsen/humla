import { describe, it, expect, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { LaunchAtLoginField } from "./LaunchAtLoginField";
import { mockTauri } from "../../../test/tauri";

// Open at login (#207). The login item on disk is the record, so the switch
// must show what the OS has, not what was last clicked.

function setup(installed: boolean, set = vi.fn(async (_args: { on: boolean }) => null)) {
  mockTauri({
    launch_at_login_get: () => installed,
    launch_at_login_set: (args) => set(args as { on: boolean }),
  });
  return { set };
}

describe("LaunchAtLoginField", () => {
  it("shows whether the login item is installed", async () => {
    setup(true);
    render(<LaunchAtLoginField />);
    const toggle = await screen.findByRole("switch", { name: "Open at login" });
    expect(toggle).toHaveAttribute("aria-checked", "true");
  });

  it("installs the login item when switched on", async () => {
    const { set } = setup(false);
    render(<LaunchAtLoginField />);
    await userEvent.click(await screen.findByRole("switch", { name: "Open at login" }));
    await waitFor(() => expect(set).toHaveBeenCalledWith({ on: true }));
    expect(screen.getByRole("switch", { name: "Open at login" })).toHaveAttribute(
      "aria-checked",
      "true",
    );
  });

  it("puts the switch back and says why when the OS refuses", async () => {
    setup(
      false,
      vi.fn(async () => {
        throw "Permission denied";
      }),
    );
    render(<LaunchAtLoginField />);
    await userEvent.click(await screen.findByRole("switch", { name: "Open at login" }));
    expect(await screen.findByText(/Permission denied/)).toBeInTheDocument();
    expect(screen.getByRole("switch", { name: "Open at login" })).toHaveAttribute(
      "aria-checked",
      "false",
    );
  });
});
