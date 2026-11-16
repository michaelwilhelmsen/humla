import { describe, expect, it, vi } from "vitest";
import { render } from "@testing-library/react";
import { NoteEditor } from "./Editor";

describe("NoteEditor", () => {
  // Opening a note must never rewrite its body: a save here syncs a change
  // the user never made to every device.
  it.each([
    ["a heading", "<h2>Title</h2>"],
    ["a list", "<ul><li><p>one</p></li></ul>"],
    ["a quote", "<blockquote><p>q</p></blockquote>"],
  ])("leaves a body ending in %s alone when the note re-renders", (_, html) => {
    const onChange = vi.fn();
    const { rerender } = render(<NoteEditor initialHTML={html} onChange={onChange} />);
    rerender(<NoteEditor initialHTML={html} onChange={onChange} />);
    rerender(<NoteEditor initialHTML={html} onChange={onChange} editable={false} />);
    rerender(<NoteEditor initialHTML={html} onChange={onChange} />);
    expect(onChange).not.toHaveBeenCalled();
  });
});
