// Onboarding wizard — step registry + shared context contract.
//
// This module is the seam later work packages build against. To add or
// replace a step you touch exactly one file under `steps/` plus the one
// entry in `STEPS` below — the shell (`Onboarding.tsx`) never needs editing.
//
// Design decisions this encodes (from design/ONBOARDING.md):
//   - Steps are identified by stable string IDs, never array indices. The
//     resume cursor persisted to the `onboarding_step` setting stores an ID,
//     so reordering steps in a future release can't silently teleport a
//     resuming user to the wrong screen.
//   - Every step writes through to live settings immediately (no final
//     commit). The shell owns navigation + the progress bar; steps own their
//     own content and their own settings writes.

import type { ComponentType } from "react";

// Ordered pipeline. The soft progress bar and back/next navigation are
// derived from this order. Keep `welcome` first and `done` last.
export const STEP_ORDER = [
  "welcome",
  "permissions",
  "language",
  "transcription",
  "summary",
  "cloud",
  "done",
] as const;

export type StepId = (typeof STEP_ORDER)[number];

// Passed to every step component by the shell. Small on purpose — steps
// drive navigation through these callbacks and never manipulate the URL,
// the progress bar, or the resume cursor directly.
export type StepContext = {
  // The step currently being shown (handy for shared step chrome).
  stepId: StepId;
  // Zero-based index of this step within STEP_ORDER.
  index: number;
  // Total number of steps (for any step that wants to display context;
  // the shell's progress bar is a soft fraction, not "N of M").
  total: number;
  // Advance to the next step (no-op on the last step). Persists the new
  // cursor to `onboarding_step` before rendering it.
  goNext: () => void;
  // Go back to the previous step (no-op on the first step). Also persists.
  goBack: () => void;
  // Jump to an arbitrary step by ID (used by the final status screen's
  // "fix this" rows in a later package). Persists.
  goTo: (id: StepId) => void;
  // Whether a Back affordance makes sense (false on the first step).
  canGoBack: boolean;
  // Finish the wizard: sets `onboarding_completed=true` and hands control
  // back to the app. Called by the final step's primary CTA. `skipped`
  // distinguishes "reached the end" from "skip setup" for future analytics
  // / nag semantics; both set the same completion flag today.
  //
  // `destination` is the route the app should land on after completion (e.g.
  // "/note/<id>" from the "Create your first note" CTA). It's threaded all
  // the way to the App-level onDone so BOTH entry modes (takeover + the
  // /onboarding route) navigate there — the route mode's default "go home"
  // can't clobber it because the destination overrides that default. Omitted
  // → the mode's own default (takeover stays put; route goes home).
  complete: (opts?: { skipped?: boolean; destination?: string }) => void;
};

// A step is a plain component receiving the shared context. Keeping the
// signature this thin means later packages can drop in a new step file
// with zero shell changes.
export type StepComponent = ComponentType<{ ctx: StepContext }>;

export type StepDef = {
  id: StepId;
  Component: StepComponent;
};

// Honour a persisted resume cursor, falling back to the first step. Used on
// the FIRST-RUN path (onboarding not yet completed): the user is walking the
// wizard in order, so we resume exactly where they left off.
export function firstIncompleteStep(persistedStep: string | null): StepId {
  if (persistedStep && (STEP_ORDER as readonly string[]).includes(persistedStep)) {
    return persistedStep as StepId;
  }
  return "welcome";
}

// Resolve where the wizard should land on OPEN, given whether onboarding has
// already been completed and the persisted cursor (design/ONBOARDING.md
// § "Nag chip" — "reopens the wizard at the first incomplete step").
//
//   - Not yet completed (first run): honour the persisted cursor — the user is
//     mid-walk and should resume in place.
//   - Completed, re-opened manually (nag chip / "Run setup again"): compute
//     from the SAME shared predicate the nag chip + recap use, so a re-run
//     lands on the first genuinely-incomplete step rather than wherever the
//     user happened to stop last time:
//       !micGranted     → "permissions"
//       !stt.working    → "transcription"
//       otherwise       → "done"  (the recap; everything essential is set up)
export async function resolveResumeStep(
  onboardingCompleted: boolean,
  persistedStep: string | null,
): Promise<StepId> {
  if (!onboardingCompleted) return firstIncompleteStep(persistedStep);
  // Lazy import to keep the type module free of runtime deps that would drag
  // the IPC/cloud layer into anything that only wants the STEP_ORDER types.
  const { computeSetupStatus } = await import("../../lib/setupStatus");
  try {
    const s = await computeSetupStatus();
    if (!s.micGranted) return "permissions";
    if (!s.stt.working) return "transcription";
    return "done";
  } catch {
    // If the predicate can't be computed, fall back to the recap rather than
    // dumping a returning user at the top of the wizard.
    return "done";
  }
}
