import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { SelectablePopover } from "./SelectablePopover";

const items = [
  { id: "c1", label: "Acme" },
  { id: "c2", label: "Globex" },
];

function open(ariaLabel = "Client") {
  fireEvent.click(screen.getByRole("button", { name: ariaLabel }));
}

describe("SelectablePopover", () => {
  it("marks the active item and selects another on click", () => {
    const onSelect = vi.fn();
    render(
      <SelectablePopover ariaLabel="Client" trigger={<span>Acme</span>} items={items} activeId="c1" onSelect={onSelect} />,
    );
    open();
    // Active row is checked.
    expect(screen.getByRole("menuitemradio", { name: /Acme/ })).toHaveAttribute("aria-checked", "true");
    expect(screen.getByRole("menuitemradio", { name: /Globex/ })).toHaveAttribute("aria-checked", "false");
    fireEvent.click(screen.getByRole("menuitemradio", { name: /Globex/ }));
    expect(onSelect).toHaveBeenCalledWith("c2");
  });

  it("unassigns via the none row", () => {
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
    open();
    fireEvent.click(screen.getByRole("menuitemradio", { name: "No client" }));
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
    open();
    fireEvent.click(screen.getByRole("menuitem", { name: /New client/ }));
    const input = screen.getByPlaceholderText("Client name");
    fireEvent.change(input, { target: { value: "Initech" } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(onCreate).toHaveBeenCalledWith("Initech");
  });

  it("renames an item inline", () => {
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
    open();
    fireEvent.click(screen.getByRole("button", { name: "Rename Acme" }));
    const input = screen.getByLabelText("Rename");
    fireEvent.change(input, { target: { value: "Acme LLC" } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(onRename).toHaveBeenCalledWith("c1", "Acme LLC");
  });

  it("deletes an item", () => {
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
    open();
    fireEvent.click(screen.getByRole("button", { name: "Delete Globex" }));
    expect(onDelete).toHaveBeenCalledWith("c2");
  });

  it("renders an optional secondary description line under a row (#62)", () => {
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
    open("Chat history");
    // Both the primary label and its relative-date secondary line render.
    expect(screen.getByRole("menuitemradio", { name: /Kickoff questions/ })).toBeInTheDocument();
    expect(screen.getByText("5m ago")).toBeInTheDocument();
    expect(screen.getByText("yesterday")).toBeInTheDocument();
  });

  it("anchors the menu's left edge to the trigger by default", () => {
    render(
      <SelectablePopover ariaLabel="Menu" trigger={<span>Open</span>} items={items} activeId="c1" onSelect={vi.fn()} />,
    );
    open("Menu");
    const menu = screen.getByRole("menu");
    // left is pinned (0px in jsdom's zero-rect), right is left unset.
    expect(menu.style.left).toBe("0px");
    expect(menu.style.right).toBe("");
  });

  it("anchors the menu's right edge to the trigger with align='end' (#62)", () => {
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
    open("Menu");
    const menu = screen.getByRole("menu");
    // right is pinned, left is left unset — so it opens leftward, not past the edge.
    expect(menu.style.right).not.toBe("");
    expect(menu.style.left).toBe("");
  });

  it("restores focus to the trigger after selecting an item (#64)", () => {
    render(
      <SelectablePopover ariaLabel="Client" trigger={<span>Acme</span>} items={items} activeId="c1" onSelect={vi.fn()} />,
    );
    const trigger = screen.getByRole("button", { name: "Client" });
    fireEvent.click(trigger);
    fireEvent.click(screen.getByRole("menuitemradio", { name: /Globex/ }));
    expect(document.activeElement).toBe(trigger);
  });

  it("restores focus to the trigger on Escape (#64)", () => {
    render(
      <SelectablePopover ariaLabel="Client" trigger={<span>Acme</span>} items={items} activeId="c1" onSelect={vi.fn()} />,
    );
    const trigger = screen.getByRole("button", { name: "Client" });
    fireEvent.click(trigger);
    fireEvent.keyDown(document, { key: "Escape" });
    expect(document.activeElement).toBe(trigger);
  });

  it("does not steal focus from a mouse click-away (#64)", () => {
    render(
      <div>
        <button>outside</button>
        <SelectablePopover ariaLabel="Client" trigger={<span>Acme</span>} items={items} activeId="c1" onSelect={vi.fn()} />
      </div>,
    );
    fireEvent.click(screen.getByRole("button", { name: "Client" }));
    const outside = screen.getByRole("button", { name: "outside" });
    outside.focus();
    // Clicking away closes the menu but must not yank focus to the trigger.
    fireEvent.mouseDown(document.body);
    expect(document.activeElement).toBe(outside);
  });

  it("is a plain checkmark menu when no edit callbacks are given (Scope-popover mode)", () => {
    render(
      <SelectablePopover ariaLabel="Scope" trigger={<span>All notes</span>} items={items} activeId="c1" onSelect={vi.fn()} />,
    );
    open("Scope");
    expect(screen.queryByRole("menuitem", { name: /New/ })).toBeNull();
    expect(screen.queryByRole("button", { name: /Rename/ })).toBeNull();
    expect(screen.queryByRole("button", { name: /Delete/ })).toBeNull();
  });
});
