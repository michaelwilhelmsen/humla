// Step 2 — Permissions (design/ONBOARDING.md § 2. Permissions).
//
// Deliberately the FIRST real step: if Screen Recording forces a relaunch,
// it's far better to hit that on step 2 than on step 6. One screen, two rows:
//
//   - Microphone (required)   — native prompt via permissions_request. The
//     Continue button is gated on this; the wizard can't record without it.
//   - System audio (skippable) — macOS has no native prompt for Screen
//     Recording; the flow is request (registers the app in the TCC list) then
//     deep-link to System Settings, the user toggles Humla on and returns; a
//     window-focus re-poll flips the pill. Skipping is a fully supported mode
//     (mic-only, in-person meetings) and must never block anything.
//
// The hard part — the restart. Screen Recording granted *during this app run*
// doesn't take effect until relaunch. Heuristic: snapshot the screen status at
// mount; if it was NOT granted at mount and becomes granted while the app is
// running, a restart is required. Already-granted-at-mount → no restart. The
// wizard owns this: it persists the resume cursor (belt-and-suspenders on top
// of the shell's own write-through) then offers a Restart Humla button that
// relaunches via the Tauri process plugin. The resume machinery lands the user
// back on this step showing the granted state.
import { useEffect, useRef, useState } from "react";
import { ShieldCheck, Mic, MonitorSpeaker, RotateCw, ArrowRight } from "lucide-react";
import { relaunch } from "@tauri-apps/plugin-process";
import { ipc, type PermissionStatus, type PermissionsStatus } from "../../../lib/ipc";
import type { StepContext } from "../types";
import { StepShell } from "../StepShell";

type Tone = "ok" | "bad" | "muted";

function pill(status: PermissionStatus): { text: string; tone: Tone } {
  switch (status) {
    case "granted":
      return { text: "Granted", tone: "ok" };
    case "denied":
      return { text: "Denied", tone: "bad" };
    case "restricted":
      return { text: "Restricted", tone: "bad" };
    case "not_determined":
      return { text: "Not requested", tone: "muted" };
    default:
      return { text: "Unknown", tone: "muted" };
  }
}

function pillColors(tone: Tone): { color: string; borderColor: string } {
  switch (tone) {
    case "ok":
      return { color: "var(--color-success)", borderColor: "var(--color-success)" };
    case "bad":
      return { color: "var(--color-danger)", borderColor: "var(--color-danger)" };
    default:
      return {
        color: "var(--color-text-muted)",
        borderColor: "var(--color-line-visible)",
      };
  }
}

function StatusPill({ status }: { status: PermissionStatus }) {
  const p = pill(status);
  return (
    <span className="nd-chip" style={pillColors(p.tone)}>
      {p.text}
    </span>
  );
}

