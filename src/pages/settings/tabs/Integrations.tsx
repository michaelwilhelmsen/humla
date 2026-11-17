import { useEffect, useState } from "react";
import { open as openExternal } from "@tauri-apps/plugin-shell";
import { ipc } from "../../../lib/ipc";
import { CommandSnippet } from "../../../components/CommandSnippet";
import { Row, Section } from "../components/Section";
import { Toggle } from "../components/Toggle";
import type { EditableKey } from "../types";
import { WebhookRows } from "./Webhook";

// The ways the user's own tools reach their notes: the MCP server (#172), which
// they read from, and the webhook (#213, `WebhookRows`), which a note is sent to.
//
// The MCP integration: one switch, and a config snippet per client.
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
const MCP_GUIDE_URL = "https://humla.team/docs/integrations/mcp";

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
        description={
          <>
            Read-only access for Claude Code, Codex and other MCP clients. Audio is never reachable.{" "}
            <button
              type="button"
              onClick={() => void openExternal(MCP_GUIDE_URL)}
              className="underline hover:text-[var(--color-text)]"
            >
              Setup guide
            </button>
          </>
        }
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
              Any stdio MCP client can run the same command. It reads your current workspace, whether
              or not Humla is open.
            </p>
          </Row>
        </>
      )}
      <WebhookRows s={s} update={update} />
    </Section>
  );
}
