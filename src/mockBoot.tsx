// Visual-check harness (dev only, never bundled into the app — `mock.html` is
// not an entry in the Tauri build). Renders real components against a mocked
// Tauri IPC so layout, spacing and token usage can be eyeballed in a browser.
// Unit tests assert behaviour; this catches the things jsdom cannot see —
// unlayered utilities losing to Tailwind, hints stacking into a wall of text,
// a control that renders but is invisible.
//
// Pick a scenario with ?case=<name>. Add scenarios to CASES below. Each one
// names the step to render and the IPC answers that put it in the state under
// review, so scenarios from past reviews stay runnable as the harness moves on
// to the next step.
import ReactDOM from "react-dom/client";
import { StrictMode, useState } from "react";
import { mockIPC } from "@tauri-apps/api/mocks";
import {
  Check,
  Copy,
  FileText as Files,
  Folder as FolderIcon,
  MessageCircle,
  Search,
} from "lucide-react";
import { CommandSnippet } from "./components/CommandSnippet";
import { Segmented } from "./pages/settings/components/Segmented";
import { Toggle } from "./pages/settings/components/Toggle";
import { NoteTitleBox, TranscriptEditor, TranscriptPlayer } from "./pages/Note";
import { IntegrationsSection } from "./pages/settings/tabs/Integrations";
import { RecordingSection } from "./pages/settings/tabs/Recording";
import { NewWorkspaceModal } from "./components/NewWorkspaceModal";
import { DISCONNECTED, useCloudStore, type CloudStatus, type CloudWorkspace } from "./lib/cloud";
import { DEFAULTS, type EditableKey } from "./pages/settings/types";
import { SummaryStep } from "./pages/onboarding/steps/Summary";
import { TranscriptionStep } from "./pages/onboarding/steps/Transcription";
import { STEP_ORDER, type StepContext, type StepId } from "./pages/onboarding/types";
import type { ProviderConfig, TimelineEntry } from "./lib/ipc";
// Mirrors src/main.tsx — every theme's typeface, so a scenario reviewed under
// `?palette=<id>` renders in that design's face rather than falling back.
import "@fontsource/hanken-grotesk/400.css";
import "@fontsource/hanken-grotesk/500.css";
import "@fontsource/hanken-grotesk/600.css";
import "@fontsource/hanken-grotesk/700.css";
import "@fontsource/dm-mono/400.css";
import "./styles/globals.css";

type Handler = (args: unknown) => unknown;
// A scenario carries everything that varies between steps — which step it is
// and how to render it — so adding a third step means writing one more
// `*Case` builder, not extending a union and every switch on it. The wizard
// position is derived from STEP_ORDER rather than written down, so a step
// moving in the real wizard can't leave the harness claiming the old slot.
// `step` is what the onboarding wrapper needs; a scenario for a component that
// isn't a wizard step leaves it off. `wrap` is the container the scenario is
// rendered into — a function rather than a named variant, so a new kind of
// scenario is still one more builder and never a union with a switch on it
// (see the file header).
type Scenario = {
  step?: StepId;
  wrap?: (node: React.ReactNode) => React.ReactNode;
  render: (ctx: StepContext) => React.ReactNode;
  ipc: Record<string, Handler>;
};

// ---- #147 axis: which Ollama models are installed --------------------------
function summaryCase(installed: string[] | "unreachable"): Scenario {
  return {
    step: "summary",
    render: (ctx) => <SummaryStep ctx={ctx} />,
    ipc: {
      local_llm_list_models: () => {
        if (installed === "unreachable") throw new Error("connection refused");
        return installed;
      },
      provider_key_get: () => null, // no OpenAI key → neutral framing
    },
  };
}