export function PermissionsStep({ ctx }: { ctx: StepContext }) {
  const [status, setStatus] = useState<PermissionsStatus | null>(null);
  const [busy, setBusy] = useState<"microphone" | "screen" | null>(null);

  // The restart heuristic's memory. `screenGrantedAtMount` is captured from the
  // FIRST status read; if screen was already granted when we arrived, no restart
  // is ever needed (it took effect on a previous run / relaunch). If it wasn't,
  // and it later flips to granted, `needsRestart` latches true.
  const screenGrantedAtMount = useRef<boolean | null>(null);
  const [needsRestart, setNeedsRestart] = useState(false);

  async function refresh() {
    try {
      const s = await ipc.permissionsStatus();
      // Record the mount-time screen state exactly once, on the first read.
      if (screenGrantedAtMount.current === null) {
        screenGrantedAtMount.current = s.screen === "granted";
      }
      // Latch the restart requirement: not-granted at mount, granted now.
      if (screenGrantedAtMount.current === false && s.screen === "granted") {
        setNeedsRestart(true);
      }
      setStatus(s);
    } catch {
      // Sidecar may be absent in some build configs — leave prior state intact.
    }
  }

  useEffect(() => {
    // Persist the resume cursor immediately so a relaunch (either our button or
    // System Settings' own "Quit & Reopen") lands back on this step showing the
    // granted state. The shell already writes the cursor when navigating here,
    // but the spec requires the step itself to guarantee it *before* any restart
    // affordance can be reached — so we write through defensively on mount.
    ipc.setSetting("onboarding_step", "permissions").catch((e) => {
      console.warn("[onboarding] failed to persist step:", e);
    });

    // No interval polling — every status call re-spawns the audio-capture
    // sidecar (shared binary, `status` subcommand). macOS only changes TCC
    // state via a trip through System Settings, and returning from there fires
    // the focus event. Mirrors src/components/Permissions.tsx.
    refresh();
    const onFocus = () => refresh();
    window.addEventListener("focus", onFocus);
    return () => {
      window.removeEventListener("focus", onFocus);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  async function requestMic() {
    setBusy("microphone");
    try {
      const s = await ipc.permissionsRequest("microphone");
      setStatus(s);
    } catch {
      await refresh();
    } finally {
      setBusy(null);
    }
  }

  async function enableScreen() {
    setBusy("screen");
    try {
      // Requesting registers the app in the Screen Recording list (it can't be
      // granted programmatically), then we open System Settings so the user can
      // flip the switch. Returning fires the focus re-poll above.
      const s = await ipc.permissionsRequest("screen");
      setStatus(s);
      if (s.screen !== "granted") {
        await ipc.permissionsOpenSettings("screen");
      }
    } catch {
      await refresh();
    } finally {
      setBusy(null);
    }
  }

  async function openMicSettings() {
    try {
      await ipc.permissionsOpenSettings("microphone");
    } catch (e) {
      console.warn("[onboarding] failed to open settings:", e);
    }
  }

  async function doRestart() {
    try {
      await relaunch();
    } catch (e) {
      console.warn("[onboarding] relaunch failed:", e);
    }
  }

  // First status read still in flight — keep the shell chrome stable.
  if (!status) {
    return (
      <StepShell
        icon={<ShieldCheck size={26} strokeWidth={1.6} />}
        title="Grant recording permissions"
        subtitle="Humla needs microphone access to record. System audio is optional."
      />
    );
  }

  const micGranted = status.microphone === "granted";
  const micBlocked =
    status.microphone === "denied" || status.microphone === "restricted";
  const screenGranted = status.screen === "granted";

  return (
    <StepShell
      icon={<ShieldCheck size={26} strokeWidth={1.6} />}
      title="Grant recording permissions"
      subtitle="Humla needs microphone access to record. System audio is optional — grant it only if you record remote calls."
    >
      <div className="w-full max-w-md flex flex-col gap-3 text-left">
        {/* Microphone — required */}
        <div className="rounded-[var(--radius)] border border-[var(--color-line)] bg-[var(--color-surface)] px-4 py-3.5">
          <div className="flex items-start justify-between gap-4">
            <div className="min-w-0">
              <div className="flex items-center gap-2">
                <Mic
                  size={16}
                  strokeWidth={1.8}
                  className="shrink-0 text-[var(--color-text-muted)]"
                />
                <span className="text-sm font-semibold text-[var(--color-text)]">
                  Microphone
                </span>
                <StatusPill status={status.microphone} />
              </div>
              <p className="mt-1.5 text-xs leading-relaxed text-[var(--color-text-muted)]">
                Required — captures your voice. Humla can't record without it.
              </p>
              {micBlocked && (
                <p className="mt-1.5 text-xs leading-relaxed text-[var(--color-danger)]">
                  Microphone access was declined. Enable it in System Settings,
                  then return here.
                </p>
              )}
            </div>
            <div className="shrink-0">
              {micGranted ? null : micBlocked ? (
                <button
                  type="button"
                  onClick={openMicSettings}
                  className="nd-btn"
                >
                  Open System Settings
                </button>
              ) : (
                <button
                  type="button"
                  onClick={requestMic}
                  disabled={busy !== null}
                  className="nd-btn nd-btn-primary"
                >
                  {busy === "microphone" ? "Requesting…" : "Allow microphone"}
                </button>
              )}
            </div>
          </div>
        </div>

        {/* System audio — skippable */}
        <div className="rounded-[var(--radius)] border border-[var(--color-line)] bg-[var(--color-surface)] px-4 py-3.5">
          <div className="flex items-start justify-between gap-4">
            <div className="min-w-0">
              <div className="flex items-center gap-2">
                <MonitorSpeaker
                  size={16}
                  strokeWidth={1.8}
                  className="shrink-0 text-[var(--color-text-muted)]"
                />
                <span className="text-sm font-semibold text-[var(--color-text)]">
                  System audio
                </span>
                <StatusPill status={status.screen} />
              </div>
              <p className="mt-1.5 text-xs leading-relaxed text-[var(--color-text-muted)]">
                macOS bundles system-audio capture under "Screen Recording."
                Humla uses it to hear the other side of your calls — it never
                captures your screen.
              </p>
            </div>
            <div className="shrink-0">
              {!screenGranted && (
                <button
                  type="button"
                  onClick={enableScreen}
                  disabled={busy !== null}
                  className="nd-btn"
                >
                  {busy === "screen" ? "Opening…" : "Enable system audio"}
                </button>
              )}
            </div>
          </div>

          {/* Restart affordance — only when a grant during this run needs a
              relaunch to take effect. */}
          {needsRestart && (
            <div
              className="mt-3 rounded-[var(--radius)] px-3 py-3 flex items-start justify-between gap-4"
              style={{ background: "var(--color-accent-soft)" }}
            >
              <div className="min-w-0">
                <p className="text-xs font-semibold text-[var(--color-warning-text)]">
                  Restart required
                </p>
                <p className="mt-1 text-xs leading-relaxed text-[var(--color-text-muted)]">
                  System audio was just enabled. Humla needs to restart for it to
                  take effect — you'll return right here.
                </p>
              </div>
              <button
                type="button"
                onClick={doRestart}
                className="nd-btn nd-btn-primary shrink-0"
              >
                <RotateCw size={14} strokeWidth={2} />
                Restart Humla
              </button>
            </div>
          )}
        </div>
      </div>

      {/* Continue — gated on microphone, and on a pending restart: letting the
          user walk past the restart would ship them into their first meeting
          with system audio silently broken for this run. */}
      <div className="mt-8 w-full max-w-md flex flex-col items-center gap-3">
        <button
          type="button"
          className="nd-btn nd-btn-primary"
          disabled={!micGranted || needsRestart}
          onClick={ctx.goNext}
        >
          Continue
          <ArrowRight size={15} strokeWidth={2} />
        </button>

        {!micGranted && (
          <p className="text-xs text-[var(--color-text-muted)] text-center max-w-xs">
            Allow microphone access to continue — Humla can't record without it.
          </p>
        )}

        {/* Escape hatch: for the impatient, and for the case where relaunch()
            itself fails. Carries its consequence. */}
        {micGranted && needsRestart && (
          <button
            type="button"
            onClick={ctx.goNext}
            className="text-xs text-[var(--color-text-muted)] hover:text-[var(--color-text)] transition-colors text-center max-w-sm leading-relaxed"
          >
            Continue without restarting
            <span className="block text-[var(--color-text-disabled)]">
              System audio won't work until you quit and reopen Humla.
            </span>
          </button>
        )}

        {/* System-audio skip — carries its consequence. Advances; never blocks. */}
        {micGranted && !screenGranted && (
          <button
            type="button"
            onClick={ctx.goNext}
            className="text-xs text-[var(--color-text-muted)] hover:text-[var(--color-text)] transition-colors text-center max-w-sm leading-relaxed"
          >
            Skip — I only record in-person meetings.
            <span className="block text-[var(--color-text-disabled)]">
              Remote call audio won't be captured. You can enable it later in
              Settings.
            </span>
          </button>
        )}
      </div>
    </StepShell>
  );
}
