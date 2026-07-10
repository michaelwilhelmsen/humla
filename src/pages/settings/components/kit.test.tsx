import { describe, it, expect } from "vitest";
import { useState } from "react";
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { Toggle } from "./Toggle";
import { Disclosure } from "./Disclosure";
import { Segmented } from "./Segmented";
import { Select } from "./Select";

describe("Toggle", () => {
  function Harness({ initial = false }: { initial?: boolean }) {
    const [on, setOn] = useState(initial);
    return <Toggle label="Keep recorded audio" checked={on} onChange={setOn} />;
  }

  it("is an accessible switch that flips on click", async () => {
    render(<Harness />);
    const sw = screen.getByRole("switch", { name: /keep recorded audio/i });
    expect(sw).not.toBeChecked();

    await userEvent.click(sw);
    expect(sw).toBeChecked();

    await userEvent.click(sw);
    expect(sw).not.toBeChecked();
  });
});

describe("Disclosure", () => {
  it("hides expert content until expanded, then collapses again", async () => {
    render(
      <Disclosure label="Advanced">
        <div>Clustering threshold</div>
      </Disclosure>,
    );
    const trigger = screen.getByRole("button", { name: /advanced/i });
    expect(trigger).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByText("Clustering threshold")).not.toBeInTheDocument();

    await userEvent.click(trigger);
    expect(trigger).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByText("Clustering threshold")).toBeInTheDocument();

    await userEvent.click(trigger);
    expect(screen.queryByText("Clustering threshold")).not.toBeInTheDocument();
  });
});

describe("Segmented", () => {
  function Harness() {
    const [value, setValue] = useState("system");
    return (
      <Segmented
        label="Theme"
        value={value}
        onChange={setValue}
        options={[
          { value: "system", label: "System" },
          { value: "light", label: "Light" },
          { value: "dark", label: "Dark" },
        ]}
      />
    );
  }

  it("selects exactly one option on click", async () => {
    render(<Harness />);
    const group = screen.getByRole("radiogroup", { name: /theme/i });
    const system = screen.getByRole("radio", { name: /system/i });
    const dark = screen.getByRole("radio", { name: /dark/i });
    expect(group).toBeInTheDocument();
    expect(system).toBeChecked();
    expect(dark).not.toBeChecked();

    await userEvent.click(dark);
    expect(dark).toBeChecked();
    expect(system).not.toBeChecked();
  });
});

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
    const trigger = screen.getByRole("button", { name: /norwegian/i });
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
      screen.getByRole("button", { name: /english/i }),
    ).toBeInTheDocument();
  });

  it("closes on Escape and on outside click without changing the value", async () => {
    render(
      <div>
        <button>outside</button>
        <Harness />
      </div>,
    );
    const trigger = screen.getByRole("button", { name: /norwegian/i });

    await userEvent.click(trigger);
    expect(screen.getByRole("listbox")).toBeInTheDocument();
    await userEvent.keyboard("{Escape}");
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();

    await userEvent.click(trigger);
    expect(screen.getByRole("listbox")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "outside" }));
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();

    expect(
      screen.getByRole("button", { name: /norwegian/i }),
    ).toBeInTheDocument();
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
    const trigger = screen.getByRole("button", { name: /norwegian/i });

    await userEvent.click(trigger);
    const listbox = screen.getByRole("listbox");

    expect(clipper.contains(listbox)).toBe(false);
    expect(document.body.contains(listbox)).toBe(true);
  });
});