// ---- #149 axis: what a returning user's stored config resumes onto ---------
// The step pre-selects a card from `transcribe_config`, but only counts a
// cloud default as chosen once its key is present (see transcribeDefault.ts),
// so keyless-openai must look exactly like a fresh install.
function resumeCase(
  def: ProviderConfig | null,
  keys: Record<string, string> = {},
): Scenario {
  return {
    step: "transcription",
    render: (ctx) => <TranscriptionStep ctx={ctx} />,
    ipc: {
      get_transcribe_config: () => (def ? { default: def, per_language: {} } : null),
      provider_key_get: (args) => keys[(args as { provider: string }).provider] ?? null,
      provider_key_test: () => ({ ok: true, status: 200, error: null }),
      local_whisper_models: () => [
        {
          id: "large-v3-turbo-q5",
          label: "Large v3 Turbo (quantized)",
          description: "The recommended default for almost all use.",
          filename: "ggml-large-v3-turbo-q5_0.bin",
          sizeBytesHint: 574_000_000,
          kind: "multilingual",
          specificLanguage: null,
          downloaded: false,
          sizeBytes: null,
          path: null,
        },
      ],
      system_arch: () => "aarch64",
      diarize_status: () => ({ downloaded: true, sizeBytes: 30_000_000, path: "/x" }),
    },
  };
}

// ---- #168 axis: the free-text transcript editor's mode header --------------
// The panel wrapper mirrors the real Transcript tab (Note.tsx: the
// `flex-1 min-h-0 flex flex-col px-4 py-4` column inside the right context
// panel) — the editor sizes itself against that column, so a plain block
// wrapper would collapse it and read as a layout bug in the component.
function transcriptCase(disabled: boolean): Scenario {
  return {
    wrap: (node) => (
      <div className="flex-1 min-h-0 flex justify-center px-6 py-8">
        <div className="w-full max-w-[420px] rounded-[var(--radius-card)] bg-[var(--color-surface)] flex flex-col min-h-0 px-4 py-4">
          {node}
        </div>
      </div>
    ),
    render: () => <TranscriptEditorHarness disabled={disabled} />,
    ipc: {},
  };
}

function TranscriptEditorHarness({ disabled }: { disabled: boolean }) {
  const [text, setText] = useState(
    "Michael: Skal vi ta gjennomgangen nå?\nHege: Ja, jeg har notatene klare.\nMichael: Bra — da starter vi med tallene fra forrige kvartal.",
  );
  return (
    <TranscriptEditor
      value={text}
      onChange={setText}
      disabled={disabled}
      fill
      bottomAligned={false}
    />
  );
}

// ---- #170 axis: per-turn editing on a timeline-backed note ----------------
// The reader renders from the timeline, so editing must too. What needs eyes:
// the hover pencil sitting beside the delete ×, and the open textarea keeping
// the turn's place in the flow instead of jumping the page.
function playerCase(disabled: boolean): Scenario {
  return {
    wrap: (node) => (
      <div className="flex-1 min-h-0 flex justify-center px-6 py-8">
        <div className="w-full max-w-[420px] rounded-[var(--radius-card)] bg-[var(--color-surface)] flex flex-col min-h-0 px-4 py-4">
          {node}
        </div>
      </div>
    ),
    render: () => <TranscriptPlayerHarness disabled={disabled} />,
    ipc: {
      note_session_playback_path: () => null,
      note_timeline_set_chunk_text: () => null,
      cloud_upload_note_sessions: () => null,
    },
  };
}

function TranscriptPlayerHarness({ disabled }: { disabled: boolean }) {
  const turn = (
    chunkIdx: number,
    label: string,
    text: string,
    startMs: number,
  ): TimelineEntry => ({
    start_ms: startMs,
    end_ms: startMs + 3000,
    label,
    text,
    words: text.split(" ").map((w, i) => ({
      text: w,
      start_ms: startMs + i * 300,
      end_ms: startMs + i * 300 + 300,
    })),
    sessionId: "s1",
    sessionIndex: 0,
    chunkIdx,
  });
  const [timeline, setTimeline] = useState<TimelineEntry[]>([
    turn(0, "Michael", "Skal vi ta gjennomgangen nå?", 0),
    turn(1, "Hege", "Ja, jeg har notatene klare", 3000),
    turn(2, "Hege", "og tallene fra forrige kvartal", 6000),
    turn(3, "Michael", "Bra — da starter vi der.", 9000),
  ]);
  return (
    <TranscriptPlayer
      noteId="mock-note"
      timeline={timeline}
      setTimeline={setTimeline}
      sessions={[
        {
          id: "s1",
          index: 1,
          startedAt: new Date().toISOString(),
          durationMs: 12000,
          streams: ["mic"],
          hasPlayback: false,
        },
      ]}
      fallbackPlaybackUrl={null}
      audioAvailable={false}
      keepAudio={false}
      disabled={disabled}
      fill
      bottomAligned={false}
    />
  );
}

