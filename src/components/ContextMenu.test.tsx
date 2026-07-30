import { describe, it, expect, vi } from "vitest";
import { useState } from "react";
import { act, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { ContextMenu, ContextMenuItem } from "./ContextMenu";

// The right-click menu keeps its x/y API over the shared `Menu` primitive
// (#114). What's worth pinning here is what the primitive does NOT give us: the
// menu is anchored to raw pointer coordinates rather than to a live element, so
// dismissal on scroll and focus restoration are this component's own.

function Harness({ onClose = vi.fn() }: { onClose?: () => void }) {
  return (
    <ContextMenu x={40} y={80} onClose={onClose}>
      <ContextMenuItem onClick={vi.fn()}>Rename</ContextMenuItem>
      <ContextMenuItem onClick={vi.fn()} danger>
        Delete
      </ContextMenuItem>
    </ContextMenu>
  );
}

/** A row that opens the menu on right-click, as the real call sites do. */
function RowHarness() {
  const [open, setOpen] = useState(false);
  return (
    <>
      <button onContextMenu={() => setOpen(true)}>row</button>
      {open && (
        <ContextMenu x={10} y={10} onClose={() => setOpen(false)}>
          <ContextMenuItem onClick={vi.fn()}>Rename</ContextMenuItem>
        </ContextMenu>
      )}
    </>
  );
}

describe("ContextMenu", () => {
  it("opens immediately at the given coordinates", () => {
    render(<Harness />);
    expect(screen.getByRole("menu")).toBeInTheDocument();
    expect(screen.getAllByRole("menuitem")).toHaveLength(2);
  });

  it("reports a chosen item through onClick and closes", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    const onRename = vi.fn();
    render(
      <ContextMenu x={0} y={0} onClose={onClose}>
        <ContextMenuItem onClick={onRename}>Rename</ContextMenuItem>
      </ContextMenu>,
    );

    await user.click(screen.getByRole("menuitem", { name: "Rename" }));
    expect(onRename).toHaveBeenCalledTimes(1);
    expect(onClose).toHaveBeenCalled();
  });

  it("closes on scroll — the anchor is a coordinate, not a live element", () => {
    const onClose = vi.fn();
    render(<Harness onClose={onClose} />);

    fireEvent.scroll(document, {});
    expect(onClose).toHaveBeenCalled();
  });

  it("closes on Escape", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    render(<Harness onClose={onClose} />);

    await user.keyboard("{Escape}");
    expect(onClose).toHaveBeenCalled();
  });

  it("closes on an outside press", async () => {
    const onClose = vi.fn();
    render(<Harness onClose={onClose} />);
    // Radix arms its outside-pointerdown listener a tick after mount, so that
    // the very press which opened a layer can't immediately dismiss it.
    await act(() => new Promise((resolve) => setTimeout(resolve, 0)));

    fireEvent.pointerDown(document.body);
    expect(onClose).toHaveBeenCalled();
  });

  it("hands focus back to the row it opened from, not to <body>", async () => {
    const user = userEvent.setup();
    render(<RowHarness />);
    const row = screen.getByRole("button", { name: "row" });
    row.focus();

    fireEvent.contextMenu(row);
    expect(await screen.findByRole("menu")).toBeInTheDocument();

    await user.keyboard("{Escape}");
    expect(screen.queryByRole("menu")).toBeNull();
    // Radix would hand focus to its trigger, but ours is a virtual span that
    // unmounts with the menu — so focus would otherwise land on <body>.
    expect(row).toHaveFocus();
  });
});
