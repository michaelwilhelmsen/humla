import { beforeAll, beforeEach, describe, expect, it } from "vitest";
import { cleanup, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { renderApp } from "../test/app";
import { makeNote } from "../test/fixtures";
import { mockLayoutBox } from "../test/layout";

// The context panel opens on every note the way the user last left it.

beforeAll(() => mockLayoutBox());
beforeEach(() => localStorage.clear());

function openNote(id: string) {
  const note = makeNote({ id, title: `Note ${id}` });
  renderApp(`/note/${id}`, {
    notes_list: () => [note],
    notes_get: () => note,
    note_timeline: () => [],
  });
}

const toggle = () => screen.findByRole("button", { name: "Toggle context panel" });

describe("the note's context panel", () => {
  it("opens by default", async () => {
    openNote("n1");
    expect(await toggle()).toHaveAttribute("aria-pressed", "true");
  });

  it("stays closed on the next note once the user closed it", async () => {
    openNote("n1");
    await userEvent.click(await toggle());
    expect(await toggle()).toHaveAttribute("aria-pressed", "false");
    cleanup();

    openNote("n2");
    expect(await toggle()).toHaveAttribute("aria-pressed", "false");
  });

  it("reopens on the next note once the user opened it again", async () => {
    localStorage.setItem("humla.panelOpen", "false");
    openNote("n1");
    await userEvent.click(await toggle());
    cleanup();

    openNote("n2");
    expect(await toggle()).toHaveAttribute("aria-pressed", "true");
  });

  it("opens at the width the user last dragged it to", async () => {
    localStorage.setItem("humla.panelWidth", "520");
    openNote("n1");
    await toggle();
    const panel = document.querySelector<HTMLElement>("[aria-hidden='false'][style*='width']");
    expect(panel?.style.width).toBe("520px");
  });
});
