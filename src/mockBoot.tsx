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
import { StrictMode } from "react";
import { mockIPC } from "@tauri-apps/api/mocks";
import { SummaryStep } from "./pages/onboarding/steps/Summary";
import { TranscriptionStep } from "./pages/onboarding/steps/Transcription";
import { STEP_ORDER, type StepContext, type StepId } from "./pages/onboarding/types";
import type { ProviderConfig } from "./lib/ipc";
import "@fontsource/hanken-grotesk/400.css";
import "@fontsource/hanken-grotesk/500.css";
import "@fontsource/hanken-grotesk/600.css";
import "@fontsource/hanken-grotesk/700.css";
import "./styles/globals.css";

type Handler = (args: unknown) => unknown;
// A scenario carries everything that varies between steps — which step it is
// and how to render it — so adding a third step means writing one more
// `*Case` builder, not extending a union and every switch on it. The wizard
// position is derived from STEP_ORDER rather than written down, so a step
// moving in the real wizard can't leave the harness claiming the old slot.
type Scenario = {
  step: StepId;
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

const CASES: Record<string, Scenario> = {
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
};

const which = new URLSearchParams(location.search).get("case") ?? "recommended";
const scenario = CASES[which] ?? CASES.recommended;

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
  index: STEP_ORDER.indexOf(scenario.step),
  total: STEP_ORDER.length,
  goNext: () => console.log("[mock] goNext"),
  goBack: () => {},
  goTo: () => {},
  canGoBack: true,
  complete: () => {},
} as unknown as StepContext;

// The wrapper MUST mirror the real wizard shell (Onboarding.tsx: the
// `flex-1 … flex items-center justify-center px-6 py-16` canvas). StepShell is
// `w-full max-w-lg` and centres its own contents but never itself, so a plain
// block wrapper pins the whole step to the left edge — a harness artefact that
// looks exactly like a layout bug in the component under review.
ReactDOM.createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <div className="relative h-screen w-full flex flex-col bg-[var(--color-canvas)]">
      <p className="pt-4 text-center text-xs text-[var(--color-text-muted)]">
        mock case: <code>{which}</code> — {Object.keys(CASES).join(" · ")}
      </p>
      <div className="flex-1 min-h-0 overflow-y-auto flex items-center justify-center px-6 py-16">
        {scenario.render(ctx)}
      </div>
    </div>
  </StrictMode>,
);
