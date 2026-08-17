// Onboarding wizard shell — full-window takeover.
//
// Responsibilities (design/ONBOARDING.md § Presentation):
//   - Render the current step full-window: no sidebar, no top bar.
//   - Soft thin progress bar across the top — an animated fraction, NOT dots,
//     NOT "step N of M" text.
//   - Back navigation + a quiet "Skip setup" affordance in the corner.
//   - Resume: the current step ID is persisted to `onboarding_step` on every
//     transition (write-through, no final commit) and restored on mount.
//   - Skip / finish both set `onboarding_completed=true` and exit to the app;
//     the wizard never auto-shows again (the nag chip, a later package, takes
//     over from live state).
//
// The shell knows nothing about individual steps beyond the registry below —
// later packages replace one file each under `steps/` with zero shell edits.
import { useCallback, useEffect, useMemo, useState } from "react";
import { X } from "lucide-react";
import { ipc } from "../../lib/ipc";
import { TelemetryNotice } from "./TelemetryNotice";
import {
  STEP_ORDER,
  resolveResumeStep,
  type StepDef,
  type StepId,
} from "./types";
import { WelcomeStep } from "./steps/Welcome";
import { PermissionsStep } from "./steps/Permissions";
import { LanguageStep } from "./steps/Language";
import { TranscriptionStep } from "./steps/Transcription";
import { SummaryStep } from "./steps/Summary";
import { CloudStep } from "./steps/Cloud";
import { DoneStep } from "./steps/Done";

// The step registry. Order comes from STEP_ORDER; this maps each ID to its
// component. Every step is now real — the wizard walks end to end.
const STEPS: StepDef[] = [
  { id: "welcome", Component: WelcomeStep },
  { id: "permissions", Component: PermissionsStep },
  { id: "language", Component: LanguageStep },
  { id: "transcription", Component: TranscriptionStep },
  { id: "summary", Component: SummaryStep },
  { id: "cloud", Component: CloudStep },
  { id: "done", Component: DoneStep },
];

// Registry order must line up with STEP_ORDER — a mismatch would break the
// progress fraction and index math. Assert once at module load (dev only).
if (import.meta.env.DEV) {
  const registryIds = STEPS.map((s) => s.id).join(",");
  const canonical = STEP_ORDER.join(",");
  if (registryIds !== canonical) {
    console.error(
      `[onboarding] STEPS order ${registryIds} does not match STEP_ORDER ${canonical}`,
    );
  }
}

