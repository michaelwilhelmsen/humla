import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { HotkeyField } from "./HotkeyField";
import { mockTauri } from "../../../test/tauri";

// The global record shortcut's recorder (#21). It is the only settings control
// that has to be registered with the OS before it can be saved, so "what the
// row shows" and "what is actually registered" must never diverge.

function setup(overrides: Record<string, (args: unknown) => unknown> = {}) {
  const set = vi.fn(async (_args: { accel: string }) => null);
  mockTauri({
    record_hotkey_get: () => "Command+Control+KeyR",
    record_hotkey_set: (args) => set(args as { accel: string }),
    ...overrides,
  });
  return { set };
}

beforeEach(() => {
  setup();
});

describe("HotkeyField", () => {
  it("shows the registered shortcut as macOS glyphs", async () => {
    render(<HotkeyField />);
    expect(await screen.findByRole("button", { name: /⌃⌘R/ })).toBeInTheDocument();
  });

  it("records a new combination and registers it", async () => {
    const { set } = setup();
    render(<HotkeyField />);
    await userEvent.click(await screen.findByRole("button", { name: /⌃⌘R/ }));
    expect(screen.getByText(/press a shortcut/i)).toBeInTheDocument();

    await userEvent.keyboard("{Alt>}{Shift>}J{/Shift}{/Alt}");

    await waitFor(() => expect(set).toHaveBeenCalledWith({ accel: "Alt+Shift+KeyJ" }));
    expect(await screen.findByRole("button", { name: /⌥⇧J/ })).toBeInTheDocument();
  });

  // Holding ⌥ down is the user mid-press, not a shortcut — the row has to keep
  // waiting rather than complain or commit.
  it("keeps waiting while only a modifier is held", async () => {
    const { set } = setup();
    render(<HotkeyField />);
    await userEvent.click(await screen.findByRole("button", { name: /⌃⌘R/ }));

    await userEvent.keyboard("{Alt>}");

    expect(screen.getByText(/press a shortcut/i)).toBeInTheDocument();
    expect(screen.queryByText(/⌘, ⌃ or ⌥/)).not.toBeInTheDocument();
    expect(set).not.toHaveBeenCalled();
    await userEvent.keyboard("{/Alt}");
  });

  it("refuses a combination with no real modifier and says why", async () => {
    const { set } = setup();
    render(<HotkeyField />);
    await userEvent.click(await screen.findByRole("button", { name: /⌃⌘R/ }));

    await userEvent.keyboard("{Shift>}J{/Shift}");

    expect(await screen.findByText(/⌘, ⌃ or ⌥/)).toBeInTheDocument();
    expect(set).not.toHaveBeenCalled();
    // Still armed, so the user can just press a valid one.
    expect(screen.getByText(/press a shortcut/i)).toBeInTheDocument();
  });

  it("cancels on Escape and keeps the old shortcut", async () => {
    const { set } = setup();
    render(<HotkeyField />);
    await userEvent.click(await screen.findByRole("button", { name: /⌃⌘R/ }));

    await userEvent.keyboard("{Escape}");

    expect(set).not.toHaveBeenCalled();
    expect(await screen.findByRole("button", { name: /⌃⌘R/ })).toBeInTheDocument();
  });

  it("turns the shortcut off, and offers the default back", async () => {
    const { set } = setup();
    render(<HotkeyField />);
    await screen.findByRole("button", { name: /⌃⌘R/ });

    await userEvent.click(screen.getByRole("button", { name: /turn off/i }));

    await waitFor(() => expect(set).toHaveBeenCalledWith({ accel: "" }));
    expect(await screen.findByRole("button", { name: /none/i })).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: /use ⌃⌘R/i }));
    await waitFor(() =>
      expect(set).toHaveBeenCalledWith({ accel: "Command+Control+KeyR" }),
    );
  });

  // A combination another app already owns is rejected by the OS. The row must
  // fall back to what is still registered rather than show a dead shortcut.
  it("reverts and reports when the OS refuses the combination", async () => {
    setup({
      record_hotkey_set: () => {
        throw new Error("HotKey already registered");
      },
    });
    render(<HotkeyField />);
    await userEvent.click(await screen.findByRole("button", { name: /⌃⌘R/ }));

    await userEvent.keyboard("{Control>}{Alt>}J{/Alt}{/Control}");

    expect(await screen.findByText(/already registered/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /⌃⌘R/ })).toBeInTheDocument();
  });
});
