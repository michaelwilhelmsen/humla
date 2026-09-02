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
import { MemoryRouter } from "react-router-dom";
import { StrictMode, useState } from "react";
import { mockIPC } from "@tauri-apps/api/mocks";
import {
  Check,
  Copy,
  FileText as Files,
  Folder as FolderIcon,
  MessageCircle,
  MessageSquare,
  Search,
} from "lucide-react";
import App from "./App";
import { mockTauri } from "./test/tauri";
import { makeNote } from "./test/fixtures";
import { CommandSnippet } from "./components/CommandSnippet";
import { RecordingBar } from "./components/RecordingBar";
import { useRecordingStore } from "./lib/store";
import { Segmented } from "./pages/settings/components/Segmented";
import { Toggle } from "./pages/settings/components/Toggle";
import { NoteTitleBox, NoteToolbar, PanelEmpty, TranscriptEditor, TranscriptPlayer } from "./pages/Note";
import { IntegrationsSection } from "./pages/settings/tabs/Integrations";
import { ChatTab } from "./pages/settings/tabs/Chat";
import { RecordingSection } from "./pages/settings/tabs/Recording";
import { NewWorkspaceModal } from "./components/NewWorkspaceModal";
import { DISCONNECTED, useCloudStore, type CloudStatus, type CloudWorkspace } from "./lib/cloud";
import { DEFAULTS, type EditableKey } from "./pages/settings/types";
import { SummaryStep } from "./pages/onboarding/steps/Summary";
import { TranscriptionStep } from "./pages/onboarding/steps/Transcription";
import { STEP_ORDER, type StepContext, type StepId } from "./pages/onboarding/types";
import type { ProviderConfig, TimelineEntry } from "./lib/ipc";
import { DEMO_CLIENTS, DEMO_FOLDERS, demoNotes } from "./test/noteLibrary";
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
  /** Renders the whole app at this route instead of one component, against the
      unit tests' own "empty but healthy" IPC defaults (`mockTauri`). For a
      question a single component can't answer — a panel's controls sitting in
      the panel, beside the pickers and the chrome they compete with. Costs a
      much bigger IPC surface, so it is the exception, not the pattern. */
  route?: string;
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
// `width` is the CONTEXT PANEL, which the user drags — `PANEL_FLOOR` (260) to
// `PANEL_MAX` (720), per Note.tsx. This card was pinned at 420 for a long time,
// which is how a turn's edit box shipped at a width that only looked right
// there (#176): judge the reader at the bounds, not at one comfortable middle.
function playerCase(disabled: boolean, width = 420, longNames = false): Scenario {
  return {
    wrap: (node) => (
      <div className="flex-1 min-h-0 flex justify-center px-6 py-8">
        <div
          className="w-full rounded-[var(--radius-card)] bg-[var(--color-surface)] flex flex-col min-h-0 px-4 py-4"
          style={{ maxWidth: width }}
        >
          {node}
        </div>
      </div>
    ),
    render: () => <TranscriptPlayerHarness disabled={disabled} longNames={longNames} />,
    ipc: {
      note_session_playback_path: () => null,
      note_timeline_set_chunk_text: () => null,
      cloud_upload_note_sessions: () => null,
    },
  };
}