// ---- #172 axis: the MCP integration section in Settings --------------------
// What needs eyes here is the multi-line Codex snippet: CommandSnippet's block
// mode wraps instead of truncating, and a config stanza next to a Copy button
// is the one row in this section that isn't a plain control row.
//
// The wrapper mirrors SettingsLayout's content column (`max-w-2xl mx-auto px-8
// py-7`) — the Section card fills its container, so a full-width wrapper would
// stretch the snippets far past what they ever get in the real dialog.
function integrationsCase(enabled: boolean): Scenario {
  return {
    wrap: (node) => (
      <div className="flex-1 min-h-0 overflow-y-auto">
        <div className="max-w-2xl mx-auto px-8 py-7">{node}</div>
      </div>
    ),
    render: () => <IntegrationsHarness enabled={enabled} />,
    ipc: {
      mcp_server_path: () => "/Applications/Humla.app/Contents/MacOS/humla-mcp",
    },
  };
}

function IntegrationsHarness({ enabled }: { enabled: boolean }) {
  const [on, setOn] = useState(enabled ? "true" : "false");
  const s = { ...DEFAULTS, mcp_enabled: on } as Record<EditableKey, string>;
  return <IntegrationsSection s={s} update={(_k, v) => setOn(v)} />;
}

// ---- #21 axis: menu-bar mode in the Recording section ----------------------
// Same wrapper as the Integrations case, and for the same reason: the section
// has to be judged inside SettingsLayout's content column, not stretched.
// Both toggle states, because the close-to-tray copy swaps entirely — it names
// which regime is in force rather than describing the switch.
function menubarCase(closeToTray: boolean, hotkey: string): Scenario {
  return {
    wrap: (node) => (
      <div className="flex-1 min-h-0 overflow-y-auto">
        <div className="max-w-2xl mx-auto px-8 py-7">{node}</div>
      </div>
    ),
    render: () => <MenubarHarness closeToTray={closeToTray} />,
    ipc: {
      record_hotkey_get: () => hotkey,
      record_hotkey_set: () => null,
      permissions_status: () => ({ microphone: "granted", screen: "granted" }),
      stored_audio_stats: () => ({ notes: 0, files: 0, bytes: 0, noteIds: [] }),
    },
  };
}

function MenubarHarness({ closeToTray }: { closeToTray: boolean }) {
  const [on, setOn] = useState(closeToTray ? "true" : "false");
  const s = { ...DEFAULTS, close_to_tray: on } as Record<EditableKey, string>;
  return <RecordingSection s={s} update={async (_k, v) => setOn(v)} />;
}

// ---- workspace-creation axis: the sheet's five stages ----------------------
// Five separate scenarios rather than one clickable flow: the stages are DERIVED
// from cloud status, so seeding the status is how you reach one — and each is a
// distinct layout to judge (a pitch panel, a form, a named field, a wait state,
// a growing list). No `wrap`: the sheet portals to <body> and covers the
// viewport, which is exactly what it does in the app.
function workspaceCase(stage: "connect" | "auth" | "name" | "trial" | "invite"): Scenario {
  const ws = (plan: CloudWorkspace["plan_status"]): CloudWorkspace => ({
    id: "w1",
    name: "Acme Inc",
    role: "owner",
    plan_status: plan,
  });
  const signedIn: CloudStatus = {
    ...DISCONNECTED,
    configured: true,
    logged_in: true,
    base_url: "https://sync.humla.team",
    user: { id: "u1", email: "michael@example.no", name: "Michael", verified: true },
    billing_enabled: true,
    seat_price_cents: 500,
    seat_currency: "usd",
  };
  const status: CloudStatus =
    stage === "connect"
      ? { ...DISCONNECTED, configured: false }
      : stage === "auth"
        ? { ...signedIn, logged_in: false, user: null }
        : stage === "name"
          ? signedIn
          : {
              ...signedIn,
              current_workspace: ws(stage === "trial" ? "none" : "trialing"),
              workspaces: [ws(stage === "trial" ? "none" : "trialing")],
            };
  return {
    wrap: (node) => <div className="flex-1 min-h-0">{node}</div>,
    render: () => <WorkspaceHarness status={status} pinned={stage === "trial" || stage === "invite"} />,
    ipc: {
      cloud_status: () => status,
      cloud_create_workspace: () => ws("none"),
      cloud_invite_member: () => "invited",
      cloud_billing_checkout: () => "https://checkout.stripe.test/x",
    },
  };
}