// Props let the same component serve both the App.tsx takeover (where a
// completion must swap to the normal app in place — `onDone`) and the manual
// `/onboarding` route (where completion just navigates away).
export function Onboarding({
  onDone,
  startAt,
}: {
  // Called after `onboarding_completed` is persisted. The takeover guard uses
  // this to re-render the normal app without a reload; the route variant
  // navigates home. `destination` (e.g. "/note/<id>" from the final step's
  // "Create your first note" CTA) is where the app should land — both modes
  // honour it so the final CTA can't be clobbered by the route mode's
  // default "go home".
  onDone?: (destination?: string) => void;
  // Force a starting step (used by the manual re-run entry). When omitted, the
  // shell resumes from the persisted `onboarding_step` cursor.
  startAt?: StepId;
}) {
  // `null` = still resolving the resume cursor; render nothing to avoid a
  // flash of the wrong step.
  const [current, setCurrent] = useState<StepId | null>(startAt ?? null);

  useEffect(() => {
    if (startAt) return; // explicit start wins — no resume read needed
    let cancelled = false;
    // Resolve where to land: on a completed install re-opened manually (nag
    // chip / "Run setup again") this computes from the shared predicate; on a
    // first run it honours the persisted cursor. Stays behind the shell's
    // null-render-while-resolving pattern so there's no flash of the wrong step.
    Promise.all([
      ipc.getSetting("onboarding_completed").catch(() => null),
      ipc.getSetting("onboarding_step").catch(() => null),
    ])
      .then(([completed, persisted]) =>
        resolveResumeStep(completed === "true", persisted),
      )
      .then((step) => {
        if (!cancelled) setCurrent(step);
      })
      .catch(() => {
        if (!cancelled) setCurrent("welcome");
      });
    return () => {
      cancelled = true;
    };
  }, [startAt]);

  // Report the furthest step reached, which is what turns "people abandon setup"
  // into "people abandon setup HERE". Each step counts at most once per install
  // (the backend holds the marker), so re-entering a step or walking back is free.
  useEffect(() => {
    if (!current) return;
    void ipc.telemetryEvent(`onboarding_reached_${current}`);
  }, [current]);

  const index = current ? STEP_ORDER.indexOf(current) : 0;
  const total = STEP_ORDER.length;
  // Soft progress: fraction of the way through, growing as steps complete.
  // Uses (index + 1) so even the first screen shows a sliver of progress.
  const fraction = current ? (index + 1) / total : 0;

  // Persist the cursor, then move. Write-through means quitting mid-wizard
  // resumes exactly here.
  const goTo = useCallback((id: StepId) => {
    setCurrent(id);
    ipc.setSetting("onboarding_step", id).catch((e) => {
      console.warn("[onboarding] failed to persist step:", e);
    });
  }, []);

  const goNext = useCallback(() => {
    const i = current ? STEP_ORDER.indexOf(current) : 0;
    if (i < STEP_ORDER.length - 1) goTo(STEP_ORDER[i + 1]);
  }, [current, goTo]);

  const goBack = useCallback(() => {
    const i = current ? STEP_ORDER.indexOf(current) : 0;
    if (i > 0) goTo(STEP_ORDER[i - 1]);
  }, [current, goTo]);

  const complete = useCallback(
    (opts?: { skipped?: boolean; destination?: string }) => {
      // Same completion flag whether the user finished or skipped — the nag
      // chip decides from live pipeline state whether to prompt again, so we
      // never need to distinguish here. `destination` (if any) is handed to
      // onDone so the app lands on the right route in both entry modes.
      void ipc.telemetryEvent(opts?.skipped ? "onboarding_skipped" : "onboarding_finished");
      ipc
        .setSetting("onboarding_completed", "true")
        .catch((e) => console.warn("[onboarding] failed to complete:", e))
        .finally(() => onDone?.(opts?.destination));
    },
    [onDone],
  );

  const ctx = useMemo(
    () =>
      current
        ? {
            stepId: current,
            index,
            total,
            goNext,
            goBack,
            goTo,
            canGoBack: index > 0,
            complete,
          }
        : null,
    [current, index, total, goNext, goBack, goTo, complete],
  );

  // Resolving the resume cursor — render nothing (avoids a flash).
  if (!current || !ctx) {
    return <div className="h-full w-full bg-[var(--color-canvas)]" />;
  }

  const def = STEPS.find((s) => s.id === current) ?? STEPS[0];
  const StepComponent = def.Component;

  return (
    <div className="relative h-full w-full flex flex-col bg-[var(--color-canvas)]">
      {/* Drag strip so the frameless window is still movable and the macOS
          traffic lights stay grabbable (the takeover has no nav card). */}
      <div data-tauri-drag-region className="absolute top-0 left-0 right-0 h-9 z-10" />

      {/* Soft progress bar. A thin animated fill — not dots, not a counter. */}
      <div
        className="absolute top-0 left-0 right-0 h-[3px] bg-transparent z-20"
        role="progressbar"
        aria-valuenow={Math.round(fraction * 100)}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-label="Setup progress"
      >
        <div
          className="h-full bg-[var(--color-accent)] transition-[width] duration-500 ease-out"
          style={{ width: `${fraction * 100}%` }}
        />
      </div>

      {/* Skip setup — quiet, top-right corner. Sets completion + exits; the
          nag chip takes over from live state (design/ONBOARDING.md). */}
      <button
        type="button"
        onClick={() => complete({ skipped: true })}
        className="no-drag absolute top-4 right-5 z-30 inline-flex items-center gap-1.5 text-xs text-[var(--color-text-muted)] hover:text-[var(--color-text)] transition-colors"
      >
        Skip setup
        <X size={13} strokeWidth={2} />
      </button>

      {/* Centred step content. */}
      <div className="flex-1 min-h-0 overflow-y-auto flex items-center justify-center px-6 py-16">
        <StepComponent ctx={ctx} />
      </div>

      {/* Telemetry disclosure + its control. Canvas-level so it shows on every
          step and on every entry point (a resumed wizard skips `welcome`). */}
      <div className="absolute bottom-4 left-0 right-0 z-20 flex justify-center px-24">
        <TelemetryNotice />
      </div>

      {/* Back navigation — bottom-left, only when there's somewhere to go. */}
      <div className="absolute bottom-5 left-6 z-30">
        {ctx.canGoBack && (
          <button
            type="button"
            onClick={goBack}
            className="no-drag text-sm text-[var(--color-text-muted)] hover:text-[var(--color-text)] transition-colors"
          >
            Back
          </button>
        )}
      </div>
    </div>
  );
}
