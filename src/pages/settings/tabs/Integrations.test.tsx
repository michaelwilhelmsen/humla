import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { IntegrationsSection } from "./Integrations";
import { mockTauri } from "../../../test/tauri";
import { DEFAULTS, type EditableKey } from "../types";

const PATH = "/Applications/Humla.app/Contents/MacOS/humla-mcp";

function settings(overrides: Partial<Record<EditableKey, string>> = {}) {
  return { ...DEFAULTS, ...overrides } as Record<EditableKey, string>;
}

beforeEach(() => {
  mockTauri({ mcp_server_path: () => PATH });
});

describe("the MCP integration switch", () => {
  it("is off out of the box, and offers no way in until it is turned on", async () => {
    render(<IntegrationsSection s={settings()} update={() => {}} />);
    expect(screen.getByRole("switch", { name: /MCP server/i })).toHaveAttribute(
      "aria-checked",
      "false",
    );
    // The whole point of the gate: an update must not quietly hand an agent a
    // working command.
    await waitFor(() => expect(screen.queryByText(/claude mcp add/)).toBeNull());
    expect(screen.queryByText(/mcp_servers/)).toBeNull();
  });

  it("persists the choice as the string the backend reads", async () => {
    const update = vi.fn();
    render(<IntegrationsSection s={settings()} update={update} />);
    await userEvent.click(screen.getByRole("switch", { name: /MCP server/i }));
    expect(update).toHaveBeenCalledWith("mcp_enabled", "true");
  });

  it("shows a working snippet for both clients once enabled", async () => {
    render(<IntegrationsSection s={settings({ mcp_enabled: "true" })} update={() => {}} />);
    // Both snippets name the real binary on THIS install — the path differs
    // between a bundle and a dev build, which is why it comes from the backend.
    const claude = await screen.findByText(/claude mcp add/);
    expect(claude.textContent).toContain(PATH);
    const codex = screen.getByText(/\[mcp_servers\.humla\]/);
    expect(codex.textContent).toContain(PATH);
    // Quoted: an .app path contains a space, and an unquoted one silently
    // becomes two arguments.
    expect(claude.textContent).toContain(`"${PATH}"`);
  });

  it("says plainly that it is read-only and that audio stays out of reach", () => {
    render(<IntegrationsSection s={settings()} update={() => {}} />);
    expect(screen.getByText(/Read-only/)).toBeInTheDocument();
    expect(screen.getByText(/audio is never reachable/)).toBeInTheDocument();
  });
});
