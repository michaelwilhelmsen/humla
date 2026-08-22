import { describe, it, expect, beforeAll, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { TranscriptEditor } from "./Note";
import { mockLayoutBox } from "../test/layout";

// TranscriptView virtualizes its lines via @tanstack/react-virtual, which reads
// a zero-sized box in jsdom and renders no rows at all — nothing to click.
// (The ResizeObserver the virtualizer also needs is already shimmed globally
// in src/test/setup.ts.)
beforeAll(() => mockLayoutBox());

function renderEditor(over?: { disabled?: boolean; onChange?: (v: string) => void }) {
  return render(
    <TranscriptEditor
      value={"Speaker 1: hello\nSpeaker 2: world"}
      onChange={over?.onChange ?? vi.fn()}
      disabled={over?.disabled ?? false}
      bottomAligned={false}
    />,
  );
}

const textarea = () => screen.queryByRole("textbox");
const header = () => screen.getByTestId("transcript-mode-header");
const liveRegion = () => screen.getByTestId("transcript-mode-live");

describe("TranscriptEditor edit mode (#168)", () => {
  it("shows an explicit Edit control in view mode and enters edit mode from it", () => {
    renderEditor();
    expect(textarea()).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Edit" }));

    expect(textarea()).not.toBeNull();
  });

  it("swaps the header for an Editing indicator and a Done control", () => {
    renderEditor();
    fireEvent.click(screen.getByRole("button", { name: "Edit" }));

    expect(screen.getByText("Editing")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Done" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Edit" })).toBeNull();
  });

  it("leaves edit mode when Done is clicked", () => {
    renderEditor();
    fireEvent.click(screen.getByRole("button", { name: "Edit" }));

    fireEvent.click(screen.getByRole("button", { name: "Done" }));

    expect(textarea()).toBeNull();
    expect(screen.getByRole("button", { name: "Edit" })).toBeTruthy();
  });

  it("prevents the default mousedown on Done so blur can't unmount it first", () => {
    // onBlur exits edit mode, and blur fires before the click would land — so
    // without preventDefault the Done button is gone by the time it is clicked.
    renderEditor();
    fireEvent.click(screen.getByRole("button", { name: "Edit" }));

    const notPrevented = fireEvent.mouseDown(screen.getByRole("button", { name: "Done" }));

    expect(notPrevented).toBe(false);
  });

  it("gives the textarea a visible border while editing", () => {
    renderEditor();
    fireEvent.click(screen.getByRole("button", { name: "Edit" }));

    // The border comes from the unlayered `textarea` rule in globals.css
    // (1px border + a focus border-colour shift). `.nd-bare` strips it with
    // `border: none !important` and beats any Tailwind utility, so keeping
    // that class off is the whole of the fix — jsdom loads no CSS, so the
    // absence of the opt-out is what's assertable here.
    const el = textarea() as HTMLTextAreaElement;
    expect(el.className).not.toContain("nd-bare");
    expect(el.className).not.toContain("focus:outline-none");
  });

  it("keeps Escape as a shortcut out of edit mode", () => {
    renderEditor();
    fireEvent.click(screen.getByRole("button", { name: "Edit" }));

    fireEvent.keyDown(textarea() as HTMLTextAreaElement, { key: "Escape" });

    expect(textarea()).toBeNull();
  });

  it("keeps click-to-edit on the transcript body", () => {
    renderEditor();

    fireEvent.click(screen.getByText("hello"));

    expect(textarea()).not.toBeNull();
  });

  // `disabled` covers both a recording in flight and a teammate's read-only
  // note — neither has a mode to enter, so neither gets the affordance.
  it("offers no edit affordance when editing is disabled", () => {
    renderEditor({ disabled: true });

    expect(screen.queryByRole("button", { name: "Edit" })).toBeNull();
    fireEvent.click(screen.getByText("hello"));
    expect(textarea()).toBeNull();
  });
});

