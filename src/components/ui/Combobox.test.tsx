import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { Combobox, type ComboboxOption } from "./Combobox";

// The filtered-listbox primitive (#116). Radix has no combobox — `Select` takes
// no typed input and `Menu` runs its own typeahead over the content, which would
// eat the keystrokes as you type a name. So this is an input plus a listbox on
// `Popover`, owning its own key handling.
//
// The load-bearing rule: Enter commits exactly what was typed unless a row is
// highlighted. A new person must never be harder to enter than an existing one.

const OPTIONS: ComboboxOption[] = [
  { value: "Hege Tronshaugen", label: "Hege Tronshaugen" },
  { value: "Michael", label: "Michael" },
  { value: "Åse Berg", label: "Åse Berg" },
];

function Harness({
  options = OPTIONS,
  initial = "",
  preselect,
  onCommit = vi.fn(),
  onCancel = vi.fn(),
}: {
  options?: ComboboxOption[];
  initial?: string;
  preselect?: string;
  onCommit?: (v: string) => void;
  onCancel?: () => void;
}) {
  const [value, setValue] = useState(initial);
  return (
    <Combobox
      value={value}
      onValueChange={setValue}
      options={options}
      preselect={preselect}
      onCommit={onCommit}
      onCancel={onCancel}
      aria-label="Speaker name"
      listLabel="Speaker names"
    />
  );
}

const input = () => screen.getByRole("combobox", { name: "Speaker name" });

describe("Combobox typing", () => {
  it("reports what the user types", async () => {
    const user = userEvent.setup();
    render(<Harness />);
    await user.type(input(), "Hege");
    expect(input()).toHaveValue("Hege");
  });

  it("lists the options it was given", async () => {
    const user = userEvent.setup();
    render(<Harness />);
    await user.click(input());
    expect(screen.getAllByRole("option").map((o) => o.textContent)).toEqual([
      "Hege Tronshaugen",
      "Michael",
      "Åse Berg",
    ]);
  });

  it("shows no listbox when there is nothing to suggest", async () => {
    const user = userEvent.setup();
    render(<Harness options={[]} />);
    await user.click(input());
    expect(screen.queryByRole("listbox")).toBeNull();
  });
});

describe("Combobox commit", () => {
  it("commits exactly what was typed when no row is highlighted", async () => {
    const user = userEvent.setup();
    const onCommit = vi.fn();
    render(<Harness onCommit={onCommit} />);

    // "Hege" is a prefix of a suggestion, and Enter still writes "Hege" —
    // silently expanding it would override a name meant literally.
    await user.type(input(), "Hege");
    await user.keyboard("{Enter}");
    expect(onCommit).toHaveBeenCalledExactlyOnceWith("Hege");
  });

  it("commits the highlighted row after ArrowDown", async () => {
    const user = userEvent.setup();
    const onCommit = vi.fn();
    render(<Harness onCommit={onCommit} />);

    await user.click(input());
    await user.keyboard("{ArrowDown}{Enter}");
    expect(onCommit).toHaveBeenCalledExactlyOnceWith("Hege Tronshaugen");
  });

  it("commits a clicked row", async () => {
    const user = userEvent.setup();
    const onCommit = vi.fn();
    render(<Harness initial="Mich" onCommit={onCommit} />);

    await user.click(input());
    await user.click(screen.getByRole("option", { name: "Michael" }));
    // Exactly once, with the row's value — the pointer-down must not blur the
    // input into committing the half-typed "Mich" first.
    expect(onCommit).toHaveBeenCalledExactlyOnceWith("Michael");
  });

  it("commits the typed text on blur, so clicking away is not a silent discard", async () => {
    const user = userEvent.setup();
    const onCommit = vi.fn();
    render(
      <>
        <Harness onCommit={onCommit} />
        <button type="button">elsewhere</button>
      </>,
    );
    await user.type(input(), "Anna");
    await user.click(screen.getByRole("button", { name: "elsewhere" }));
    expect(onCommit).toHaveBeenCalledExactlyOnceWith("Anna");
  });
});

describe("Combobox keyboard navigation", () => {
  it("nothing is highlighted before the first arrow key", async () => {
    const user = userEvent.setup();
    render(<Harness />);
    await user.click(input());
    expect(input()).not.toHaveAttribute("aria-activedescendant");
    expect(screen.queryByRole("option", { selected: true })).toBeNull();
  });

  it("moves the highlight down and up, wrapping", async () => {
    const user = userEvent.setup();
    render(<Harness />);
    await user.click(input());

    const selected = () => screen.getByRole("option", { selected: true }).textContent;
    await user.keyboard("{ArrowDown}");
    expect(selected()).toBe("Hege Tronshaugen");
    await user.keyboard("{ArrowDown}{ArrowDown}");
    expect(selected()).toBe("Åse Berg");
    await user.keyboard("{ArrowDown}"); // wraps
    expect(selected()).toBe("Hege Tronshaugen");
    await user.keyboard("{ArrowUp}"); // wraps to last
    expect(selected()).toBe("Åse Berg");
  });

  it("points aria-activedescendant at the highlighted row", async () => {
    const user = userEvent.setup();
    render(<Harness />);
    await user.click(input());
    await user.keyboard("{ArrowDown}");
    const row = screen.getByRole("option", { selected: true });
    expect(input()).toHaveAttribute("aria-activedescendant", row.id);
  });

  it("keeps focus in the input while navigating", async () => {
    const user = userEvent.setup();
    render(<Harness />);
    await user.click(input());
    await user.keyboard("{ArrowDown}");
    // Focus never leaves the input — that is what lets you keep typing after
    // arrowing, and what an active-descendant listbox requires.
    expect(input()).toHaveFocus();
  });

  it("drops the highlight when the query changes", async () => {
    const user = userEvent.setup();
    const onCommit = vi.fn();
    render(<Harness onCommit={onCommit} />);
    await user.click(input());
    await user.keyboard("{ArrowDown}");
    // Typing after arrowing must not leave a stale row armed for Enter.
    await user.type(input(), "A");
    await user.keyboard("{Enter}");
    expect(onCommit).toHaveBeenCalledExactlyOnceWith("A");
  });

  it("Escape cancels without committing", async () => {
    const user = userEvent.setup();
    const onCommit = vi.fn();
    const onCancel = vi.fn();
    render(<Harness onCommit={onCommit} onCancel={onCancel} />);
    await user.type(input(), "Anna");
    await user.keyboard("{Escape}");
    expect(onCancel).toHaveBeenCalled();
    expect(onCommit).not.toHaveBeenCalled();
  });
});

describe("Combobox preselect", () => {
  it("starts with the preselected row highlighted, so Enter takes it", async () => {
    const user = userEvent.setup();
    const onCommit = vi.fn();
    render(<Harness initial="åse berg" preselect="Åse Berg" onCommit={onCommit} />);
    await user.click(input());
    expect(screen.getByRole("option", { selected: true }).textContent).toBe("Åse Berg");
    await user.keyboard("{Enter}");
    expect(onCommit).toHaveBeenCalledExactlyOnceWith("Åse Berg");
  });
});
