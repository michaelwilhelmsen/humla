import { useEffect, useState } from "react";
import { ipc } from "../../../lib/ipc";
import { CommandSnippet } from "../../../components/CommandSnippet";
import { Row, Section } from "../components/Section";
import { Toggle } from "../components/Toggle";
import type { EditableKey } from "../types";

// The MCP integration (#172): one switch, and a config snippet per client.
//
// Off by default and explicitly opt-in — this hands an outside agent the
// contents of real meetings, so installing an update must never quietly open
// them. The switch writes `mcp_enabled`, which the `humla-mcp` process reads on
// every call, so turning it off takes effect on the next tool call rather than
// at the client's next restart.
//
// Snippets rather than a one-click install: `.mcpb` bundles target the GUI
// Claude apps and don't work with Claude Code in the terminal, and a Claude Code
// plugin would serve only one of the two clients this is for.
export function IntegrationsSection({
  s,
  update,
}: {
  s: Record<EditableKey, string>;
  update: (key: EditableKey, value: string) => void;
}) {
  const enabled = s.mcp_enabled === "true";
  const [path, setPath] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    ipc
      .mcpServerPath()
      .then((p) => {
        if (!cancelled) setPath(p);
      })
      .catch(() => {
        /* the snippet section simply stays hidden */
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // Quoted, because an .app lives under a path with a space in it.
  const claudeCode = path ? `claude mcp add --scope user humla -- "${path}"` : "";
  const codex = path
    ? `[mcp_servers.humla]\ncommand = "${path}"`
    : "";

  return (
    <Section title="Integrations">
      <Row
        label="Model Context Protocol server"
        description="Let Claude Code, Codex or any other MCP client search and read your notes. Read-only: nothing can change or delete a note, and recorded audio is never reachable."
        control={
          <Toggle
            checked={enabled}
            onChange={(next) => update("mcp_enabled", next ? "true" : "false")}
            label="Enable the MCP server"
          />
        }
      />
      {enabled && path && (
        <>
          <Row label="Claude Code">
            <p className="text-xs text-[var(--color-text-muted)] mb-2">
              Run this once in a terminal.
            </p>
            {/* Wrapped, not truncated: the path is the part that differs per
                install, and it sits at the END of the line — truncating hides
                exactly the thing worth reading before pasting. */}
            <CommandSnippet block command={claudeCode} ariaLabel="Copy the Claude Code command" />
          </Row>
          <Row label="Codex">
            <p className="text-xs text-[var(--color-text-muted)] mb-2">
              Add this to <code style={{ fontFamily: "var(--font-code)" }}>~/.codex/config.toml</code>.
            </p>
            <CommandSnippet block command={codex} ariaLabel="Copy the Codex configuration" />
          </Row>
          <Row label="Anything else">
            <p className="text-xs text-[var(--color-text-muted)]">
              Any MCP client that speaks stdio can run this command directly. It
              works whether or not Humla is open, and reads whichever workspace
              you're currently in.
            </p>
          </Row>
        </>
      )}
    </Section>
  );
}
