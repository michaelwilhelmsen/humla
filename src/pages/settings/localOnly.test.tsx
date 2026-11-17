import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { SummaryTab } from "./tabs/Summary";
import { ChatTab } from "./tabs/Chat";
import { TranscriptionTab } from "./tabs/Transcription";
import { IntegrationsSection } from "./tabs/Integrations";
import { mockTauri } from "../../test/tauri";
import type { ProviderConfig } from "../../lib/ipc";
import { NO_POLICY, type EffectivePolicy } from "../../lib/orgPolicy";
import { DEFAULTS, EMPTY_DIARIZE_STATE, EMPTY_LOCAL_STATE, type EditableKey } from "./types";

const LOCAL_ONLY: EffectivePolicy = {
  ...NO_POLICY,
  org_name: "Northwind",
  reason: "Nothing leaves this Mac.",
  local_only: true,
  rules: [{ kind: "local_only", source: "Northwind", reason: "Nothing leaves this Mac." }],
};

function settings(overrides: Partial<Record<EditableKey, string>> = {}) {
  return { ...DEFAULTS, ...overrides } as Record<EditableKey, string>;
}

let keyReads: string[] = [];

beforeEach(() => {
  keyReads = [];
  mockTauri({
    org_status: () => ({ policy: LOCAL_ONLY }),
    mcp_server_path: () => "/Applications/Humla.app/Contents/MacOS/humla-mcp",
    provider_key_get: (args) => {
      keyReads.push((args as { provider: string }).provider);
      return "stored";
    },
    webhook_get_endpoint: () => ({ url: "", headers: [] }),
  });
});

const managedBadge = () => screen.findAllByRole("button", { name: /Managed by Northwind/ });

describe("under local_only", () => {
  it("summaries show the local server, fixed, with no cloud model or key", async () => {
    render(<SummaryTab s={settings({ summary_provider: "anthropic" })} update={async () => {}} />);
    await managedBadge();
    expect(screen.getByRole("combobox", { name: /Local \(any OpenAI-compatible server\)/ })).toBeDisabled();
    expect(screen.queryByLabelText("Anthropic API key")).toBeNull();
    expect(keyReads).toEqual([]);
  });

  it("summaries keep a custom endpoint on this Mac", async () => {
    render(
      <SummaryTab
        s={settings({ summary_provider: "custom", custom_base_url: "http://localhost:1234/v1" })}
        update={async () => {}}
      />,
    );
    await managedBadge();
    expect(screen.getByRole("combobox", { name: /Custom/ })).toBeDisabled();
  });

  it("chat shows the local server, fixed, with no cloud key", async () => {
    render(<ChatTab s={settings({ chat_provider: "openai" })} update={async () => {}} />);
    await managedBadge();
    expect(screen.getByRole("combobox", { name: /^Local/ })).toBeDisabled();
    expect(screen.queryByLabelText("OpenAI API key")).toBeNull();
  });

  it("transcription shows on-device Whisper for the default and each override", async () => {
    const config = {
      default: { provider: "deepgram", model: "nova-3" } as ProviderConfig,
      per_language: { no: { provider: "groq", model: "whisper-large-v3-turbo" } as ProviderConfig },
    };
    render(
      <TranscriptionTab
        s={settings()}
        update={vi.fn()}
        transcribeConfig={config}
        setDefaultConfig={vi.fn()}
        setLanguageOverride={vi.fn()}
        removeLanguageOverride={vi.fn()}
        local={EMPTY_LOCAL_STATE}
        downloadModel={vi.fn()}
        deleteModel={vi.fn()}
        diarize={{ nemotron3: EMPTY_DIARIZE_STATE, community1: EMPTY_DIARIZE_STATE }}
        downloadDiarize={vi.fn()}
        deleteDiarize={vi.fn()}
      />,
    );
    await managedBadge();
    expect(screen.getAllByRole("combobox").find((c) => c.textContent?.includes("Local"))).toBeDisabled();
    expect(screen.queryByLabelText("Deepgram API key")).toBeNull();
    await userEvent.click(screen.getAllByRole("button", { name: /Advanced/ })[0]);
    expect(screen.getByText(/^Local · large-v3-turbo-q5/)).toBeInTheDocument();
    expect(screen.queryByText(/Groq ·/)).toBeNull();
    expect(screen.queryByText("Other API keys")).toBeNull();
    expect(await managedBadge()).toHaveLength(2);
  });

  it("the MCP server is off and can't be turned on", async () => {
    const update = vi.fn();
    render(<IntegrationsSection s={settings({ mcp_enabled: "true" })} update={update} />);
    await managedBadge();
    const toggle = screen.getByRole("switch", { name: /MCP server/i });
    expect(toggle).toHaveAttribute("aria-checked", "false");
    expect(toggle).toBeDisabled();
    expect(screen.queryByText(/claude mcp add/)).toBeNull();
  });

  it("the webhook URL is marked as managed", async () => {
    render(<IntegrationsSection s={settings({ webhook_enabled: "true" })} update={vi.fn()} />);
    await screen.findByLabelText("Webhook URL");
    await waitFor(() => expect(screen.getAllByRole("button", { name: /Managed by Northwind/ })).toHaveLength(2));
    expect(screen.getByLabelText("Webhook URL")).toHaveAttribute("placeholder", "http://localhost:8787/webhook");
  });
});
