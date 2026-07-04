// Step 7 — "You're all set" status recap (design/ONBOARDING.md § 7).
//
// The wizard's receipt. A checklist of rows, each with a status pill and each
// clickable to jump back to the owning step and fix it. It evaluates the SAME
// shared predicate (computeSetupStatus) as the sidebar nag chip — "all green
// here" and "no nag" are one source of truth, they can never drift.
//
// The transcription row shows a LIVE progress bar while the on-device model is
// still downloading (subscribed off the global download slice in store.ts), so
// a user who finished the wizard mid-download still sees it move.
//
// Primary CTA: "Create your first note" — creates a real note and lands the
// user IN it, wizard gone. The create → complete(destination) sequence hands
// the route to the App-level onDone so BOTH entry modes end on /note/<id>.
import { useCallback, useEffect, useState } from "react";
import {
  Mic,
  MonitorSpeaker,
  HardDriveDownload,
  Sparkles,
  Cloud,
  Languages,
  Check,
  ChevronRight,
  PlusCircle,
} from "lucide-react";
import { ipc } from "../../../lib/ipc";
import { useDownloadStore, useNotesStore } from "../../../lib/store";
import { computeSetupStatus, type SetupStatus } from "../../../lib/setupStatus";
import { LANGUAGES } from "../../../lib/languages";
import type { StepContext } from "../types";
import { StepShell } from "../StepShell";

type PillTone = "ok" | "muted" | "warn";

function providerLabel(provider: string): string {
  switch (provider) {
    case "openai":
      return "OpenAI";
    case "deepgram":
      return "Deepgram";
    case "groq":
      return "Groq";
    case "local":
      return "On-device";
    default:
      return provider;
  }
}

function languageDisplay(code: string): string {
  if (code === "auto" || !code) return "Auto-detect";
  const l = LANGUAGES.find((x) => x.value === code);
  if (!l) return code;
  return l.native && l.native !== l.label ? l.native : l.label;
}

