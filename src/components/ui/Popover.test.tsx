import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { Popover, PopoverContent, PopoverTrigger } from "./Popover";

// `Popover` has no call site yet — it is the base #116's Combobox and #115's
// Client pin are built on (see #114's scope). These assertions stand in for the
// call site the six-widget migration couldn't give it: the panel portals out of
// its container, takes arbitrary content including a focusable input, and hands
// focus back to the trigger when dismissed.

describe("Popover", () => {
  it("opens a portalled panel and dismisses on Escape", async () => {
    const user = userEvent.setup();
    render(
      <Popover>
        <PopoverTrigger aria-label="Details">Open</PopoverTrigger>
        <PopoverContent aria-label="Details panel">
          <input aria-label="Filter" />
        </PopoverContent>
      </Popover>,
    );
    const trigger = screen.getByRole("button", { name: "Details" });

    await user.click(trigger);
    const input = screen.getByRole("textbox", { name: "Filter" });
    expect(document.body.contains(input)).toBe(true);

    await user.keyboard("{Escape}");
    expect(screen.queryByRole("textbox", { name: "Filter" })).toBeNull();
    expect(trigger).toHaveFocus();
  });
});
