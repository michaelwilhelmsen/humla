import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { TemplatesSection } from "./Templates";
import { mockTauri } from "../../../test/tauri";
import { DISCONNECTED, useCloudStore, type CloudWorkspace } from "../../../lib/cloud";
import type { SummaryPrompt } from "../../../lib/ipc";

function prompt(id: string, name: string, workspace_id = ""): SummaryPrompt {
  return { id, name, content: `${name} instructions`, created_at: 0, updated_at: 0, workspace_id };
}

function inWorkspace(name: string, role: CloudWorkspace["role"] = "member") {
  const ws = { id: "ws1", name, role } as CloudWorkspace;
  useCloudStore.setState({
    status: {
      ...DISCONNECTED,
      configured: true,
      logged_in: true,
      current_workspace: ws,
      workspaces: [ws],
    },
    ready: true,
  });
}

beforeEach(() => {
  mockTauri();
  useCloudStore.setState({ status: DISCONNECTED, ready: true });
});

describe("TemplatesSection", () => {
  it("lists the six built-in templates", async () => {
    render(<TemplatesSection language="en" />);
    const builtIn = screen.getByRole("heading", { name: "Built-in" }).closest("section")!;
    expect(within(builtIn).getAllByRole("button", { name: /^View / })).toHaveLength(6);
  });

  it("shows a built-in's instructions read-only", async () => {
    render(<TemplatesSection language="en" />);
    await userEvent.click(screen.getByRole("button", { name: "View Meeting" }));
    const viewer = await screen.findByRole("dialog", { name: "Meeting" });
    expect(viewer).toHaveTextContent("You produce meeting notes");
    expect(within(viewer).queryByRole("textbox")).toBeNull();
  });

  it("duplicates a built-in into the editor as a copy without its language clause", async () => {
    render(<TemplatesSection language="en" />);
    await userEvent.click(screen.getByRole("button", { name: "Duplicate Lecture" }));
    const editor = await screen.findByRole("dialog", { name: "New template" });
    expect(within(editor).getByLabelText("Name")).toHaveValue("Lecture (copy)");
    const instructions = within(editor).getByLabelText("Instructions") as HTMLTextAreaElement;
    expect(instructions.value).toContain("study notes");
    expect(instructions.value).not.toMatch(/Reply in|\{LANGUAGE\}/);
  });

  it("creates a template under the name typed", async () => {
    const created = vi.fn(() => prompt("n1", "Standup"));
    mockTauri({ summary_prompts_create: created });
    render(<TemplatesSection language="en" />);
    await userEvent.click(screen.getByRole("button", { name: /new template/i }));
    const editor = await screen.findByRole("dialog", { name: "New template" });
    expect(within(editor).getByRole("button", { name: "Save template" })).toBeDisabled();
    await userEvent.type(within(editor).getByLabelText("Name"), "Standup");
    await userEvent.type(within(editor).getByLabelText("Instructions"), "Blockers first.");
    await userEvent.click(within(editor).getByRole("button", { name: "Save template" }));
    expect(created).toHaveBeenCalledWith({ name: "Standup", content: "Blockers first." });
  });

  it("deletes only after an inline confirm", async () => {
    const deleted = vi.fn(() => null);
    mockTauri({
      summary_prompts_list: () => [prompt("p1", "Standup")],
      summary_prompts_delete: deleted,
    });
    render(<TemplatesSection language="en" />);
    await userEvent.click(await screen.findByRole("button", { name: "Delete Standup" }));
    expect(deleted).not.toHaveBeenCalled();
    expect(screen.getByText("Delete “Standup”?")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Delete" }));
    expect(deleted).toHaveBeenCalledWith({ id: "p1" });
  });

  it("keeps everything under Your templates outside a workspace", async () => {
    mockTauri({ summary_prompts_list: () => [prompt("p1", "Standup")] });
    render(<TemplatesSection language="en" />);
    const yours = (await screen.findByRole("heading", { name: "Your templates" })).closest("section")!;
    expect(await within(yours).findByText("Standup")).toBeInTheDocument();
    expect(within(yours).getByRole("button", { name: /new template/i })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: /^Shared/ })).toBeNull();
  });

  // A template made in a workspace syncs to its members, so "New template" sits
  // where the new one will land.
  it("splits a workspace's shared templates from your personal ones", async () => {
    inWorkspace("Rana kommune");
    mockTauri({
      summary_prompts_list: () => [prompt("p1", "Standup"), prompt("p2", "Journal note", "ws1")],
    });
    render(<TemplatesSection language="en" />);
    const shared = (
      await screen.findByRole("heading", { name: "Shared in Rana kommune" })
    ).closest("section")!;
    expect(await within(shared).findByText("Journal note")).toBeInTheDocument();
    expect(within(shared).getByRole("button", { name: /new template/i })).toBeInTheDocument();
    expect(within(shared).getByRole("button", { name: "Edit Journal note" })).toBeInTheDocument();

    const yours = screen.getByRole("heading", { name: "Your templates" }).closest("section")!;
    expect(within(yours).getByText("Standup")).toBeInTheDocument();
    expect(within(yours).queryByRole("button", { name: /new template/i })).toBeNull();
  });

  it("hides an empty personal list inside a workspace", async () => {
    inWorkspace("Team");
    render(<TemplatesSection language="en" />);
    expect(await screen.findByRole("heading", { name: "Shared in Team" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Your templates" })).toBeNull();
  });

  // humla-cloud refuses every template write from a viewer, and anything new
  // lands in the active workspace.
  it("gives a viewer read-only shared templates and nothing that creates one", async () => {
    inWorkspace("Rana kommune", "viewer");
    mockTauri({
      summary_prompts_list: () => [prompt("p1", "Standup"), prompt("p2", "Journal note", "ws1")],
    });
    render(<TemplatesSection language="en" />);
    const shared = (
      await screen.findByRole("heading", { name: "Shared in Rana kommune" })
    ).closest("section")!;
    expect(await within(shared).findByText("Journal note")).toBeInTheDocument();
    expect(within(shared).queryByRole("button", { name: /new template/i })).toBeNull();
    expect(within(shared).queryByRole("button", { name: "Edit Journal note" })).toBeNull();
    expect(within(shared).queryByRole("button", { name: "Delete Journal note" })).toBeNull();
    expect(screen.queryByRole("button", { name: /^Duplicate / })).toBeNull();

    await userEvent.click(within(shared).getByRole("button", { name: "View Journal note" }));
    const viewer = await screen.findByRole("dialog", { name: "Journal note" });
    expect(viewer).toHaveTextContent("Journal note instructions");
    expect(within(viewer).queryByRole("button", { name: "Duplicate" })).toBeNull();
    await userEvent.keyboard("{Escape}");

    const yours = screen.getByRole("heading", { name: "Your templates" }).closest("section")!;
    expect(within(yours).getByRole("button", { name: "Edit Standup" })).toBeInTheDocument();
    expect(within(yours).getByRole("button", { name: "Delete Standup" })).toBeInTheDocument();
  });
});
