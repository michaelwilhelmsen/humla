import { beforeAll, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router-dom";
import App from "../App";
import { makeNote } from "../test/fixtures";
import { mockLayoutBox } from "../test/layout";
import { mockTauri } from "../test/tauri";
import { NO_POLICY } from "../lib/orgPolicy";
import type { Note } from "../lib/ipc";

beforeAll(() => mockLayoutBox());

function open(note: Note, audit: Record<string, boolean> | undefined) {
  const opened = vi.fn(() => null);
  const copied = vi.fn(() => null);
  mockTauri({
    notes_list: () => [note],
    notes_get: () => note,
    note_timeline: () => [],
    org_status: () => ({ policy: NO_POLICY, orgs: [], retention: {}, ...(audit ? { audit } : {}) }),
    audit_note_opened: opened,
    audit_note_copied: copied,
  });
  render(
    <MemoryRouter initialEntries={[`/note/${note.id}`]}>
      <App />
    </MemoryRouter>,
  );
  return { opened, copied };
}

const titleBox = () => screen.getByPlaceholderText("New note") as HTMLTextAreaElement;

async function copySummary() {
  Object.defineProperty(navigator, "clipboard", {
    value: { writeText: vi.fn(() => Promise.resolve()) },
    configurable: true,
  });
  await userEvent.click(await screen.findByRole("button", { name: "Copy Summary" }));
  await screen.findByRole("button", { name: "Summary copied" });
}

describe("the organisation audit log", () => {
  it("reports opening a note once when its organisation records opens", async () => {
    const { opened } = open(makeNote({ id: "n1", title: "Case", workspace_id: "w1" }), { w1: true });
    await waitFor(() => expect(opened).toHaveBeenCalledWith({ noteId: "n1" }));
    await userEvent.type(titleBox(), " notes");
    expect(opened).toHaveBeenCalledTimes(1);
  });

  it("doesn't report opening a note when audit_reads is off, in Personal, or with no organisation", async () => {
    const cases: [string, Record<string, boolean> | undefined][] = [
      ["w1", { w1: false }],
      ["", { w1: true }],
      ["w2", { w1: true }],
      ["w1", undefined],
    ];
    for (const [workspace_id, audit] of cases) {
      const { opened } = open(makeNote({ id: "n1", title: "Case", workspace_id }), audit);
      await waitFor(() => expect(titleBox()).toHaveValue("Case"));
      await new Promise((r) => setTimeout(r, 50));
      expect(opened).not.toHaveBeenCalled();
      cleanup();
    }
  });

  it("reports a copy of an organisation note, audit_reads or not", async () => {
    const { copied } = open(makeNote({ id: "n1", title: "Case", summary: "Decided.", workspace_id: "w1" }), { w1: false });
    await copySummary();
    expect(copied).toHaveBeenCalledWith({ noteId: "n1" });
  });

  it("never reports a copy of a Personal note", async () => {
    const { copied } = open(makeNote({ id: "n1", title: "Case", summary: "Decided.", workspace_id: "" }), { w1: true });
    await copySummary();
    expect(copied).not.toHaveBeenCalled();
  });
});