describe("TranscriptEditor header slot + mode announcement (#171)", () => {
  // jsdom measures nothing — every element is 0×0 — so the reserved height
  // itself is unassertable here. What IS assertable is the structure that
  // produces it: the same header row renders in both states, holding a
  // placeholder that carries the control's own text (and so its line box)
  // when there is no control to show. The pixel check is the mock harness's
  // job (`?case=transcript` vs `?case=transcript-recording`).
  it("keeps the header slot in the tree while editing is disabled", () => {
    renderEditor({ disabled: true });

    expect(header()).toBeTruthy();
    expect(header().textContent).toBe("Edit");
  });

  it("hides the disabled placeholder from assistive tech and offers no control", () => {
    renderEditor({ disabled: true });

    expect(header().querySelector("button")).toBeNull();
    expect(header().firstElementChild?.getAttribute("aria-hidden")).toBe("true");
  });

  it("announces nothing on first render, in either mode", () => {
    const { unmount } = renderEditor();
    expect(liveRegion().textContent).toBe("");
    unmount();

    renderEditor({ disabled: true });
    expect(liveRegion().textContent).toBe("");
  });

  it("uses a polite live region that stays mounted across the mode swap", () => {
    renderEditor();
    expect(liveRegion()).toHaveAttribute("aria-live", "polite");

    const before = liveRegion();
    fireEvent.click(screen.getByRole("button", { name: "Edit" }));

    expect(liveRegion()).toBe(before);
  });

  it("announces entering edit mode", () => {
    renderEditor();

    fireEvent.click(screen.getByRole("button", { name: "Edit" }));

    expect(liveRegion().textContent).toBe("Editing transcript");
  });

  it.each([
    ["Done", () => fireEvent.click(screen.getByRole("button", { name: "Done" }))],
    ["Escape", () => fireEvent.keyDown(textarea() as HTMLTextAreaElement, { key: "Escape" })],
    ["clicking away", () => fireEvent.blur(textarea() as HTMLTextAreaElement)],
  ])("announces leaving edit mode via %s", (_route, leave) => {
    renderEditor();
    fireEvent.click(screen.getByRole("button", { name: "Edit" }));

    leave();

    expect(liveRegion().textContent).toBe("Transcript no longer editable");
  });

  it("gives the textarea an accessible name", () => {
    renderEditor();
    fireEvent.click(screen.getByRole("button", { name: "Edit" }));

    expect(screen.getByRole("textbox", { name: "Transcript" })).toBeTruthy();
  });
});

// #176. The timeline-backed reader titles each turn with its speaker; this one
// — the reader a note falls to when it has no local timeline, which is mostly a
// teammate's synced note — carried the same defect the title fixed: it stripped
// the `Speaker 1: ` prefix off the line and put a coloured dot in its place, so
// the name appeared nowhere at all. The two must not disagree about how a
// transcript looks depending on which machine opens the note.
describe("the styled fallback reader titles its turns (#176)", () => {
  function renderView(value: string) {
    return render(
      <TranscriptEditor value={value} onChange={vi.fn()} disabled={false} bottomAligned={false} />,
    );
  }

  it("names the speaker above the turn rather than dropping the prefix", () => {
    renderView("Hege: jeg har notatene klare\nYou: bra");
    expect(screen.getByText("Hege")).toBeInTheDocument();
    expect(screen.getByText("You")).toBeInTheDocument();
    // The name is a title, so it is no longer part of the spoken line.
    expect(screen.getByText("jeg har notatene klare")).toBeInTheDocument();
  });

  it("titles a run of consecutive lines from one speaker once", () => {
    renderView("Hege: første linje\nHege: andre linje\nYou: svar");
    expect(screen.getAllByText("Hege")).toHaveLength(1);
  });

  it("does not announce the speaker twice — the dot beside the name is decorative", () => {
    renderView("Hege: jeg har notatene klare");
    expect(screen.getByText("Hege")).toBeInTheDocument();
    expect(screen.queryByLabelText("Speaker: Hege")).toBeNull();
  });

  it("leaves a line with no speaker prefix alone", () => {
    const { container } = renderView("bare tekst uten etikett");
    expect(screen.getByText("bare tekst uten etikett")).toBeInTheDocument();
    expect(container.querySelector("[data-turn-title]")).toBeNull();
  });
});
