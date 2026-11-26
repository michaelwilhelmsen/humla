import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { SpeakerLabels } from "./SpeakerLabels";
import { renameSpeakerInTranscript } from "../lib/speakers";

// The speaker names drive both rename and merge. Under the hood a
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
  it("offers the note's other speakers as merges when a name is opened", async () => {
    const user = userEvent.setup();
    render(<SpeakerLabels transcript={THREE_SPEAKERS} onRename={vi.fn()} />);

    await user.click(screen.getByRole("button", { name: /^Speaker 1$/ }));

    // Speaker 1 is the source — targets are Speaker 2 and Speaker 3 only.
    const options = screen.getAllByRole("option");
    expect(options.map((o) => o.getAttribute("aria-label"))).toEqual(["Speaker 2", "Speaker 3"]);
    expect(options.every((o) => /merge/i.test(o.textContent ?? ""))).toBe(true);
  });

  it("narrows the merges to what is typed", async () => {
    const user = userEvent.setup();
    render(<SpeakerLabels transcript={THREE_SPEAKERS} onRename={vi.fn()} />);

    await user.click(screen.getByRole("button", { name: /^Speaker 1$/ }));
    await user.clear(screen.getByRole("combobox"));
    await user.type(screen.getByRole("combobox"), "3");

    expect(screen.getAllByRole("option").map((o) => o.getAttribute("aria-label"))).toEqual(["Speaker 3"]);
  });

  it("calls onRename(source, target) when a merge is chosen", async () => {
    const user = userEvent.setup();
    const onRename = vi.fn();
    render(<SpeakerLabels transcript={TWO_SPEAKERS} onRename={onRename} />);

    await user.click(screen.getByRole("button", { name: /^Speaker 1$/ }));
    await user.click(screen.getByRole("option", { name: "Speaker 2" }));

    expect(onRename).toHaveBeenCalledTimes(1);
    expect(onRename).toHaveBeenCalledWith("Speaker 1", "Speaker 2");
    // And that call, applied to the transcript, actually collapses the two
    // labels — the transcript-rewrite half of a merge.
    const [oldLabel, newLabel] = onRename.mock.calls[0];
    const merged = renameSpeakerInTranscript(TWO_SPEAKERS, oldLabel, newLabel);
    expect(merged).not.toContain("Speaker 1:");
    expect(merged).toContain("Speaker 2:");
  });

  it("merges with the arrow keys", async () => {
    const user = userEvent.setup();
    const onRename = vi.fn();
    render(<SpeakerLabels transcript={THREE_SPEAKERS} onRename={onRename} />);

    await user.click(screen.getByRole("button", { name: /^Speaker 1$/ }));
    await user.keyboard("{ArrowDown}{ArrowDown}{Enter}");

    expect(onRename).toHaveBeenCalledWith("Speaker 1", "Speaker 3");
  });

  it("merges on this note alone, without asking about other notes", async () => {
    const user = userEvent.setup();
    const onRename = vi.fn();
    const onRenameEverywhere = vi.fn();
    render(
      <SpeakerLabels
        transcript={"Hege: a\nMichael: b"}
        onRename={onRename}
        onRenameEverywhere={onRenameEverywhere}
        otherNotesWithLabel={{ Hege: 3 }}
      />,
    );

    await user.click(screen.getByRole("button", { name: /^Hege$/ }));
    await user.click(screen.getByRole("option", { name: "Michael" }));

    expect(onRename).toHaveBeenCalledWith("Hege", "Michael");
    expect(onRenameEverywhere).not.toHaveBeenCalled();
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("leaves the speakers as they were on Escape", async () => {
    const user = userEvent.setup();
    const onRename = vi.fn();
    render(<SpeakerLabels transcript={TWO_SPEAKERS} onRename={onRename} />);

    await user.click(screen.getByRole("button", { name: /^Speaker 1$/ }));
    expect(screen.getByRole("listbox")).toBeTruthy();
    await user.keyboard("{Escape}");

    expect(screen.queryByRole("listbox")).toBeNull();
    expect(screen.getByRole("button", { name: /^Speaker 1$/ })).toBeTruthy();
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
    await user.click(screen.getByRole("button", { name: /^Michael$/ }));
    await user.click(screen.getByRole("option", { name: "Speaker 2" }));
    expect(onRename).toHaveBeenCalledWith("Michael", "Speaker 2");
  });
});

