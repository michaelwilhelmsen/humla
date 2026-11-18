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

  describe("from an organisation", () => {
    const orgStatus = {
      policy: {
        org_name: null,
        reason: null,
        local_only: false,
        retention_days: 0,
        telemetry_off: false,
        audio_forbidden: false,
        rules: [],
      },
      orgs: [{ id: "o1", name: "Northwind", reason: "Approved for every department.", vocabulary: [], workspaces: ["ws1"] }],
    };
    const orgTemplate = (id: string, name: string, departments: string[]): SummaryPrompt => ({
      ...prompt(id, name),
      org_id: "o1",
      org_workspaces: departments,
    });

    it("lists them first, read-only, with the marker once on the heading", async () => {
      inWorkspace("Intake");
      mockTauri({
        org_status: () => orgStatus,
        summary_prompts_list: () => [
          prompt("p1", "Standup"),
          orgTemplate("t1", "Case meeting", []),
          orgTemplate("t2", "Intake review", ["ws1"]),
          orgTemplate("t3", "Elsewhere", ["ws9"]),
        ],
      });
      render(<TemplatesSection language="en" />);
      const heading = await screen.findByRole("heading", { name: "From your organisation" });
      const group = heading.closest("section")!;
      const sections = screen.getAllByRole("heading", { level: 2 }).map((h) => h.textContent);
      expect(sections[0]).toBe("From your organisation");
      expect(await within(group).findAllByRole("button", { name: /Managed by Northwind/ })).toHaveLength(1);
      expect(within(group).getByText("Case meeting")).toBeInTheDocument();
      expect(within(group).getByText("Intake review")).toBeInTheDocument();
      expect(within(group).queryByText("Elsewhere")).toBeNull();
      expect(within(group).getByText("Intake")).toBeInTheDocument();
      expect(within(group).queryByRole("button", { name: /^Edit / })).toBeNull();
      expect(within(group).queryByRole("button", { name: /^Delete / })).toBeNull();
      expect(within(group).getByRole("button", { name: "View Case meeting" })).toBeInTheDocument();
      expect(screen.queryByText("Standup")?.closest("section")).not.toBe(group);
    });

    it("duplicates one into the editor as the user's own copy", async () => {
      mockTauri({
        org_status: () => orgStatus,
        summary_prompts_list: () => [orgTemplate("t1", "Case meeting", [])],
      });
      render(<TemplatesSection language="en" />);
      await userEvent.click(await screen.findByRole("button", { name: "Duplicate Case meeting" }));
      const editor = await screen.findByRole("dialog", { name: "New template" });
      expect(within(editor).getByLabelText("Name")).toHaveValue("Case meeting (copy)");
      expect(within(editor).getByLabelText("Instructions")).toHaveValue("Case meeting instructions");
    });

    it("offers none in a team workspace outside the organisation, so none can be duplicated there", async () => {
      inWorkspace("Book club");
      mockTauri({
        org_status: () => ({ ...orgStatus, orgs: [{ ...orgStatus.orgs[0], workspaces: ["ws9"] }] }),
        summary_prompts_list: () => [prompt("p1", "Standup"), orgTemplate("t1", "Case meeting", [])],
      });
      render(<TemplatesSection language="en" />);
      await screen.findByText("Standup");
      await new Promise((r) => setTimeout(r, 0));
      expect(screen.queryByRole("heading", { name: "From your organisation" })).toBeNull();
      expect(screen.queryByText("Case meeting")).toBeNull();
      expect(screen.queryByRole("button", { name: "Duplicate Case meeting" })).toBeNull();
    });

    it("shows no organisation group without one", async () => {
      mockTauri({ summary_prompts_list: () => [prompt("p1", "Standup")] });
      render(<TemplatesSection language="en" />);
      await screen.findByText("Standup");
      expect(screen.queryByRole("heading", { name: "From your organisation" })).toBeNull();
      expect(screen.getAllByRole("heading", { level: 2 }).map((h) => h.textContent)).toEqual([
        "Built-in",
        "Your templates",
      ]);
    });
  });
  describe("as an organisation admin", () => {
    const intake = { id: "ws1", name: "Intake", role: "member" } as CloudWorkspace;
    const homeCare = { id: "ws2", name: "Home care", role: "member" } as CloudWorkspace;
    const policy = {
      org_name: null,
      reason: null,
      local_only: false,
      retention_days: 0,
      telemetry_off: false,
      audio_forbidden: false,
      rules: [],
    };
    const orgStatus = {
      policy,
      orgs: [{ id: "o1", name: "Northwind", reason: "", vocabulary: [], workspaces: ["ws1", "ws2"] }],
    };
    const orgTemplate = (clientId: string, name: string, departments: string[]): SummaryPrompt => ({
      ...prompt(`org_o1_${clientId}`, name),
      org_id: "o1",
      org_workspaces: departments,
    });
    const rows = () => [orgTemplate("case", "Case meeting", []), orgTemplate("home", "Home visit", ["ws2"])];
    // The server's copy also holds a template for a department the admin isn't
    // in, which the config never sends.
    const serverContent = {
      templates: [
        { client_id: "case", name: "Case meeting", content: "Case meeting instructions", workspaces: [] },
        { client_id: "home", name: "Home visit", content: "Home visit instructions", workspaces: ["ws2"] },
        { client_id: "far", name: "Far away", content: "x", workspaces: ["ws9"] },
      ],
      vocabulary: [],
    };

    function signedIn(twoFactor = false) {
      useCloudStore.setState({
        status: {
          ...DISCONNECTED,
          configured: true,
          logged_in: true,
          user: { id: "u1", email: "a@example.no", name: "A", verified: true },
          current_workspace: intake,
          workspaces: [intake, homeCare],
          two_factor: { available: true, enabled: twoFactor, recovery_codes_left: 0, required_by: [] },
        },
        ready: true,
      });
    }

    async function orgGroup() {
      return (await screen.findByRole("heading", { name: "From your organisation" })).closest("section")!;
    }

    it("leaves a member's view read-only", async () => {
      signedIn();
      mockTauri({ org_status: () => orgStatus, summary_prompts_list: rows, org_admin_orgs: () => [] });
      render(<TemplatesSection language="en" />);
      const group = await orgGroup();
      expect(await within(group).findByText("Case meeting")).toBeInTheDocument();
      expect(within(group).queryByText("Home visit")).toBeNull();
      expect(within(group).queryByRole("button", { name: /^Edit / })).toBeNull();
      expect(within(group).queryByRole("button", { name: /new template/i })).toBeNull();
    });

    it("never asks the server signed out", async () => {
      const asked = vi.fn(() => [{ id: "o1", name: "Northwind", require_2fa: false }]);
      mockTauri({ org_status: () => orgStatus, summary_prompts_list: rows, org_admin_orgs: asked });
      render(<TemplatesSection language="en" />);
      const group = await orgGroup();
      await within(group).findByText("Case meeting");
      expect(asked).not.toHaveBeenCalled();
      expect(within(group).queryByRole("button", { name: /^Edit / })).toBeNull();
    });

    it("lists every one of the organisation's templates to an admin, editable", async () => {
      signedIn();
      mockTauri({
        org_status: () => orgStatus,
        summary_prompts_list: rows,
        org_admin_orgs: () => [{ id: "o1", name: "Northwind", require_2fa: false }],
        org_admin_content: () => serverContent,
      });
      render(<TemplatesSection language="en" />);
      const group = await orgGroup();
      expect(await within(group).findByRole("button", { name: "Edit Home visit" })).toBeInTheDocument();
      expect(within(group).getByRole("button", { name: "Edit Case meeting" })).toBeInTheDocument();
      expect(within(group).getByRole("button", { name: "Delete Case meeting" })).toBeInTheDocument();
      expect(within(group).getByText("Home care")).toBeInTheDocument();
      expect(within(group).getByRole("button", { name: "Edit Far away" })).toBeInTheDocument();
      expect(within(group).getByText("1 department")).toBeInTheDocument();
      expect(within(group).getAllByText("Case meeting")).toHaveLength(1);
    });

    it("shows the cached rows read-only until the server's copy is read", async () => {
      signedIn();
      mockTauri({
        org_status: () => orgStatus,
        summary_prompts_list: rows,
        org_admin_orgs: () => [{ id: "o1", name: "Northwind", require_2fa: false }],
        org_admin_content: () => {
          throw "Couldn't reach the server. Try again when you're online.";
        },
      });
      render(<TemplatesSection language="en" />);
      const group = await orgGroup();
      expect(await within(group).findByRole("alert")).toHaveTextContent("Couldn't reach the server.");
      expect(within(group).getByText("Case meeting")).toBeInTheDocument();
      expect(within(group).queryByRole("button", { name: /^Edit / })).toBeNull();
      expect(within(group).queryByText("Far away")).toBeNull();
    });

    it("keeps the editor open on a deleted template and reads the server again", async () => {
      signedIn();
      const asked = vi.fn(() => serverContent);
      mockTauri({
        org_status: () => orgStatus,
        summary_prompts_list: rows,
        org_admin_orgs: () => [{ id: "o1", name: "Northwind", require_2fa: false }],
        org_admin_content: asked,
        org_template_open: () => ({ name: "Case meeting", content: "x", workspaces: [], hidden: 0 }),
        org_template_save: () => {
          throw "This template was deleted.";
        },
      });
      render(<TemplatesSection language="en" />);
      await userEvent.click(await screen.findByRole("button", { name: "Edit Case meeting" }));
      const editor = await screen.findByRole("dialog", { name: "Edit organisation template" });
      await userEvent.click(within(editor).getByRole("button", { name: "Save template" }));
      expect(await within(editor).findByRole("alert")).toHaveTextContent("This template was deleted.");
      await vi.waitFor(() => expect(asked).toHaveBeenCalledTimes(2));
    });

    it("creates one for chosen departments", async () => {
      signedIn();
      const saved = vi.fn(() => null);
      mockTauri({
        org_status: () => orgStatus,
        summary_prompts_list: rows,
        org_admin_orgs: () => [{ id: "o1", name: "Northwind", require_2fa: false }],
        org_admin_content: () => serverContent,
        org_template_save: saved,
      });
      render(<TemplatesSection language="en" />);
      const group = await orgGroup();
      await userEvent.click(await within(group).findByRole("button", { name: /new template/i }));
      const editor = await screen.findByRole("dialog", { name: "New organisation template" });
      await userEvent.type(within(editor).getByLabelText("Name"), "Intake review");
      await userEvent.type(within(editor).getByLabelText("Instructions"), "Needs first.");
      await userEvent.click(within(editor).getByRole("radio", { name: "Some departments" }));
      expect(within(editor).getByRole("button", { name: "Save template" })).toBeDisabled();
      await userEvent.click(within(editor).getByRole("checkbox", { name: "Intake" }));
      await userEvent.click(within(editor).getByRole("button", { name: "Save template" }));
      expect(saved).toHaveBeenCalledWith({
        orgId: "o1",
        clientId: null,
        name: "Intake review",
        content: "Needs first.",
        workspaces: ["ws1"],
      });
      expect(screen.queryByRole("dialog", { name: "New organisation template" })).toBeNull();
    });

    it("edits one from the server's copy and keeps departments it can't show", async () => {
      signedIn();
      const opened = vi.fn(() => ({ name: "Home visit", content: "Server text", workspaces: ["ws2"], hidden: 2 }));
      const saved = vi.fn(() => null);
      mockTauri({
        org_status: () => orgStatus,
        summary_prompts_list: rows,
        org_admin_orgs: () => [{ id: "o1", name: "Northwind", require_2fa: false }],
        org_admin_content: () => serverContent,
        org_template_open: opened,
        org_template_save: saved,
      });
      render(<TemplatesSection language="en" />);
      await userEvent.click(await screen.findByRole("button", { name: "Edit Home visit" }));
      expect(opened).toHaveBeenCalledWith({ orgId: "o1", clientId: "home" });
      const editor = await screen.findByRole("dialog", { name: "Edit organisation template" });
      expect(within(editor).getByLabelText("Instructions")).toHaveValue("Server text");
      expect(within(editor).getByRole("radio", { name: "Some departments" })).toHaveAttribute("aria-checked", "true");
      expect(within(editor).getByRole("checkbox", { name: "Home care" })).toBeChecked();
      expect(within(editor).getByRole("checkbox", { name: "Intake" })).not.toBeChecked();
      expect(within(editor).getByText("Also offered in 2 departments you're not in.")).toBeInTheDocument();
      // Unticking every department shown still leaves the hidden ones.
      await userEvent.click(within(editor).getByRole("checkbox", { name: "Home care" }));
      expect(within(editor).getByRole("button", { name: "Save template" })).toBeEnabled();
      await userEvent.click(within(editor).getByRole("checkbox", { name: "Intake" }));
      await userEvent.click(within(editor).getByRole("button", { name: "Save template" }));
      expect(saved).toHaveBeenCalledWith({
        orgId: "o1",
        clientId: "home",
        name: "Home visit",
        content: "Server text",
        workspaces: ["ws1"],
      });
    });

    it("offers it in every department again", async () => {
      signedIn();
      const saved = vi.fn(() => null);
      mockTauri({
        org_status: () => orgStatus,
        summary_prompts_list: rows,
        org_admin_orgs: () => [{ id: "o1", name: "Northwind", require_2fa: false }],
        org_admin_content: () => serverContent,
        org_template_open: () => ({ name: "Home visit", content: "x", workspaces: ["ws2"], hidden: 0 }),
        org_template_save: saved,
      });
      render(<TemplatesSection language="en" />);
      await userEvent.click(await screen.findByRole("button", { name: "Edit Home visit" }));
      const editor = await screen.findByRole("dialog", { name: "Edit organisation template" });
      await userEvent.click(within(editor).getByRole("radio", { name: "Every department" }));
      expect(within(editor).queryByRole("checkbox")).toBeNull();
      await userEvent.click(within(editor).getByRole("button", { name: "Save template" }));
      expect(saved).toHaveBeenCalledWith(expect.objectContaining({ clientId: "home", workspaces: null }));
    });

    it("deletes one for everyone after an inline confirm", async () => {
      signedIn();
      const deleted = vi.fn(() => null);
      mockTauri({
        org_status: () => orgStatus,
        summary_prompts_list: rows,
        org_admin_orgs: () => [{ id: "o1", name: "Northwind", require_2fa: false }],
        org_admin_content: () => serverContent,
        org_template_delete: deleted,
      });
      render(<TemplatesSection language="en" />);
      await userEvent.click(await screen.findByRole("button", { name: "Delete Case meeting" }));
      expect(deleted).not.toHaveBeenCalled();
      expect(screen.getByText("Delete “Case meeting” for everyone?")).toBeInTheDocument();
      await userEvent.click(screen.getByRole("button", { name: "Delete" }));
      expect(deleted).toHaveBeenCalledWith({ orgId: "o1", clientId: "case" });
    });

    it("shows a refused save in the editor and keeps it open", async () => {
      signedIn();
      mockTauri({
        org_status: () => orgStatus,
        summary_prompts_list: rows,
        org_admin_orgs: () => [{ id: "o1", name: "Northwind", require_2fa: false }],
        org_admin_content: () => serverContent,
        org_template_open: () => ({ name: "Case meeting", content: "x", workspaces: [], hidden: 0 }),
        org_template_save: () => {
          throw "Only Northwind's admins can change this.";
        },
      });
      render(<TemplatesSection language="en" />);
      await userEvent.click(await screen.findByRole("button", { name: "Edit Case meeting" }));
      const editor = await screen.findByRole("dialog", { name: "Edit organisation template" });
      await userEvent.click(within(editor).getByRole("button", { name: "Save template" }));
      expect(await within(editor).findByRole("alert")).toHaveTextContent("Only Northwind's admins can change this.");
    });

    it("shows a refused delete under the group", async () => {
      signedIn();
      mockTauri({
        org_status: () => orgStatus,
        summary_prompts_list: rows,
        org_admin_orgs: () => [{ id: "o1", name: "Northwind", require_2fa: false }],
        org_admin_content: () => serverContent,
        org_template_delete: () => {
          throw "Couldn't reach the server. Try again when you're online.";
        },
      });
      render(<TemplatesSection language="en" />);
      await userEvent.click(await screen.findByRole("button", { name: "Delete Case meeting" }));
      await userEvent.click(screen.getByRole("button", { name: "Delete" }));
      const group = await orgGroup();
      expect(await within(group).findByRole("alert")).toHaveTextContent("Couldn't reach the server.");
    });

    it("asks for two-factor authentication before an admin can edit where it's required", async () => {
      signedIn(false);
      mockTauri({
        org_status: () => orgStatus,
        summary_prompts_list: rows,
        org_admin_orgs: () => [{ id: "o1", name: "Northwind", require_2fa: true }],
      });
      render(<TemplatesSection language="en" />);
      const group = await orgGroup();
      expect(
        await within(group).findByText(/Northwind requires two-factor authentication to edit its templates/),
      ).toBeInTheDocument();
      expect(within(group).queryByRole("button", { name: /^Edit / })).toBeNull();
      expect(within(group).queryByRole("button", { name: /new template/i })).toBeNull();
    });
  });
  describe("as an admin of an organisation outside the user's departments", () => {
    const policy = {
      org_name: null,
      reason: null,
      local_only: false,
      retention_days: 0,
      telemetry_off: false,
      audio_forbidden: false,
      rules: [],
    };
    const content = {
      templates: [
        { client_id: "board", name: "Board minutes", content: "Motions and votes.", workspaces: [] },
        { client_id: "audit", name: "Audit review", content: "Findings.", workspaces: ["x1", "x2"] },
      ],
      vocabulary: ["Fjord"],
    };

    function signedIn() {
      useCloudStore.setState({
        status: {
          ...DISCONNECTED,
          configured: true,
          logged_in: true,
          user: { id: "u1", email: "a@example.no", name: "A", verified: true },
        },
        ready: true,
      });
    }

    function handlers(extra: Record<string, (args: unknown) => unknown> = {}) {
      return {
        org_status: () => ({ policy, orgs: [] }),
        summary_prompts_list: () => [prompt("p1", "Standup")],
        org_admin_orgs: () => [{ id: "o2", name: "Fjordby", require_2fa: false }],
        org_admin_content: () => content,
        ...extra,
      };
    }

    it("lists its templates from the server, editable, and caches none of them", async () => {
      signedIn();
      const asked = vi.fn(() => content);
      const created = vi.fn();
      const updated = vi.fn();
      mockTauri(handlers({ org_admin_content: asked, summary_prompts_create: created, summary_prompts_update: updated }));
      render(<TemplatesSection language="en" />);
      const group = (await screen.findByRole("heading", { name: "From your organisation" })).closest("section")!;
      expect(await within(group).findByRole("button", { name: "Edit Board minutes" })).toBeInTheDocument();
      expect(within(group).getByText("2 departments")).toBeInTheDocument();
      expect(within(group).queryByRole("button", { name: /Managed by/ })).toBeNull();
      expect(asked).toHaveBeenCalledWith({ orgId: "o2" });
      expect(created).not.toHaveBeenCalled();
      expect(updated).not.toHaveBeenCalled();
    });

    it("creates one for every department and reloads from the server", async () => {
      signedIn();
      const asked = vi.fn(() => content);
      const saved = vi.fn(() => null);
      mockTauri(handlers({ org_admin_content: asked, org_template_save: saved }));
      render(<TemplatesSection language="en" />);
      const group = (await screen.findByRole("heading", { name: "From your organisation" })).closest("section")!;
      await userEvent.click(await within(group).findByRole("button", { name: /new template/i }));
      const editor = await screen.findByRole("dialog", { name: "New organisation template" });
      expect(within(editor).queryByRole("radio")).toBeNull();
      await userEvent.type(within(editor).getByLabelText("Name"), "Agenda");
      await userEvent.click(within(editor).getByRole("button", { name: "Save template" }));
      expect(saved).toHaveBeenCalledWith({ orgId: "o2", clientId: null, name: "Agenda", content: "", workspaces: null });
      await vi.waitFor(() => expect(asked).toHaveBeenCalledTimes(2));
    });

    it("keeps the departments a template already names", async () => {
      signedIn();
      const saved = vi.fn(() => null);
      mockTauri(
        handlers({
          org_template_open: () => ({ name: "Audit review", content: "Findings.", workspaces: [], hidden: 2 }),
          org_template_save: saved,
        }),
      );
      render(<TemplatesSection language="en" />);
      await userEvent.click(await screen.findByRole("button", { name: "Edit Audit review" }));
      const editor = await screen.findByRole("dialog", { name: "Edit organisation template" });
      expect(within(editor).getByRole("radio", { name: "Some departments" })).toHaveAttribute("aria-checked", "true");
      expect(within(editor).queryByRole("checkbox")).toBeNull();
      expect(within(editor).getByText("Also offered in 2 departments you're not in.")).toBeInTheDocument();
      await userEvent.click(within(editor).getByRole("button", { name: "Save template" }));
      expect(saved).toHaveBeenCalledWith(expect.objectContaining({ orgId: "o2", clientId: "audit", workspaces: [] }));
    });

    it("asks nothing and shows nothing for a user in no organisation who administers none", async () => {
      signedIn();
      const asked = vi.fn();
      mockTauri({ summary_prompts_list: () => [prompt("p1", "Standup")], org_admin_orgs: () => [], org_admin_content: asked });
      render(<TemplatesSection language="en" />);
      await screen.findByText("Standup");
      await new Promise((r) => setTimeout(r, 0));
      expect(asked).not.toHaveBeenCalled();
      expect(screen.queryByRole("alert")).toBeNull();
      expect(screen.getAllByRole("heading", { level: 2 }).map((h) => h.textContent)).toEqual([
        "Built-in",
        "Your templates",
      ]);
    });

    it("skips the listing entirely when not signed in", async () => {
      const listed = vi.fn(() => []);
      mockTauri({ org_admin_orgs: listed });
      render(<TemplatesSection language="en" />);
      await screen.findByRole("heading", { name: "Built-in" });
      await new Promise((r) => setTimeout(r, 0));
      expect(listed).not.toHaveBeenCalled();
    });
  });
});
