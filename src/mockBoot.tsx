// Visual-check harness (dev only, never bundled into the app — `mock.html` is
// not an entry in the Tauri build). Renders real components against a mocked
// Tauri IPC so layout, spacing and token usage can be eyeballed in a browser.
// Unit tests assert behaviour; this catches the things jsdom cannot see —
// unlayered utilities losing to Tailwind, hints stacking into a wall of text,
// a control that renders but is invisible.
//
// Pick a scenario with ?case=<name>. Add scenarios to CASES below.
import ReactDOM from "react-dom/client";
import { StrictMode } from "react";
import { mockIPC } from "@tauri-apps/api/mocks";
import { SummaryStep } from "./pages/onboarding/steps/Summary";
import type { StepContext } from "./pages/onboarding/types";
import "@fontsource/hanken-grotesk/400.css";
import "@fontsource/hanken-grotesk/500.css";
import "@fontsource/hanken-grotesk/600.css";
import "@fontsource/hanken-grotesk/700.css";
import "./styles/globals.css";

// Installed-model lists per scenario — the axis #147 turns on.
const CASES: Record<string, string[] | "unreachable"> = {
  // The issue's exact report: recommended model already pulled.
  recommended: ["gemma4:12b-mlx", "embeddinggemma"],
  // 16 GB tier fallback.
  "16gb": ["qwen3.5:4b"],
  // Neither recommendation — the merged branch: ✓ + upgrade hint + pull + picker.
  fallback: ["llama3.2:3b", "mistral:7b"],
  // Models present but none can chat.
  "embedding-only": ["embeddinggemma"],
  // No models at all.
  empty: [],
  unreachable: "unreachable",
};

const which = new URLSearchParams(location.search).get("case") ?? "recommended";
const installed = CASES[which] ?? CASES.recommended;

mockIPC(async (cmd, args) => {
  if (cmd === "local_llm_list_models") {
    if (installed === "unreachable") throw new Error("connection refused");
    return installed;
  }
  if (cmd === "provider_key_get") return null; // no OpenAI key → neutral framing
  if (cmd === "settings_set") {
    console.log("[mock] settings_set", args);
    return null;
  }
  if (cmd === "settings_get") return null;
  if (cmd.startsWith("plugin:")) return undefined;
  return null;
});

const ctx = {
  stepId: "summary",
  index: 4,
  total: 6,
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
        <SummaryStep ctx={ctx} />
      </div>
    </div>
  </StrictMode>,
);