describe("SpeakerLabels rename picker", () => {
  // The picker is the cross-note identity strategy (#116 part 1): with no alias
  // table (ADR-0002) the rename IS the join key, so these tests are about
  // convergence on one spelling, not about convenience.
  const SUGGESTIONS = {
    stats: [
      { label: "Hege Tronshaugen", note_count: 4, last_used_at: 1_767_000_000 },
      { label: "Åse Berg", note_count: 1, last_used_at: 1_766_000_000 },
    ],
    roster: ["Anna Lie"],
  };

  it("suggests a name you have used in another note", async () => {
    const user = userEvent.setup();
    render(
      <SpeakerLabels transcript={TWO_SPEAKERS} onRename={vi.fn()} suggestions={SUGGESTIONS} />,
    );
    await user.click(screen.getByRole("button", { name: /^Speaker 1$/ }));
    await user.clear(screen.getByRole("combobox"));
    await user.type(screen.getByRole("combobox"), "tron");

    // Word-start match: a surname is how you'd actually reach a full name.
    expect(screen.getByRole("option", { name: /Hege Tronshaugen/ })).toBeTruthy();
  });

  it("commits the suggestion picked with the arrow keys", async () => {
    const user = userEvent.setup();
    const onRename = vi.fn();
    render(
      <SpeakerLabels transcript={TWO_SPEAKERS} onRename={onRename} suggestions={SUGGESTIONS} />,
    );
    await user.click(screen.getByRole("button", { name: /^Speaker 1$/ }));
    await user.clear(screen.getByRole("combobox"));
    await user.type(screen.getByRole("combobox"), "tron");
    await user.keyboard("{ArrowDown}{Enter}");

    expect(onRename).toHaveBeenCalledWith("Speaker 1", "Hege Tronshaugen");
  });

  it("still commits free text, so a new person is no harder than an existing one", async () => {
    const user = userEvent.setup();
    const onRename = vi.fn();
    render(
      <SpeakerLabels transcript={TWO_SPEAKERS} onRename={onRename} suggestions={SUGGESTIONS} />,
    );
    await user.click(screen.getByRole("button", { name: /^Speaker 1$/ }));
    await user.clear(screen.getByRole("combobox"));
    // A prefix of an existing suggestion, committed literally.
    await user.type(screen.getByRole("combobox"), "Hege");
    await user.keyboard("{Enter}");

    expect(onRename).toHaveBeenCalledWith("Speaker 1", "Hege");
  });

  it("preselects the existing spelling when the typed text is a case variant", async () => {
    const user = userEvent.setup();
    const onRename = vi.fn();
    render(
      <SpeakerLabels transcript={TWO_SPEAKERS} onRename={onRename} suggestions={SUGGESTIONS} />,
    );
    await user.click(screen.getByRole("button", { name: /^Speaker 1$/ }));
    await user.clear(screen.getByRole("combobox"));
    await user.type(screen.getByRole("combobox"), "ase berg");
    await user.keyboard("{Enter}");

    // "ase berg" is never a deliberate second person — in a Norwegian-market
    // product this split is the specific risk the picker exists to prevent.
    expect(onRename).toHaveBeenCalledWith("Speaker 1", "Åse Berg");
  });

  it("tags a label already on this note as a merge", async () => {
    const user = userEvent.setup();
    render(
      <SpeakerLabels
        transcript={"Michael: hi\nHege Tronshaugen: hello"}
        onRename={vi.fn()}
        suggestions={SUGGESTIONS}
      />,
    );
    await user.click(screen.getByRole("button", { name: /^Michael$/ }));
    await user.clear(screen.getByRole("combobox"));
    await user.type(screen.getByRole("combobox"), "hege");

    // Shown rather than hidden: typing the name in full merges anyway, so
    // hiding the row would only remove the warning.
    expect(screen.getByRole("option", { name: /Hege Tronshaugen/ }).textContent).toMatch(/merge/i);
  });

  it("never suggests a placeholder label", async () => {
    const user = userEvent.setup();
    render(
      <SpeakerLabels
        transcript={"Speaker 1: hello\nMichael: hi"}
        onRename={vi.fn()}
        suggestions={{
          stats: [
            { label: "Speaker 2", note_count: 9, last_used_at: 1_767_000_000 },
            { label: "You", note_count: 9, last_used_at: 1_767_000_000 },
          ],
          roster: [],
        }}
      />,
    );
    await user.click(screen.getByRole("button", { name: /^Speaker 1$/ }));
    // Converging on a placeholder is the opposite of the point. Michael is
    // offered only as a merge, being on this note.
    expect(screen.getAllByRole("option").map((o) => o.getAttribute("aria-label"))).toEqual(["Michael"]);
  });

  it("renames with no picker at all when there is nothing to suggest", async () => {
    const user = userEvent.setup();
    const onRename = vi.fn();
    render(<SpeakerLabels transcript={TWO_SPEAKERS} onRename={onRename} />);

    await user.click(screen.getByRole("button", { name: /^Speaker 1$/ }));
    await user.clear(screen.getByRole("combobox"));
    await user.type(screen.getByRole("combobox"), "Michael");
    await user.keyboard("{Enter}");

    expect(onRename).toHaveBeenCalledWith("Speaker 1", "Michael");
  });
});