function WorkspaceHarness({ status, pinned }: { status: CloudStatus; pinned: boolean }) {
  // Seed before the first paint so the sheet never renders a stage it is about
  // to leave — the store is the component's only input.
  const [ready] = useState(() => {
    useCloudStore.setState({ status, ready: true });
    return true;
  });
  // `pinned` scenarios work an EXISTING workspace, which is how the read-only
  // banner enters; the others create one, so they pass no id.
  return ready ? (
    <NewWorkspaceModal
      open
      onClose={() => console.log("[mock] close")}
      workspaceId={pinned ? "w1" : null}
    />
  ) : null;
}

// ---- theme axis: every token-driven surface on one page --------------------
// Real components where one exists (Toggle, Segmented, CommandSnippet's mono
// block); the utility classes themselves where the component is a page (a nav
// row, a bar, a badge). The point is to see a whole design at once — type scale,
// control language, icon size, row rhythm — not to exercise behaviour.
function ThemeSpecimen() {
  const [on, setOn] = useState(true);
  const [seg, setSeg] = useState("balanced");
  return (
    <div className="max-w-3xl mx-auto flex flex-col gap-7">
      <div>
        <div className="nd-title">Weekly sync with Hege</div>
        <p className="prose-note mt-2">
          Body copy in the theme’s own size, leading and tracking. Long enough to
          show the measure and how the line spacing reads over more than one line
          of actual prose.
        </p>
      </div>

      <div className="flex flex-col gap-2">
        <div className="nd-label">Controls</div>
        <div className="flex flex-wrap items-center gap-3">
          <button className="nd-btn-primary nd-btn">
            <Check size={14} strokeWidth={2} />
            Summarise
          </button>
          <button className="nd-btn">
            <Copy size={14} strokeWidth={2} />
            Secondary
          </button>
          <button className="nd-btn-icon" aria-label="Icon button">
            <Copy strokeWidth={1.6} />
          </button>
          <button className="nd-btn-icon is-active" aria-label="Active icon button">
            <Check strokeWidth={1.6} />
          </button>
          <span className="nd-badge">Synced</span>
          <Toggle checked={on} onChange={setOn} label="A switch" />
        </div>
        <Segmented
          label="Quality"
          value={seg}
          onChange={setSeg}
          options={[
            { value: "fast", label: "Fast" },
            { value: "balanced", label: "Balanced" },
            { value: "quality", label: "Quality" },
          ]}
        />
      </div>

      <div className="flex flex-col gap-2">
        <div className="nd-label">Navigation + bars</div>
        <div className="nd-bar max-w-sm">
          <Search strokeWidth={1.5} className="text-[var(--color-icon)] shrink-0" />
          <input placeholder="Search notes" className="flex-1 text-sm min-w-0 bg-transparent" />
        </div>
        <div className="nd-list max-w-sm mt-1">
          <div className="nd-navrow is-active">
            <Files strokeWidth={1.6} className="shrink-0 opacity-85" />
            <span className="flex-1 truncate">All notes</span>
            <span className="text-[11px] text-[var(--color-text-disabled)]">12</span>
          </div>
          <div className="nd-navrow">
            <MessageCircle strokeWidth={1.6} className="shrink-0 opacity-85" />
            <span className="flex-1 truncate">Chat</span>
          </div>
          <div className="nd-navrow">
            <FolderIcon strokeWidth={1.6} className="shrink-0 opacity-85" />
            <span className="flex-1 truncate">Kundemøter</span>
          </div>
        </div>
      </div>

      <div className="flex flex-col gap-2">
        <div className="nd-label">Commands</div>
        <CommandSnippet command="ollama pull gemma4:12b-mlx" />
      </div>

      <div className="flex flex-col gap-2">
        <div className="nd-label">Tokens</div>
        <div className="flex flex-wrap gap-2">
          {[
            "--color-canvas",
            "--color-sidebar-bg",
            "--color-surface",
            "--color-surface-2",
            "--color-surface-raised",
            "--color-accent",
            "--color-accent-soft",
            "--color-record",
            "--color-danger",
            "--color-interactive",
            "--color-success",
            "--color-warning",
            "--color-speaker-4",
          ].map((t) => (
            <div key={t} className="flex items-center gap-2 text-[11px] text-[var(--color-text-muted)]">
              <span
                className="w-5 h-5 rounded border border-[var(--color-line-visible)]"
                style={{ background: `var(${t})` }}
              />
              {t.replace("--color-", "")}
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}

// ---- #90 axis: is a title being written for this note right now? -----------
//
// A layout question, which is why it's here: the shimmer stands IN for the
// title box, so its line box has to be exactly the height the real title
// occupies — otherwise the meta row under it jumps when the call lands. Both
// cases render inside the note body's own column so the width is honest.
function titleCase(writing: boolean, title: string): Scenario {
  return {
    wrap: (node) => (
      <div className="flex-1 min-h-0 overflow-y-auto">
        <div className="mx-auto w-full px-12 pt-3 pb-32 max-w-[640px]">{node}</div>
      </div>
    ),
    render: () => (
      <>
        <NoteTitleBox title={title} onChange={() => {}} readOnly={false} writing={writing} />
        {/* The meta row is the thing that must not move. */}
        <div className="mb-8 pb-4 border-b border-[var(--color-line)]">
          <div className="flex flex-wrap items-center gap-1 -mx-2">
            <span className="nd-meta">19 Aug 2026</span>
            <span className="nd-meta is-interactive">Kundemøter</span>
          </div>
        </div>
        <p className="text-[var(--color-text-muted)]">
          Body copy, so the column below the title has something in it.
        </p>
      </>
    ),
    ipc: {},
  };
}

const CASES: Record<string, Scenario> = {
  // --- #90: the automatic titler's two states. Compare `title-writing` against
  // `title-idle` and `title-idle-long` — nothing below the title may shift.
  "title-idle": titleCase(false, "Recording 19 Aug 14:32"),
  "title-writing": titleCase(true, "Recording 19 Aug 14:32"),
  "title-idle-long": titleCase(
    false,
    "Oppstartsmøte med Hege om lanseringsplanen og alt som gjenstår",
  ),

  // --- #21: menu-bar mode. Off (the default), on, and with no shortcut set.
  menubar: menubarCase(false, "Command+Control+KeyR"),
  "menubar-on": menubarCase(true, "Command+Control+KeyR"),
  "menubar-nohotkey": menubarCase(false, ""),

  // --- #172: the MCP switch, off (the default) and on (snippets revealed).
  "mcp-off": integrationsCase(false),
  "mcp-on": integrationsCase(true),

  // --- #147: the issue's exact report — recommended model already pulled.
  recommended: summaryCase(["gemma4:12b-mlx", "embeddinggemma"]),
  // 16 GB tier fallback.
  "16gb": summaryCase(["qwen3.5:4b"]),
  // Neither recommendation — the merged branch: ✓ + upgrade hint + pull + picker.
  fallback: summaryCase(["llama3.2:3b", "mistral:7b"]),
  // Models present but none can chat.
  "embedding-only": summaryCase(["embeddinggemma"]),
  // No models at all.
  empty: summaryCase([]),
  unreachable: summaryCase("unreachable"),

  // --- #149: the regression — a stored OpenAI default WITH a key must resume
  // onto the Cloud card, showing the stored-key sentinel and a live Test.
  "resume-openai": resumeCase({ provider: "openai", model: "whisper-1" }, {
    openai: "sk-stored",
  }),
  // The same config with no key IS the fresh install: nothing selected.
  "resume-openai-nokey": resumeCase({ provider: "openai", model: "whisper-1" }),
  // The path that already worked, for comparison.
  "resume-deepgram": resumeCase({ provider: "deepgram", model: "nova-3" }, {
    deepgram: "dg-stored",
  }),
  // On-device resume — the other pre-select branch, unchanged by #149.
  "resume-local": resumeCase({
    provider: "local",
    model_id: "large-v3-turbo-q5",
    preset: "quality",
    use_gpu: true,
  }),
  // No config at all.
  "resume-none": resumeCase(null),

  // --- #168: the free-text transcript editor (notes with no timeline).
  transcript: transcriptCase(false),
  // Recording in flight — no mode to enter, so the header slot is empty.
  "transcript-recording": transcriptCase(true),

  // --- #170: per-turn editing on a timeline-backed note.
  player: playerCase(false),
  // Recording in flight — no edit or delete affordance on any turn.
  "player-recording": playerCase(true),

  // --- The create-team-workspace sheet, one scenario per derived stage.
  "ws-connect": workspaceCase("connect"),
  "ws-auth": workspaceCase("auth"),
  "ws-name": workspaceCase("name"),
  "ws-trial": workspaceCase("trial"),
  "ws-invite": workspaceCase("invite"),

  // --- Themes: the token-driven chrome on one page, so a theme can be judged
  // as a design rather than as a diff. Combine with ?palette= and ?theme=.
  themes: {
    wrap: (node) => <div className="flex-1 min-h-0 overflow-y-auto px-8 py-6">{node}</div>,
    render: () => <ThemeSpecimen />,
    ipc: {},
  },
};

const params = new URLSearchParams(location.search);
const which = params.get("case") ?? "recommended";
const scenario = CASES[which] ?? CASES.recommended;

// The theme axes are attributes on <html> — exactly what palette.ts and theme.ts
// write at runtime — so the harness sets them the same way instead of faking a
// store. ?palette=<id> picks the design, ?theme=light|dark pins the mode
// (omitted = follow the OS, which is what "System" does in the app).
document.documentElement.setAttribute("data-palette", params.get("palette") ?? "warm");
const mode = params.get("theme");
if (mode === "light" || mode === "dark") {
  document.documentElement.setAttribute("data-theme", mode);
}

mockIPC(async (cmd, args) => {
  if (cmd in scenario.ipc) return scenario.ipc[cmd](args);
  if (cmd === "settings_set") {
    console.log("[mock] settings_set", args);
    return null;
  }
  if (cmd === "settings_get") return null;
  if (cmd.startsWith("plugin:")) return undefined;
  return null;
});

const ctx = {
  stepId: scenario.step,
  index: scenario.step ? STEP_ORDER.indexOf(scenario.step) : 0,
  total: STEP_ORDER.length,
  goNext: () => console.log("[mock] goNext"),
  goBack: () => {},
  goTo: () => {},
  canGoBack: true,
  complete: () => {},
} as unknown as StepContext;

// A wrapper MUST mirror the real container the component lives in. This one is
// the wizard shell (Onboarding.tsx: the `flex-1 … flex items-center
// justify-center px-6 py-16` canvas), and is the default for wizard steps.
// StepShell is `w-full max-w-lg` and centres its own contents but never
// itself, so a plain block wrapper pins the whole step to the left edge — a
// harness artefact that looks exactly like a layout bug in the component under
// review. Scenarios for non-wizard components bring their own `wrap`.
const onboardingCanvas = (node: React.ReactNode) => (
  <div className="flex-1 min-h-0 overflow-y-auto flex items-center justify-center px-6 py-16">
    {node}
  </div>
);

ReactDOM.createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <div className="relative h-screen w-full flex flex-col bg-[var(--color-canvas)]">
      <p className="pt-4 text-center text-xs text-[var(--color-text-muted)]">
        mock case: <code>{which}</code> — {Object.keys(CASES).join(" · ")}
      </p>
      {(scenario.wrap ?? onboardingCanvas)(scenario.render(ctx))}
    </div>
  </StrictMode>,
);
