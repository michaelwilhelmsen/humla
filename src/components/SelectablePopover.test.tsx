import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { SelectablePopover } from "./SelectablePopover";

const items = [
  { id: "c1", label: "Acme" },
  { id: "c2", label: "Globex" },
];

// Radix triggers open on pointerdown, not a bare click event.
async function open(ariaLabel = "Client") {
  await userEvent.click(screen.getByRole("button", { name: ariaLabel }));
}

describe("SelectablePopover", () => {
  it("marks the active item and selects another on click", async () => {
    const onSelect = vi.fn();
    render(
      <SelectablePopover ariaLabel="Client" trigger={<span>Acme</span>} items={items} activeId="c1" onSelect={onSelect} />,
    );
    await open();
    // Active row is checked.
    expect(screen.getByRole("menuitemradio", { name: /Acme/ })).toHaveAttribute("aria-checked", "true");
    expect(screen.getByRole("menuitemradio", { name: /Globex/ })).toHaveAttribute("aria-checked", "false");
    await userEvent.click(screen.getByRole("menuitemradio", { name: /Globex/ }));
    expect(onSelect).toHaveBeenCalledWith("c2");
  });

  it("unassigns via the none row", async () => {
    const onSelect = vi.fn();
    render(
      <SelectablePopover
        ariaLabel="Client"
        trigger={<span>Acme</span>}
        items={items}
        activeId="c1"
        onSelect={onSelect}
        noneLabel="No client"
      />,
    );
    await open();
    await userEvent.click(screen.getByRole("menuitemradio", { name: "No client" }));
    expect(onSelect).toHaveBeenCalledWith(null);
  });

  it("creates a new item from the inline row", async () => {
    const onCreate = vi.fn();
    render(
      <SelectablePopover
        ariaLabel="Client"
        trigger={<span>No client</span>}
        items={items}
        activeId={null}
        onSelect={vi.fn()}
        onCreate={onCreate}
        createLabel="New client"
        createPlaceholder="Client name"
      />,
    );
    await open();
    await userEvent.click(screen.getByRole("menuitem", { name: /New client/ }));
    const input = screen.getByPlaceholderText("Client name");
    fireEvent.change(input, { target: { value: "Initech" } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(onCreate).toHaveBeenCalledWith("Initech");
  });

  it("renames an item inline", async () => {
    const onRename = vi.fn();
    render(
      <SelectablePopover
        ariaLabel="Client"
        trigger={<span>Acme</span>}
        items={items}
        activeId="c1"
        onSelect={vi.fn()}
        onRename={onRename}
      />,
    );
    await open();
    await userEvent.click(screen.getByRole("button", { name: "Rename Acme" }));
    const input = screen.getByLabelText("Rename");
    fireEvent.change(input, { target: { value: "Acme LLC" } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(onRename).toHaveBeenCalledWith("c1", "Acme LLC");
  });

  it("deletes an item", async () => {
    const onDelete = vi.fn();
    render(
      <SelectablePopover
        ariaLabel="Client"
        trigger={<span>Acme</span>}
        items={items}
        activeId="c1"
        onSelect={vi.fn()}
        onDelete={onDelete}
      />,
    );
    await open();
    await userEvent.click(screen.getByRole("button", { name: "Delete Globex" }));
    expect(onDelete).toHaveBeenCalledWith("c2");
  });

  it("renders an optional secondary description line under a row (#62)", async () => {
    const withDesc = [
      { id: "c1", label: "Kickoff questions", description: "5m ago" },
      { id: "c2", label: "New chat", description: "yesterday" },
    ];
    render(
      <SelectablePopover
        ariaLabel="Chat history"
        trigger={<span>History</span>}
        items={withDesc}
        activeId="c1"
        onSelect={vi.fn()}
      />,
    );
    await open("Chat history");
    // Both the primary label and its relative-date secondary line render.
    expect(screen.getByRole("menuitemradio", { name: /Kickoff questions/ })).toBeInTheDocument();
    expect(screen.getByText("5m ago")).toBeInTheDocument();
    expect(screen.getByText("yesterday")).toBeInTheDocument();
  });

  it("anchors the menu's left edge to the trigger by default", async () => {
    render(
      <SelectablePopover ariaLabel="Menu" trigger={<span>Open</span>} items={items} activeId="c1" onSelect={vi.fn()} />,
    );
    await open("Menu");
    // Placement is Radix's now, so assert the alignment it resolved rather
    // than the inline left/right we used to compute by hand.
    expect(screen.getByRole("menu")).toHaveAttribute("data-align", "start");
  });

  it("anchors the menu's right edge to the trigger with align='end' (#62)", async () => {
    render(
      <SelectablePopover
        ariaLabel="Menu"
        align="end"
        trigger={<span>Open</span>}
        items={items}
        activeId="c1"
        onSelect={vi.fn()}
      />,
    );
    await open("Menu");
    // Aligned to the trigger's right edge, so it opens leftward rather than
    // past the viewport edge.
    expect(screen.getByRole("menu")).toHaveAttribute("data-align", "end");
  });

  it("restores focus to the trigger after selecting an item (#64)", async () => {
    render(
      <SelectablePopover ariaLabel="Client" trigger={<span>Acme</span>} items={items} activeId="c1" onSelect={vi.fn()} />,
    );
    const trigger = screen.getByRole("button", { name: "Client" });
    await userEvent.click(trigger);
    await userEvent.click(screen.getByRole("menuitemradio", { name: /Globex/ }));
    expect(document.activeElement).toBe(trigger);
  });

  it("restores focus to the trigger on Escape (#64)", async () => {
    render(
      <SelectablePopover ariaLabel="Client" trigger={<span>Acme</span>} items={items} activeId="c1" onSelect={vi.fn()} />,
    );
    const trigger = screen.getByRole("button", { name: "Client" });
    await userEvent.click(trigger);
    await userEvent.keyboard("{Escape}");
    expect(document.activeElement).toBe(trigger);
  });

  it("does not steal focus from a mouse click-away (#64)", async () => {
    render(
      <div>
        <button>outside</button>
        <SelectablePopover ariaLabel="Client" trigger={<span>Acme</span>} items={items} activeId="c1" onSelect={vi.fn()} />
      </div>,
    );
    await userEvent.click(screen.getByRole("button", { name: "Client" }));
    const outside = screen.getByRole("button", { name: "outside" });
    outside.focus();
    // Clicking away closes the menu but must not yank focus to the trigger.
    fireEvent.pointerDown(document.body);
    expect(screen.queryByRole("menu")).toBeNull();
    expect(document.activeElement).toBe(outside);
  });

  it("renders an optional leading icon in menu rows (#69)", async () => {
    const withIcons = [
      { id: "c1", label: "This note", icon: <span data-testid="row-icon" /> },
      { id: "c2", label: "All notes", icon: <span data-testid="row-icon" /> },
    ];
    render(
      <SelectablePopover
        ariaLabel="Chat scope"
        trigger={<span>This note</span>}
        items={withIcons}
        activeId="c1"
        onSelect={vi.fn()}
      />,
    );
    await open("Chat scope");
    expect(screen.getAllByTestId("row-icon")).toHaveLength(2);
  });

  it("picks a row with the arrow keys (#114)", async () => {
    const onSelect = vi.fn();
    render(
      <SelectablePopover ariaLabel="Client" trigger={<span>Acme</span>} items={items} activeId="c1" onSelect={onSelect} />,
    );
    await open();
    await userEvent.keyboard("{ArrowDown}{ArrowDown}{Enter}");
    expect(onSelect).toHaveBeenCalledWith("c2");
  });

  it("jumps to a row by typing (#114)", async () => {
    const onSelect = vi.fn();
    render(
      <SelectablePopover ariaLabel="Client" trigger={<span>Acme</span>} items={items} activeId="c1" onSelect={onSelect} />,
    );
    await open();
    await userEvent.keyboard("glo{Enter}");
    expect(onSelect).toHaveBeenCalledWith("c2");
  });

  it("is a plain checkmark menu when no edit callbacks are given (Scope-popover mode)", async () => {
    render(
      <SelectablePopover ariaLabel="Scope" trigger={<span>All notes</span>} items={items} activeId="c1" onSelect={vi.fn()} />,
    );
    await open("Scope");
    expect(screen.queryByRole("menuitem", { name: /New/ })).toBeNull();
    expect(screen.queryByRole("button", { name: /Rename/ })).toBeNull();
    expect(screen.queryByRole("button", { name: /Delete/ })).toBeNull();
  });

  it("Escape during a rename cancels the edit and keeps the menu open", async () => {
    render(
      <SelectablePopover ariaLabel="Client" trigger={<span>Acme</span>} items={items} activeId="c1" onSelect={vi.fn()} onRename={vi.fn()} />,
    );
    await open();
    await userEvent.click(screen.getByRole("button", { name: "Rename Acme" }));
    expect(screen.getByLabelText("Rename")).toBeInTheDocument();
    await userEvent.keyboard("{Escape}");
    expect(screen.queryByLabelText("Rename")).toBeNull();
    expect(screen.getByRole("menu")).toBeInTheDocument();
  });
});
