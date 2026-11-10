import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { mockTauri } from "../../../test/tauri";
import { PermissionsStep } from "./Permissions";
import type { StepContext } from "../types";

function ctx(): StepContext {
  return {
    stepId: "permissions",
    index: 1,
    total: 6,
    goNext: vi.fn(),
    goBack: vi.fn(),
    goTo: vi.fn(),
    canGoBack: true,
    complete: vi.fn(),
  } as unknown as StepContext;
}

function status(microphone: string, notifications: string) {
  return { microphone, screen: "granted", notifications };
}

describe("onboarding permissions: notifications", () => {
  it("offers notifications without making Continue wait for the answer", async () => {
    const asked: unknown[] = [];
    mockTauri({
      permissions_status: () => status("granted", "not_determined"),
      permissions_request: (args) => {
        asked.push(args);
        return new Promise(() => {});
      },
    });
    const step = ctx();
    render(<PermissionsStep ctx={step} />);

    expect(await screen.findByText("Notifications")).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "Allow notifications" }));
    expect(asked).toEqual([{ kind: "notifications" }]);

    await userEvent.click(screen.getByRole("button", { name: "Continue" }));
    expect(step.goNext).toHaveBeenCalled();
  });

  // macOS's notification prompt waits for an answer; the microphone, which
  // Humla can't record without, must stay askable meanwhile.
  it("keeps the microphone askable while macOS waits on the notifications answer", async () => {
    mockTauri({
      permissions_status: () => status("not_determined", "not_determined"),
      permissions_request: (args) =>
        (args as { kind: string }).kind === "notifications"
          ? new Promise(() => {})
          : status("granted", "not_determined"),
    });
    render(<PermissionsStep ctx={ctx()} />);

    await userEvent.click(await screen.findByRole("button", { name: "Allow notifications" }));
    const mic = screen.getByRole("button", { name: "Allow microphone" }) as HTMLButtonElement;
    expect(mic.disabled).toBe(false);
  });
});