describe("SpeakerLabels cross-note rename", () => {
  // #116 part 2. ADR-0002 forbids an alias table, so rewriting transcripts is
  // the only repair for divergent spellings — and the choice of scope IS the
  // commit: no modal, no destructive default, same reasoning as #23's merge menu.
  //
  // The source label is a real NAME throughout, because that is the only case a
  // sweep is offered for: see the per-recording-label suite below.
  const NAMED = "Hege: hello there\nSpeaker 2: hi back";

  async function renameTo(user: ReturnType<typeof userEvent.setup>, name: string) {
    await user.click(screen.getByRole("button", { name: /^Hege$/ }));
    await user.clear(screen.getByRole("combobox"));
    await user.type(screen.getByRole("combobox"), name);
    await user.keyboard("{Enter}");
  }

  it("offers the scope choice when the label is in other notes too", async () => {
    const user = userEvent.setup();
    render(
      <SpeakerLabels
        transcript={NAMED}
        onRename={vi.fn()}
        onRenameEverywhere={vi.fn()}
        otherNotesWithLabel={{ Hege: 11 }}
      />,
    );
    await renameTo(user, "Hege Tronshaugen");

    // The count says what it will touch: this note plus the other 11.
    expect(screen.getByRole("menuitem", { name: /rename here only/i })).toBeTruthy();
    expect(screen.getByRole("menuitem", { name: /rename in all 12 notes/i })).toBeTruthy();
  });

  it("does not rename until a scope is chosen", async () => {
    const user = userEvent.setup();
    const onRename = vi.fn();
    render(
      <SpeakerLabels
        transcript={NAMED}
        onRename={onRename}
        onRenameEverywhere={vi.fn()}
        otherNotesWithLabel={{ Hege: 3 }}
      />,
    );
    await renameTo(user, "Hege Tronshaugen");
    expect(onRename).not.toHaveBeenCalled();
  });

  it("renames this note only when that is the choice", async () => {
    const user = userEvent.setup();
    const onRename = vi.fn();
    const onRenameEverywhere = vi.fn();
    render(
      <SpeakerLabels
        transcript={NAMED}
        onRename={onRename}
        onRenameEverywhere={onRenameEverywhere}
        otherNotesWithLabel={{ Hege: 3 }}
      />,
    );
    await renameTo(user, "Hege Tronshaugen");
    await user.click(screen.getByRole("menuitem", { name: /rename here only/i }));

    expect(onRename).toHaveBeenCalledWith("Hege", "Hege Tronshaugen");
    expect(onRenameEverywhere).not.toHaveBeenCalled();
  });

  it("renames everywhere when that is the choice", async () => {
    const user = userEvent.setup();
    const onRename = vi.fn();
    const onRenameEverywhere = vi.fn();
    render(
      <SpeakerLabels
        transcript={NAMED}
        onRename={onRename}
        onRenameEverywhere={onRenameEverywhere}
        otherNotesWithLabel={{ Hege: 3 }}
      />,
    );
    await renameTo(user, "Hege Tronshaugen");
    await user.click(screen.getByRole("menuitem", { name: /rename in all 4 notes/i }));

    expect(onRenameEverywhere).toHaveBeenCalledWith("Hege", "Hege Tronshaugen");
    // The per-note callback is NOT also fired — the sweep covers this note too.
    expect(onRename).not.toHaveBeenCalled();
  });

  it("renames immediately when no other note carries the label", async () => {
    const user = userEvent.setup();
    const onRename = vi.fn();
    render(
      <SpeakerLabels
        transcript={NAMED}
        onRename={onRename}
        onRenameEverywhere={vi.fn()}
        otherNotesWithLabel={{ Hege: 0 }}
      />,
    );
    await renameTo(user, "Hege Tronshaugen");

    // Nothing to choose between, so asking would be a pointless extra click.
    expect(onRename).toHaveBeenCalledWith("Hege", "Hege Tronshaugen");
    expect(screen.queryByRole("menuitem", { name: /rename here only/i })).toBeNull();
  });

  it("abandons the scope choice on Escape without renaming", async () => {
    const user = userEvent.setup();
    const onRename = vi.fn();
    const onRenameEverywhere = vi.fn();
    render(
      <SpeakerLabels
        transcript={NAMED}
        onRename={onRename}
        onRenameEverywhere={onRenameEverywhere}
        otherNotesWithLabel={{ Hege: 3 }}
      />,
    );
    await renameTo(user, "Hege Tronshaugen");
    await user.keyboard("{Escape}");

    expect(screen.queryByRole("menuitem", { name: /rename here only/i })).toBeNull();
    expect(onRename).not.toHaveBeenCalled();
    expect(onRenameEverywhere).not.toHaveBeenCalled();
  });

  it("keeps renaming per-note when the parent offers no sweep", async () => {
    const user = userEvent.setup();
    const onRename = vi.fn();
    // No `onRenameEverywhere` / no counts → the strip behaves exactly as it did
    // before part 2 existed.
    render(<SpeakerLabels transcript={NAMED} onRename={onRename} />);
    await renameTo(user, "Hege Tronshaugen");
    expect(onRename).toHaveBeenCalledWith("Hege", "Hege Tronshaugen");
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

describe("SpeakerLabels never sweeps a per-recording label", () => {
  // "You" is whoever held the mic and "Speaker 1" is whoever the diarizer
  // clustered first, so each names a different person in every recording.
  // Renaming them across notes wrote false attribution into teammates' meetings.
  it.each(["You", "Speaker 1"])("renames %s in this note only, with no choice offered", async (label) => {
    const user = userEvent.setup();
    const onRename = vi.fn();
    const onRenameEverywhere = vi.fn();
    render(
      <SpeakerLabels
        transcript={`${label}: hi\nHege: hello`}
        onRename={onRename}
        onRenameEverywhere={onRenameEverywhere}
        otherNotesWithLabel={{ [label]: 40 }}
      />,
    );
    await user.click(screen.getByRole("button", { name: new RegExp(`^${label}$`) }));
    await user.clear(screen.getByRole("combobox"));
    await user.type(screen.getByRole("combobox"), "Kurt Skoland");
    await user.keyboard("{Enter}");

    expect(screen.queryByRole("menuitem", { name: /rename in all/i })).toBeNull();
    expect(onRename).toHaveBeenCalledWith(label, "Kurt Skoland");
    expect(onRenameEverywhere).not.toHaveBeenCalled();
  });

  it("still sweeps a real name, which does mean one person everywhere", async () => {
    const user = userEvent.setup();
    const onRenameEverywhere = vi.fn();
    render(
      <SpeakerLabels
        transcript={"Hege: hi\nSpeaker 2: hello"}
        onRename={vi.fn()}
        onRenameEverywhere={onRenameEverywhere}
        otherNotesWithLabel={{ Hege: 3 }}
      />,
    );
    await user.click(screen.getByRole("button", { name: /^Hege$/ }));
    await user.clear(screen.getByRole("combobox"));
    await user.type(screen.getByRole("combobox"), "Hege Tronshaugen");
    await user.keyboard("{Enter}");
    await user.click(screen.getByRole("menuitem", { name: /rename in all 4 notes/i }));

    expect(onRenameEverywhere).toHaveBeenCalledWith("Hege", "Hege Tronshaugen");
  });
});
