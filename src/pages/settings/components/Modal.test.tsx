import { describe, it, expect, vi } from "vitest";
import { useState } from "react";
import { render, screen, fireEvent } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { Modal } from "./Modal";

// Harness: a trigger button that opens the modal, so we can assert focus
// returns to the trigger on close.
function Harness({ onClose }: { onClose?: () => void }) {
  const [open, setOpen] = useState(false);
  return (
    <div>
      <button onClick={() => setOpen(true)}>Open</button>
      <Modal
        open={open}
        onClose={() => {
          setOpen(false);
          onClose?.();
        }}
        title="Test dialog"
      >
        <button>First</button>
        <button>Second</button>
        <button>Third</button>
      </Modal>
    </div>
  );
}

describe("Modal focus management", () => {
  it("moves focus to the first focusable element on open", async () => {
    const user = userEvent.setup();
    render(<Harness />);

    await user.click(screen.getByRole("button", { name: "Open" }));

    const dialog = screen.getByRole("dialog", { name: "Test dialog" });
    expect(dialog).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "First" })).toHaveFocus();
  });

  it("traps Tab within the dialog (wraps last -> first)", async () => {
    const user = userEvent.setup();
    render(<Harness />);
    await user.click(screen.getByRole("button", { name: "Open" }));

    const first = screen.getByRole("button", { name: "First" });
    const third = screen.getByRole("button", { name: "Third" });

    // Land on the last focusable, then Tab: focus must wrap back to the first,
    // never onto the "Open" trigger behind the dimmed overlay.
    third.focus();
    expect(third).toHaveFocus();
    await user.tab();
    expect(first).toHaveFocus();
  });

  it("traps Shift-Tab within the dialog (wraps first -> last)", async () => {
    const user = userEvent.setup();
    render(<Harness />);
    await user.click(screen.getByRole("button", { name: "Open" }));

    const first = screen.getByRole("button", { name: "First" });
    const third = screen.getByRole("button", { name: "Third" });

    first.focus();
    await user.tab({ shift: true });
    expect(third).toHaveFocus();
  });

  it("restores focus to the trigger on close", async () => {
    const user = userEvent.setup();
    render(<Harness />);
    const trigger = screen.getByRole("button", { name: "Open" });
    await user.click(trigger);
    expect(screen.getByRole("button", { name: "First" })).toHaveFocus();

    // Esc closes; focus returns to the element that opened the modal.
    fireEvent.keyDown(document.activeElement ?? document.body, { key: "Escape" });
    expect(trigger).toHaveFocus();
  });

  it("calls onClose on Escape", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    render(<Harness onClose={onClose} />);
    await user.click(screen.getByRole("button", { name: "Open" }));

    fireEvent.keyDown(document.body, { key: "Escape" });
    expect(onClose).toHaveBeenCalled();
  });
});
