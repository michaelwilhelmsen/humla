import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { mockTauri } from "../test/tauri";
import { Permissions } from "./Permissions";

// Microphone and screen granted, so the only buttons on screen are the
// notifications row's.
function status(notifications: string) {
  return { microphone: "granted", screen: "granted", notifications };
}

describe("Settings → Permissions: notifications", () => {
  it("offers to ask when Humla hasn't asked yet, and shows the answer", async () => {
    const asked: unknown[] = [];
    mockTauri({
      permissions_status: () => status("not_determined"),
      permissions_request: (args) => {
        asked.push(args);
        return status("granted");
      },
    });
    render(<Permissions />);

    expect(await screen.findByText("Notifications")).toBeTruthy();
    expect(screen.getByText("Not requested")).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "Request" }));

    expect(asked).toEqual([{ kind: "notifications" }]);
    expect(await screen.findAllByText("Granted")).toHaveLength(3);
    expect(screen.queryByRole("button", { name: "Request" })).toBeNull();
  });

  it("keeps the microphone askable while macOS waits on the notifications answer", async () => {
    mockTauri({
      permissions_status: () => ({
        microphone: "not_determined",
        screen: "granted",
        notifications: "not_determined",
      }),
      permissions_request: (args) =>
        (args as { kind: string }).kind === "notifications" ? new Promise(() => {}) : null,
    });
    render(<Permissions />);

    await screen.findByText("Notifications");
    const [mic, notifications] = screen.getAllByRole("button", { name: "Request" });
    await userEvent.click(notifications);
    expect((mic as HTMLButtonElement).disabled).toBe(false);
  });

  it("sends the user to System Settings once notifications were turned down", async () => {
    const opened: unknown[] = [];
    mockTauri({
      permissions_status: () => status("denied"),
      permissions_open_settings: (args) => {
        opened.push(args);
        return null;
      },
    });
    render(<Permissions />);

    expect(await screen.findByText("Denied")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Request" })).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: "Open System Settings" }));
    expect(opened).toEqual([{ kind: "notifications" }]);
  });
});