function TranscriptPlayerHarness({
  disabled,
  longNames = false,
}: {
  disabled: boolean;
  longNames?: boolean;
}) {
  const turn = (
    chunkIdx: number,
    label: string,
    text: string,
    startMs: number,
  ): TimelineEntry => ({
    startMs: startMs,
    endMs: startMs + 3000,
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
    turn(0, longNames ? "Michael Mehlum Wilhelmsen" : "Michael", "Skal vi ta gjennomgangen nå?", 0),
    turn(1, longNames ? "Hege Marie Tronshaugen-Lillevik" : "Hege", "Ja, jeg har notatene klare", 3000),
    turn(2, longNames ? "Hege Marie Tronshaugen-Lillevik" : "Hege", "og tallene fra forrige kvartal", 6000),
    turn(3, longNames ? "Michael Mehlum Wilhelmsen" : "Michael", "Bra — da starter vi der.", 9000),
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
          canTranscribe: false,
          canRetranscribe: false,
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

// ---- #146 axis: the Transcript panel with nothing in it -------------------
// A "Transcribe manually" capture leaves this pane reporting the gap, so the
// control that closes it belongs here and not only in the toolbar. What needs
// eyes: a bordered button under two lines of muted text, centred in a column
// that is otherwise all disabled grey — and whether it still reads as an offer
// at `PANEL_FLOOR`, where the sentence takes a line more.
function panelEmptyCase(pending: boolean, width = 320): Scenario {
  return {
    wrap: (node) => (
      <div className="flex-1 min-h-0 flex justify-center px-6 py-8">
        <div
          className="w-full rounded-[var(--radius-card)] bg-[var(--color-surface)] flex flex-col min-h-0 px-4 py-4"
          style={{ maxWidth: width }}
        >
          {node}
        </div>
      </div>
    ),
    render: () => (
      <PanelEmpty
        icon={<MessageSquare size={22} strokeWidth={1.5} />}
        text={
          pending
            ? "This recording hasn't been transcribed yet."
            : "No transcript yet. Start a recording from the toolbar to capture and transcribe audio."
        }
        action={
          pending ? (
            <button type="button" className="nd-btn">
              <Files size={15} strokeWidth={1.6} />
              Transcribe
            </button>
          ) : undefined
        }
      />
    ),
    // Nothing here talks to the backend — the button is inert on purpose, so
    // the scenario is only ever a question about the layout.
    ipc: {},
  };
}

// ---- #146 axis: the Transcribe action ON the panel that reports it missing --
// The whole note, because the question is about a control's place among the
// others: the empty state's button under two lines of muted text, and — once a
// second take is pending on a note that already has text — the icon sitting
// between Copy and Re-transcribe, in a row that also holds two pickers.
function noteTranscribeCase(withText: boolean): Scenario {
  const note = makeNote({
    id: "n1",
    title: "Kvartalsgjennomgang",
    transcript: withText ? "Michael: Skal vi ta gjennomgangen nå?\nHege: Ja, jeg har notatene klare" : "",
    language: "no",
    expected_speakers: 2,
  });
  const take = (index: number, pending: boolean) => ({
    id: `s${index}`,
    index,
    startedAt: new Date(1_755_000_000_000).toISOString(),
    durationMs: 1_800_000,
    streams: ["mic"],
    hasPlayback: true,
    canTranscribe: pending,
    canRetranscribe: true,
  });
  return {
    route: "/note/n1",
    render: () => null, // unused — `route` renders the app
    ipc: {
      notes_list: () => [note],
      notes_get: () => note,
      note_timeline: () => [],
      note_sessions: () =>
        withText ? [take(1, false), take(2, true)] : [take(1, true)],
    },
  };
}

// ---- #146 axis: the note toolbar at the widths the body column really gets --
// The toolbar sits in the BODY column, whose width is the window minus the
// context panel (clamped 320–720) — so at the 720px minimum window it can be
// under 400px wide, and the row carries a back link plus up to three `.nd-btn`s
// and two icon buttons. `.nd-btn` is `flex-shrink: 0; white-space: nowrap`, so
// the row cannot absorb the squeeze; it degrades by container query instead.
// One scenario per width, because the whole question is where each step lands.
//
// `width` is the column, not the viewport: the `@container` is the toolbar's own
// inline size, which is what the real one queries too.
function toolbarCase(
  width: number,
  pending: boolean,
  sidebarCollapsed = false,
): Scenario {
  return {
    wrap: (node) => (
      <div className="flex-1 min-h-0 flex flex-col items-center gap-3 py-6">
        <p className="text-xs text-[var(--color-text-muted)]">
          body column: {width}px
          {sidebarCollapsed && " · sidebar collapsed (traffic-light gutter)"}
        </p>
        {/* The rounded card the body column is, so an overflowing row is
            visible as a row that leaves its card. */}
        <div
          className="rounded-[var(--radius-card)] bg-[var(--color-surface)] overflow-hidden"
          style={{ width }}
        >
          <MemoryRouter>{node}</MemoryRouter>
        </div>
      </div>
    ),
    render: () => (
      <NoteToolbar
        noteId="mock-note"
        backTo="/"
        backLabel="Kundemøter og oppfølging"
        readOnly={false}
        recActive={false}
        canRecord
        panelOpen
        onTogglePanel={() => {}}
        onSummarizeFailed={() => {}}
        onRegenerateTitle={() => {}}
        pendingTranscription={pending}
        isTranscribing={false}
        sidebarCollapsed={sidebarCollapsed}
      />
    ),
    ipc: {
      notes_list: () => [],
    },
  };
}

// Every settings-section scenario stands its section in the same scrollable
// column the dialog gives it. A shared wrap because a section judged in a
// wider or narrower box than the real one is judged against the wrong layout.
const settingsWrap: Scenario["wrap"] = (node) => (
  <div className="flex-1 min-h-0 overflow-y-auto">
    <div className="max-w-2xl mx-auto px-8 py-7">{node}</div>
  </div>
);

// ---- #172 axis: the MCP integration section in Settings --------------------
// What needs eyes here is the multi-line Codex snippet: CommandSnippet's block
// mode wraps instead of truncating, and a config stanza next to a Copy button
// is the one row in this section that isn't a plain control row.
//
// `settingsWrap` mirrors SettingsLayout's content column — the Section card
// fills its container, so a full-width wrapper would stretch the snippets far
// past what they ever get in the real dialog.
function integrationsCase(enabled: boolean): Scenario {
  return {
    wrap: settingsWrap,
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

// ---- #179: the local chat provider, on Ollama and on a server that is not
// Ollama. The second is the case the issue was filed about: no `ollama pull`
// commands, and an embedder pointed at its own address. jsdom asserts the words
// but not that two input rows and a status line sit right in the column.
function chatCase(kind: "ollama" | "compat" | "compat-embedding"): Scenario {
  let unembedded = 4;
  const chatUrl = kind === "ollama" ? "http://localhost:11434/v1" : "http://127.0.0.1:8000/v1";
  const model = kind === "ollama" ? "gemma4:12b-mlx" : "mlx-community/Qwen3-8B";
  return {
    wrap: settingsWrap,
    render: () => (
      <ChatHarness
        chatUrl={chatUrl}
        model={model}
        embedUrl={kind === "compat-embedding" ? "http://localhost:11434/v1" : ""}
      />
    ),
    ipc: {
      local_llm_list_models: () => [model, "embeddinggemma"],
      // A library part-way through: the embedder answers and four notes still
      // have no vector under its name — the two facts #179 kept conflating.
      // Stateful, so pressing Embed now leads somewhere.
      chat_unembedded_note_count: () => unembedded,
      chat_embed_missing: () => {
        const n = unembedded;
        unembedded = 0;
        return n;
      },
      chat_stale_note_count: () => 0,
      // Only the Ollama-side embedder answers; on the bare compat server the
      // probe fails, which is the state that has to read as a soft warning
      // rather than an error.
      local_llm_embed_probe: (args) => {
        const url = (args as { baseUrl: string }).baseUrl;
        if (url.includes(":11434")) return 768;
        throw new Error("HTTP 404 from http://127.0.0.1:8000/v1/embeddings: Not Found");
      },
      provider_key_get: () => null,
    },
  };
}

function ChatHarness({
  chatUrl,
  model,
  embedUrl,
}: {
  chatUrl: string;
  model: string;
  embedUrl: string;
}) {
  const [s, setS] = useState({
    ...DEFAULTS,
    chat_provider: "ollama",
    chat_model: model,
    local_llm_base_url: chatUrl,
    embed_base_url: embedUrl,
  } as Record<EditableKey, string>);
  return <ChatTab s={s} update={async (k, v) => setS((prev) => ({ ...prev, [k]: v }))} />;
}

// ---- #21 axis: menu-bar mode in the Recording section ----------------------
// Same wrapper as the Integrations case, and for the same reason: the section
// has to be judged inside SettingsLayout's content column, not stretched.
// Both toggle states, because the close-to-tray copy swaps entirely — it names
// which regime is in force rather than describing the switch.
function menubarCase(closeToTray: boolean, hotkey: string): Scenario {
  return {
    wrap: settingsWrap,
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

// ---- #146: audio retention and the "Transcribe manually" disclosure --------
// Both toggles are LIVE here, because the thing to judge is the reveal: the
// second row appears only once retention is on, and the section has to hold its
// shape as it does. jsdom asserts the presence; only a real webview shows
// whether the row lands where a row should.
function retentionCase(keepAudio: boolean, manual: boolean): Scenario {
  return {
    wrap: settingsWrap,
    render: () => <RetentionHarness keepAudio={keepAudio} manual={manual} />,
    ipc: {
      record_hotkey_get: () => "Command+Control+KeyR",
      record_hotkey_set: () => null,
      permissions_status: () => ({ microphone: "granted", screen: "granted" }),
      stored_audio_stats: () => ({ notes: 0, files: 0, bytes: 0, noteIds: [] }),
    },
  };
}

function RetentionHarness({
  keepAudio,
  manual,
}: {
  keepAudio: boolean;
  manual: boolean;
}) {
  const [keep, setKeep] = useState(keepAudio ? "true" : "false");
  const [man, setMan] = useState(manual ? "true" : "false");
  const s = {
    ...DEFAULTS,
    keep_audio: keep,
    transcribe_manually: man,
  } as Record<EditableKey, string>;
  return (
    <RecordingSection
      s={s}
      update={async (k, v) => {
        if (k === "keep_audio") setKeep(v);
        if (k === "transcribe_manually") setMan(v);
      }}
    />
  );
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

// ---- #174 axis: whether the no-audio warning can name its input device ------
//
// The copy grew by a device name, so the question worth looking at is whether
// it still fits — which jsdom, pinning every box to 0, cannot answer.
//
// The width to judge it at is `BODY_MIN` (420), NOT the window's 720px
// `minWidth`: RecordingBar positions itself against the note page's body
// column, and the sidebar and right panel both eat into that. 420 is the
// narrowest that column is allowed to get.
const BODY_MIN_PX = 420;

function noAudioCase(device: string | null, width = BODY_MIN_PX): Scenario {
  return {
    wrap: (node) => (
      <div
        className="relative mx-auto h-[200px] border border-dashed border-[var(--color-line-visible)]"
        style={{ width }}
      >
        {node}
      </div>
    ),
    render: () => {
      // The bar reads the store, not IPC. Seed the exact state the warning
      // needs: this note recording, mic never heard, and past the ~10s latch.
      useRecordingStore.setState({
        status: { noteId: "n1", phase: "recording" },
        micHeard: false,
        activeSince: null,
        activeAccumMs: 20_000,
        diag: {
          noteId: "n1",
          micFrames: 224_000,
          sysFrames: 224_000,
          chunks: 0,
          micPeak: 0,
          sysPeak: 0,
          inputDevice: device,
        },
      });
      return <RecordingBar noteId="n1" />;
    },
    ipc: {},
  };
}

// ---- #177 axis: how wide the controls row is against the column it centres in
//
// The row is diagnostics pill + (optionally) a busy pill + the timer/controls
// pill, every one of them `shrink-0 whitespace-nowrap` — so the row's width is
// a constant and the column's is not. jsdom pins every box to 0, so whether
// the row fits can only be asked here (and by `scripts/measure-recording-bar.js`,
// which sweeps these widths).
//
// The widths that matter: 420 is `BODY_MIN`, the narrowest the body column is
// nominally allowed to be, and 414 is what it actually gets in the SHIPPED
// DEFAULT — 1100px window, sidebar open, panel at its clamped 406. The bar
// overhung both before the degradation ladder went in.
function recBarCase(
  width: number,
  opts: { summarizing?: boolean; paused?: boolean; long?: boolean } = {},
): Scenario {
  const { summarizing = false, paused = false, long = false } = opts;
  return {
    wrap: (node) => (
      <div className="flex-1 flex flex-col items-center gap-3 py-6">
        <p className="text-xs text-[var(--color-text-muted)]">
          body column: {width}px
          {summarizing && " · summarizing"}
          {paused && " · paused"}
          {long && " · long recording"}
        </p>
        {/* The body column, dashed so an overhanging row is visible as one that
            leaves it. Deliberately NOT `overflow-hidden`: the real column has
            no clip either, which is why the overhang paints over the nav card
            on one side and under the context panel on the other. */}
        <div
          className="relative h-[220px] border border-dashed border-[var(--color-line-visible)]"
          style={{ width }}
        >
          {node}
        </div>
      </div>
    ),
    render: () => {
      // The bar reads the store, not IPC. `long` seeds the widest content each
      // pill can honestly reach: a 90-minute capture (4-digit seconds, 3-digit
      // chunk count) and a timer past the hour.
      useRecordingStore.setState({
        status: { noteId: "n1", phase: paused ? "paused" : "recording" },
        summarizing: summarizing ? { n1: true } : {},
        micHeard: true,
        activeSince: null,
        activeAccumMs: 0,
        micLevel: 0.4,
        sysLevel: 0.2,
        diag: {
          noteId: "n1",
          micFrames: long ? 16000 * 5400 : 16000 * 14,
          sysFrames: long ? 16000 * 5400 : 16000 * 14,
          chunks: long ? 428 : 3,
          micPeak: 0.4,
          sysPeak: 0.2,
          inputDevice: "MacBook Pro-mikrofon",
        },
      });
      return <RecordingBar noteId="n1" />;
    },
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

  // --- #146: the toolbar at the widths the body column really gets. The
  // `-wide` pair is the comfortable case; each step down is a documented
  // degradation, and `toolbar-380` is the 720px-window / 320px-panel floor.
  "toolbar-wide": toolbarCase(760, true),
  "toolbar-620": toolbarCase(620, true),
  "toolbar-520": toolbarCase(520, true),
  "toolbar-430": toolbarCase(430, true),
  "toolbar-380": toolbarCase(380, true),
  // Without a pending take there is no Transcribe button — the case that
  // shipped before #146, for comparison.
  "toolbar-380-notranscribe": toolbarCase(380, false),
  // Collapsed sidebar: the same row minus 104px, spent on clearing the traffic
  // lights. Its thresholds are shifted up by exactly that.
  "toolbar-collapsed-760": toolbarCase(760, true, true),
  "toolbar-collapsed-620": toolbarCase(620, true, true),
  "toolbar-collapsed-430": toolbarCase(430, true, true),

  // --- #146: the deferred-transcription disclosure. `retention-off` must show
  // one row; the other two are the revealed toggle in both positions.
  "retention-off": retentionCase(false, false),
  "retention-on": retentionCase(true, false),
  "retention-manual": retentionCase(true, true),

  // --- #179: local chat on Ollama, on a plain OpenAI-compat server, and on one
  // whose embedder is pointed back at Ollama (the shape the issue needs).
  "chat-ollama": chatCase("ollama"),
  "chat-compat": chatCase("compat"),
  "chat-compat-embedding": chatCase("compat-embedding"),

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
  // --- #176: the turn title at the two ends of the panel's clamp, and with
  // names long enough to make the title row's truncation do its job. The
  // question at 260 is whether the title row and the open edit box still fit;
  // at 720 it is whether the title still reads as a title.
  "player-260": playerCase(false, 260),
  "player-720": playerCase(false, 720),
  "player-260-longnames": playerCase(false, 260, true),

  // --- #146: the Transcript panel's empty states, with and without the run
  // that would fill it. The narrow one is the real test — the button has to
  // still read as an offer once the sentence wraps.
  "panel-pending": panelEmptyCase(true),
  "panel-pending-260": panelEmptyCase(true, 260),
  "panel-nothing": panelEmptyCase(false),
  // The same two states in the real note, where the action has to hold its own
  // beside the toolbar's copy of it and the panel's pickers.
  "note-pending": noteTranscribeCase(false),
  "note-pending-text": noteTranscribeCase(true),

  // --- The create-team-workspace sheet, one scenario per derived stage.
  "ws-connect": workspaceCase("connect"),
  "ws-auth": workspaceCase("auth"),
  "ws-name": workspaceCase("name"),
  "ws-trial": workspaceCase("trial"),
  "ws-invite": workspaceCase("invite"),

  // --- #177: the controls row against the column it centres in. `recbar-420`
  // is `BODY_MIN`; `recbar-default` is the width the shipped default window
  // actually gives the column; the `-summary` pair adds the third pill that
  // a summary running during a recording puts in the same row; `-long` is the
  // widest honest content (90-minute capture, paused, hour-plus timer).
  "recbar-wide": recBarCase(900),
  "recbar-default": recBarCase(414),
  "recbar-420": recBarCase(420),
  "recbar-380": recBarCase(380),
  "recbar-summary": recBarCase(414, { summarizing: true }),
  "recbar-summary-380": recBarCase(380, { summarizing: true }),
  "recbar-long": recBarCase(420, { paused: true, long: true }),
  "recbar-long-summary": recBarCase(420, { paused: true, long: true, summarizing: true }),

  // --- #174: the no-audio warning naming the device it isn't hearing.
  // A real (localized) device name, the fallback when the HAL won't name one,
  // and the clamp doing its job on a pathological user-authored name.
  noaudio: noAudioCase("MacBook Pro-mikrofon"),
  "noaudio-unknown": noAudioCase(null),
  "noaudio-long": noAudioCase("Michael's Extremely Long Audio Interface Name Mk II"),
  // The same pill with the body column at a comfortable width, so the
  // narrow-column layout can be compared against the roomy one.
  "noaudio-wide": noAudioCase("MacBook Pro-mikrofon", 900),

  // --- The note grid, whole-app at /all-notes against a populated library.
  // Card density, excerpt length and the borderless card's separation from its
  // ground are all things jsdom pins to zero.
  notes: {
    route: "/all-notes",
    render: () => null, // unused — `route` renders the app
    ipc: {
      notes_list: () => demoNotes(),
      folders_list: () => DEMO_FOLDERS,
      clients_list: () => DEMO_CLIENTS,
    },
  },

  // A library with nothing in it — the first screen a fresh install shows.
  "notes-empty": {
    route: "/all-notes",
    render: () => null, // unused — `route` renders the app
    ipc: { notes_list: () => [], folders_list: () => [], clients_list: () => [] },
  },

  // The same grid inside a folder, where the folder chip is deliberately absent
  // from every card.
  "notes-folder": {
    route: "/folder/f1",
    render: () => null, // unused — `route` renders the app
    ipc: {
      notes_list: () => demoNotes(),
      folders_list: () => DEMO_FOLDERS,
      clients_list: () => DEMO_CLIENTS,
    },
  },

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

if (scenario.route) {
  // A whole-app scenario needs every command the boot path fires, which is the
  // unit tests' problem too — so it borrows their answers rather than growing a
  // second set here.
  //
  // `?palette=` / `?theme=` have to be answered as SETTINGS, not stamped on
  // <html>: the real app hydrates both from `settings_get` on boot, so the
  // attributes the harness wrote above are overwritten a tick later and the
  // whole theme axis silently stops working for these scenarios. Answering the
  // rows instead puts them through the app's own path, which is what a route
  // scenario is for. `onboarding_completed` has to be carried through with
  // them — a `settings_get` handler here replaces `mockTauri`'s entirely, and
  // without it every note scenario opens on the wizard.
  mockTauri({
    ...scenario.ipc,
    settings_get: (args) => {
      const key = (args as { key?: string } | undefined)?.key;
      if (key === "palette") return params.get("palette");
      if (key === "theme") return mode;
      if (key === "onboarding_completed") return "true";
      return scenario.ipc.settings_get?.(args) ?? null;
    },
  });
} else {
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
}

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
      {scenario.route ? (
        <MemoryRouter initialEntries={[scenario.route]}>
          <App />
        </MemoryRouter>
      ) : (
        (scenario.wrap ?? onboardingCanvas)(scenario.render(ctx))
      )}
    </div>
  </StrictMode>,
);
