import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { SpeakerLabels } from "./SpeakerLabels";
import { renameSpeakerInTranscript } from "../lib/speakers";

// The speaker chip strip drives both rename and merge. Under the hood a
// merge IS a rename whose target is an already-existing label (#23), so
// both actions surface through the single `onRename(oldLabel, newLabel)`
// callback the parent wires to the transcript rewrite + timeline rename.

const TWO_SPEAKERS = "Speaker 1: hello there\nSpeaker 2: hi back\nSpeaker 1: bye";
const THREE_SPEAKERS =
  "Speaker 1: a\nSpeaker 2: b\nSpeaker 3: c\nSpeaker 1: d";

describe("SpeakerLabels strip visibility", () => {
  it("renders nothing for a single speaker", () => {
    const { container } = render(
      <SpeakerLabels transcript="Speaker 1: solo monologue" onRename={vi.fn()} />,
    );
    expect(container).toBeEmptyDOMElement();
  });

  it("renders nothing for a transcript with no speaker prefixes", () => {
    const { container } = render(
      <SpeakerLabels transcript="just some text, no labels" onRename={vi.fn()} />,
    );
    expect(container).toBeEmptyDOMElement();
  });

  it("renders a chip per unique speaker when there are 2+", () => {
    render(<SpeakerLabels transcript={TWO_SPEAKERS} onRename={vi.fn()} />);
    expect(screen.getByRole("button", { name: /^Speaker 1$/ })).toBeTruthy();
    expect(screen.getByRole("button", { name: /^Speaker 2$/ })).toBeTruthy();
  });
});

describe("SpeakerLabels merge flow", () => {
  it("offers a merge affordance on each chip", () => {
    render(<SpeakerLabels transcript={TWO_SPEAKERS} onRename={vi.fn()} />);
    expect(
      screen.getByRole("button", { name: /merge speaker 1 into another/i }),
    ).toBeTruthy();
    expect(
      screen.getByRole("button", { name: /merge speaker 2 into another/i }),
    ).toBeTruthy();
  });

  it("lists only the OTHER labels as merge targets", async () => {
    const user = userEvent.setup();
    render(<SpeakerLabels transcript={THREE_SPEAKERS} onRename={vi.fn()} />);

    await user.click(
      screen.getByRole("button", { name: /merge speaker 1 into another/i }),
    );

    const menu = screen.getByRole("menu");
    const items = screen.getAllByRole("menuitem");
    // Speaker 1 is the source — targets are Speaker 2 and Speaker 3 only.
    expect(items).toHaveLength(2);
    expect(menu.textContent).toContain("Speaker 2");
    expect(menu.textContent).toContain("Speaker 3");
    expect(items.some((i) => i.textContent?.includes("Speaker 1"))).toBe(false);
  });

  it("calls onRename(source, target) when a merge target is chosen", async () => {
    const user = userEvent.setup();
    const onRename = vi.fn();
    render(<SpeakerLabels transcript={TWO_SPEAKERS} onRename={onRename} />);

    await user.click(
      screen.getByRole("button", { name: /merge speaker 1 into another/i }),
    );
    await user.click(
      screen.getByRole("menuitem", { name: /merge speaker 1 into speaker 2/i }),
    );

    expect(onRename).toHaveBeenCalledTimes(1);
    expect(onRename).toHaveBeenCalledWith("Speaker 1", "Speaker 2");
    // And that call, applied to the transcript, actually collapses the two
    // labels — the transcript-rewrite half of a merge.
    const [oldLabel, newLabel] = onRename.mock.calls[0];
    const merged = renameSpeakerInTranscript(TWO_SPEAKERS, oldLabel, newLabel);
    expect(merged).not.toContain("Speaker 1:");
    expect(merged).toContain("Speaker 2:");
  });

  it("closes the merge menu on Escape without merging", async () => {
    const user = userEvent.setup();
    const onRename = vi.fn();
    render(<SpeakerLabels transcript={TWO_SPEAKERS} onRename={onRename} />);

    await user.click(
      screen.getByRole("button", { name: /merge speaker 1 into another/i }),
    );
    expect(screen.queryByRole("menu")).toBeTruthy();
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("menu")).toBeNull();
    expect(onRename).not.toHaveBeenCalled();
  });

  it("drops the merged-away chip and hides the strip once one speaker remains", () => {
    const onRename = vi.fn();
    const { container, rerender } = render(
      <SpeakerLabels transcript={TWO_SPEAKERS} onRename={onRename} />,
    );
    expect(screen.getByRole("button", { name: /^Speaker 1$/ })).toBeTruthy();

    // Simulate the parent applying the merge (Speaker 1 -> Speaker 2) and
    // re-rendering with the rewritten transcript.
    const merged = renameSpeakerInTranscript(TWO_SPEAKERS, "Speaker 1", "Speaker 2");
    rerender(<SpeakerLabels transcript={merged} onRename={onRename} />);

    // Only one unique label remains -> the whole strip hides.
    expect(container).toBeEmptyDOMElement();
  });
});

describe("SpeakerLabels rename still works", () => {
  it("calls onRename(oldLabel, typedName) after inline edit", async () => {
    const user = userEvent.setup();
    const onRename = vi.fn();
    render(<SpeakerLabels transcript={TWO_SPEAKERS} onRename={onRename} />);

    await user.click(screen.getByRole("button", { name: /^Speaker 1$/ }));
    const input = screen.getByDisplayValue("Speaker 1");
    await user.clear(input);
    await user.type(input, "Michael");
    await user.keyboard("{Enter}");

    expect(onRename).toHaveBeenCalledWith("Speaker 1", "Michael");
  });

  it("still offers merge after a rename reduces label to a custom name", async () => {
    const user = userEvent.setup();
    const onRename = vi.fn();
    // Simulate the merged result: Speaker 1 renamed to Michael, still 2 labels.
    render(
      <SpeakerLabels
        transcript={"Michael: hi\nSpeaker 2: hello"}
        onRename={onRename}
      />,
    );
    await user.click(
      screen.getByRole("button", { name: /merge michael into another/i }),
    );
    await user.click(
      screen.getByRole("menuitem", { name: /merge michael into speaker 2/i }),
    );
    expect(onRename).toHaveBeenCalledWith("Michael", "Speaker 2");
  });
});

describe("SpeakerLabels read-only", () => {
  it("renders static pills with no rename or merge affordance", () => {
    render(<SpeakerLabels transcript={TWO_SPEAKERS} onRename={vi.fn()} readOnly />);
    // No interactive buttons at all in read-only mode.
    expect(screen.queryByRole("button")).toBeNull();
    expect(screen.getByText("Speaker 1")).toBeTruthy();
    expect(screen.getByText("Speaker 2")).toBeTruthy();
  });
});