export function DoneStep({ ctx }: { ctx: StepContext }) {
  const [status, setStatus] = useState<SetupStatus | null>(null);
  const [creating, setCreating] = useState(false);
  // Live download progress from the global slice — drives the transcription
  // row's progress bar and re-triggers a status recompute on completion.
  const download = useDownloadStore((s) => s.active);
  const upsertLocal = useNotesStore((s) => s.upsertLocal);

  // Refetch the full status only on download TRANSITIONS (start / finish /
  // model change), never on the 100ms progress ticks — computeSetupStatus
  // spawns the permissions sidecar, so a per-tick refetch would fork a process
  // ~10×/sec for the length of a model download. The live percentage is merged
  // into the rendered status below instead.
  const downloadKey = download?.modelId ?? null;

  const refresh = useCallback(async () => {
    const s = await computeSetupStatus().catch(() => null);
    if (s) setStatus(s);
  }, []);

  // Recompute on mount, on window focus (returning from System Settings after
  // granting a permission — mirrors Permissions.tsx), and on download
  // transitions (so completing a download flips the row to ready).
  useEffect(() => {
    void refresh();
    const onFocus = () => void refresh();
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [refresh, downloadKey]);

  async function createFirstNote() {
    if (creating) return;
    setCreating(true);
    try {
      const note = await ipc.createNote();
      upsertLocal(note);
      // Hand the destination to the App-level onDone: it navigates to the note
      // AND completes the wizard in one shot, in both entry modes.
      ctx.complete({ destination: `/note/${note.id}` });
    } catch (e) {
      console.warn("[onboarding] failed to create first note:", e);
      setCreating(false);
      // Fall back to completing without a destination so the user isn't stuck.
      ctx.complete();
    }
  }

  // Rows read from the resolved status, with the LIVE download progress merged
  // in at render time (the fetched status is stable between transitions; the
  // slice ticks every ~100ms). Until it resolves, show skeletal rows with
  // muted pills so the shell chrome stays stable.
  const s: SetupStatus | null =
    status &&
    download &&
    status.stt.kind === "local" &&
    !status.stt.working &&
    download.modelId === status.stt.model
      ? { ...status, stt: { ...status.stt, downloading: download } }
      : status;

  return (
    <StepShell
      icon={<Check size={26} strokeWidth={1.9} />}
      title="You're all set"
      subtitle="Here's how Humla is configured. Anything can be changed later in Settings — click a row to adjust it now."
    >
      <div className="w-full max-w-md flex flex-col gap-2 text-left">
        {/* Microphone — required; the only row that reads as a gap when unmet. */}
        <ChecklistRow
          icon={<Mic size={16} strokeWidth={1.8} />}
          label="Microphone"
          value={
            s == null
              ? "Checking…"
              : s.micGranted
                ? "Granted"
                : "Not granted"
          }
          tone={s == null ? "muted" : s.micGranted ? "ok" : "warn"}
          onClick={() => ctx.goTo("permissions")}
        />

        {/* System audio — skippable; "Skipped" is NOT a failure state. */}
        <ChecklistRow
          icon={<MonitorSpeaker size={16} strokeWidth={1.8} />}
          label="System audio"
          value={
            s == null
              ? "Checking…"
              : s.screenGranted
                ? "Granted"
                : "Skipped — in-person only"
          }
          tone={s == null ? "muted" : s.screenGranted ? "ok" : "muted"}
          onClick={() => ctx.goTo("permissions")}
        />

        {/* Transcription — provider + model; live progress bar while downloading. */}
        <ChecklistRow
          icon={<HardDriveDownload size={16} strokeWidth={1.8} />}
          label="Transcription"
          value={
            s == null
              ? "Checking…"
              : s.stt.downloading
                ? "Downloading model…"
                : s.stt.working
                  ? transcriptionValue(s)
                  : "Not set up"
          }
          tone={
            s == null
              ? "muted"
              : s.stt.working
                ? "ok"
                : s.stt.downloading
                  ? "muted"
                  : "warn"
          }
          onClick={() => ctx.goTo("transcription")}
        >
          {s?.stt.downloading && (
            <div className="mt-2">
              <div className="h-1 rounded bg-[var(--color-pill-hover)] overflow-hidden">
                <div
                  className="h-full bg-[var(--color-accent)] transition-[width] duration-150"
                  style={{
                    width: s.stt.downloading.total
                      ? `${Math.min(
                          100,
                          (s.stt.downloading.received / s.stt.downloading.total) * 100,
                        )}%`
                      : "25%",
                  }}
                />
              </div>
            </div>
          )}
        </ChecklistRow>

        {/* AI Summary — optional; "Not set up — add this later" is neutral. */}
        <ChecklistRow
          icon={<Sparkles size={16} strokeWidth={1.8} />}
          label="AI Summary"
          value={
            s == null
              ? "Checking…"
              : s.summaryConfigured
                ? providerLabel(s.summaryProvider)
                : "Not set up — add this later"
          }
          tone={s == null ? "muted" : s.summaryConfigured ? "ok" : "muted"}
          onClick={() => ctx.goTo("summary")}
        />

        {/* Humla Cloud — informational; "Local only" is the happy default. */}
        <ChecklistRow
          icon={<Cloud size={16} strokeWidth={1.8} />}
          label="Humla Cloud"
          value={s == null ? "Checking…" : (s.cloudWorkspace ?? "Local only")}
          tone={s == null ? "muted" : s.cloudWorkspace ? "ok" : "muted"}
          onClick={() => ctx.goTo("cloud")}
        />

        {/* Language — meeting language display name. */}
        <ChecklistRow
          icon={<Languages size={16} strokeWidth={1.8} />}
          label="Language"
          value={s == null ? "Checking…" : languageDisplay(s.language)}
          tone="muted"
          onClick={() => ctx.goTo("language")}
        />
      </div>

      {/* Primary CTA + the quiet per-language routing pointer. */}
      <div className="mt-8 w-full max-w-md flex flex-col items-center gap-3">
        <button
          type="button"
          className="nd-btn nd-btn-primary"
          onClick={createFirstNote}
          disabled={creating}
        >
          <PlusCircle size={15} strokeWidth={2} />
          {creating ? "Creating…" : "Create your first note"}
        </button>
        <p className="text-xs text-[var(--color-text-muted)] text-center max-w-sm leading-relaxed">
          Meetings in several languages? Set per-language transcription in
          Settings → Transcription.
        </p>
      </div>
    </StepShell>
  );
}

function transcriptionValue(s: SetupStatus): string {
  const provider = providerLabel(s.stt.provider);
  const model = s.stt.model;
  return model ? `${provider} · ${model}` : provider;
}

// One recap row: leading icon, label, a status pill, and a chevron affording
// "click to fix". Optional children render below (the download progress bar).
function ChecklistRow({
  icon,
  label,
  value,
  tone,
  onClick,
  children,
}: {
  icon: React.ReactNode;
  label: string;
  value: string;
  tone: PillTone;
  onClick: () => void;
  children?: React.ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="group text-left rounded-[var(--radius)] border border-[var(--color-line)] bg-[var(--color-surface)] px-4 py-3 hover:border-[var(--color-line-visible)] transition-colors"
    >
      <div className="flex items-center gap-3">
        <span className="shrink-0 text-[var(--color-text-muted)]">{icon}</span>
        <span className="text-sm font-medium text-[var(--color-text)] shrink-0">
          {label}
        </span>
        <span className="flex-1 min-w-0 flex items-center justify-end gap-2">
          <StatusPill tone={tone} text={value} />
          <ChevronRight
            size={15}
            strokeWidth={2}
            className="shrink-0 text-[var(--color-text-disabled)] group-hover:text-[var(--color-text-muted)] transition-colors"
          />
        </span>
      </div>
      {children}
    </button>
  );
}

function StatusPill({ tone, text }: { tone: PillTone; text: string }) {
  const style =
    tone === "ok"
      ? { color: "var(--color-success)", borderColor: "var(--color-success)" }
      : tone === "warn"
        ? { color: "var(--color-warning-text)", borderColor: "var(--color-warning)" }
        : { color: "var(--color-text-muted)", borderColor: "var(--color-line-visible)" };
  return (
    <span className="nd-chip inline-flex items-center gap-1 truncate max-w-[220px]" style={style}>
      {tone === "ok" && <Check size={11} strokeWidth={2.5} className="shrink-0" />}
      <span className="truncate">{text}</span>
    </span>
  );
}
