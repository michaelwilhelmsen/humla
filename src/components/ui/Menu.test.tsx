import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { Menu, MenuContent, MenuItem, MenuRadioGroup, MenuRadioItem, MenuTrigger } from "./Menu";

// These cover the behaviours the six hand-rolled popovers were each missing a
// different half of (#114). They belong on the primitive rather than being
// re-asserted at every call site.

function MenuHarness({ onSelect = vi.fn() }: { onSelect?: (id: string) => void }) {
  return (
    <Menu>
      <MenuTrigger aria-label="Actions">Open</MenuTrigger>
      <MenuContent>
        <MenuItem onSelect={() => onSelect("rename")}>Rename</MenuItem>
        <MenuItem onSelect={() => onSelect("duplicate")}>Duplicate</MenuItem>
        <MenuItem danger onSelect={() => onSelect("delete")}>
          Delete
        </MenuItem>
      </MenuContent>
    </Menu>
  );
}

describe("Menu", () => {
  it("opens from the trigger and reports the chosen item", async () => {
    const user = userEvent.setup();
    const onSelect = vi.fn();
    render(<MenuHarness onSelect={onSelect} />);

    expect(screen.queryByRole("menu")).toBeNull();
    await user.click(screen.getByRole("button", { name: "Actions" }));
    await user.click(screen.getByRole("menuitem", { name: "Duplicate" }));

    expect(onSelect).toHaveBeenCalledWith("duplicate");
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("roves focus with the arrow keys and wraps at both ends", async () => {
    const user = userEvent.setup();
    render(<MenuHarness />);

    await user.click(screen.getByRole("button", { name: "Actions" }));
    const items = screen.getAllByRole("menuitem");

    await user.keyboard("{ArrowDown}");
    expect(items[0]).toHaveFocus();
    await user.keyboard("{ArrowUp}"); // wraps to the last item
    expect(items[2]).toHaveFocus();
    await user.keyboard("{ArrowDown}"); // wraps back to the first
    expect(items[0]).toHaveFocus();
  });

  it("jumps to a row by typing its first letters", async () => {
    const user = userEvent.setup();
    render(<MenuHarness />);

    await user.click(screen.getByRole("button", { name: "Actions" }));
    await user.keyboard("de");

    expect(screen.getByRole("menuitem", { name: "Delete" })).toHaveFocus();
  });

  it("closes on Escape and returns focus to the trigger", async () => {
    const user = userEvent.setup();
    render(<MenuHarness />);
    const trigger = screen.getByRole("button", { name: "Actions" });

    await user.click(trigger);
    await user.keyboard("{Escape}");

    expect(screen.queryByRole("menu")).toBeNull();
    expect(trigger).toHaveFocus();
  });

  it("portals out of an overflow-clipping ancestor", async () => {
    const user = userEvent.setup();
    const { container } = render(
      <div data-testid="clipper" style={{ overflow: "hidden" }}>
        <MenuHarness />
      </div>,
    );

    await user.click(screen.getByRole("button", { name: "Actions" }));
    const menu = screen.getByRole("menu");
    const clipper = container.querySelector('[data-testid="clipper"]')!;

    expect(clipper.contains(menu)).toBe(false);
    expect(document.body.contains(menu)).toBe(true);
  });

  it("marks exactly one radio row as checked", async () => {
    const user = userEvent.setup();
    const onValueChange = vi.fn();
    render(
      <Menu>
        <MenuTrigger aria-label="Scope">Scope</MenuTrigger>
        <MenuContent>
          <MenuRadioGroup value="note" onValueChange={onValueChange}>
            <MenuRadioItem value="note">This note</MenuRadioItem>
            <MenuRadioItem value="all">All notes</MenuRadioItem>
          </MenuRadioGroup>
        </MenuContent>
      </Menu>,
    );

    await user.click(screen.getByRole("button", { name: "Scope" }));
    expect(screen.getByRole("menuitemradio", { name: "This note" })).toHaveAttribute(
      "aria-checked",
      "true",
    );
    expect(screen.getByRole("menuitemradio", { name: "All notes" })).toHaveAttribute(
      "aria-checked",
      "false",
    );

    await user.click(screen.getByRole("menuitemradio", { name: "All notes" }));
    expect(onValueChange).toHaveBeenCalledWith("all");
  });
});
