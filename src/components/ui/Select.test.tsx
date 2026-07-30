import { describe, it, expect, vi } from "vitest";
import { useState } from "react";
import { fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { Select } from "./Select";

describe("Select", () => {
  function Harness() {
    const [value, setValue] = useState("no");
    return (
      <Select
        value={value}
        onChange={setValue}
        options={[
          { value: "no", label: "Norwegian" },
          { value: "en", label: "English" },
        ]}
      />
    );
  }

  it("opens a styled listbox and picks an option", async () => {
    render(<Harness />);
    const trigger = screen.getByRole("combobox", { name: /norwegian/i });
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();

    await userEvent.click(trigger);
    const listbox = screen.getByRole("listbox");
    expect(
      within(listbox).getByRole("option", { name: /norwegian/i }),
    ).toHaveAttribute("aria-selected", "true");

    await userEvent.click(
      within(listbox).getByRole("option", { name: /english/i }),
    );

    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
    expect(
      screen.getByRole("combobox", { name: /english/i }),
    ).toBeInTheDocument();
  });

  it("closes on Escape and on outside click without changing the value", async () => {
    render(
      <div>
        <button>outside</button>
        <Harness />
      </div>,
    );
    const trigger = screen.getByRole("combobox", { name: /norwegian/i });

    await userEvent.click(trigger);
    expect(screen.getByRole("listbox")).toBeInTheDocument();
    await userEvent.keyboard("{Escape}");
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();

    await userEvent.click(trigger);
    expect(screen.getByRole("listbox")).toBeInTheDocument();
    // An open listbox is modal — the page behind it is inert and aria-hidden,
    // so dismissal comes from pointing outside rather than from activating
    // whatever is under the pointer.
    fireEvent.pointerDown(document.body);
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();

    expect(
      screen.getByRole("combobox", { name: /norwegian/i }),
    ).toBeInTheDocument();
  });

  it("picks an option with the arrow keys (#114)", async () => {
    render(<Harness />);

    await userEvent.click(screen.getByRole("combobox", { name: /norwegian/i }));
    await userEvent.keyboard("{ArrowDown}{Enter}");

    // Every picker in the app was mouse-or-nothing before Radix.
    expect(screen.getByRole("combobox", { name: /english/i })).toBeInTheDocument();
  });

  it("supports a placeholder row whose value is the empty string", async () => {
    // Radix reserves "" for "nothing chosen" and throws on an empty item value;
    // OllamaConnect and the Chat tab both offer a "Choose a…" row like this.
    const onChange = vi.fn();
    render(
      <Select
        value=""
        onChange={onChange}
        options={[
          { value: "", label: "Choose a model…" },
          { value: "qwen3:8b", label: "qwen3:8b" },
        ]}
      />,
    );

    await userEvent.click(screen.getByRole("combobox", { name: /choose a model/i }));
    await userEvent.click(screen.getByRole("option", { name: "qwen3:8b" }));
    expect(onChange).toHaveBeenCalledWith("qwen3:8b");
  });

  it("portals the listbox to <body>, escaping an overflow-clipping ancestor", async () => {
    // Mirrors ImportDialog's placement: the shared Modal's panel is
    // `overflow-y-auto`, which clips absolutely-positioned in-flow children.
    // The listbox must render outside that subtree (a body-level portal),
    // not as a descendant of the clipping container.
    const { container } = render(
      <div data-testid="clipper" style={{ overflow: "hidden" }}>
        <Harness />
      </div>,
    );
    const clipper = container.querySelector('[data-testid="clipper"]')!;
    const trigger = screen.getByRole("combobox", { name: /norwegian/i });

    await userEvent.click(trigger);
    const listbox = screen.getByRole("listbox");

    expect(clipper.contains(listbox)).toBe(false);
    expect(document.body.contains(listbox)).toBe(true);
  });
});
