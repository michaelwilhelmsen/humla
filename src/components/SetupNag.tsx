// Sidebar setup-nag chip (design/ONBOARDING.md § "Nag chip").
//
// Renders only when the recording pipeline isn't functional — the SAME
// predicate the "You're all set" recap and the record gate use
// (computeSetupStatus → pipelineReady). It's calm, not alarming: gold accent,
// one line, one click into the wizard at the first incomplete step.
//
// Three states:
//   - hidden          — pipelineReady (mic granted AND a working STT path).
//   - "Finish setup"  — a genuine gap (no mic, or no working STT and nothing
//                        downloading).
//   - "Downloading model — NN%" — the ONLY gap is an in-flight model download
//                        and mic is granted. The user did everything right;
//                        nagging mid-download would read as a false accusation.
//
// Re-evaluates on mount, on window focus (returning from System Settings after
// granting mic — mirrors Permissions.tsx), and whenever the global download
// slice changes. No interval polling.
import { useCallback, useEffect, useState } from "react";
import { useNavigate } from "react-router-dom";
import { AlertCircle, Download } from "lucide-react";
import { useDownloadStore } from "../lib/store";
import { computeSetupStatus, type SetupStatus } from "../lib/setupStatus";

export function SetupNag() {
  const navigate = useNavigate();
  const [status, setStatus] = useState<SetupStatus | null>(null);
  const download = useDownloadStore((s) => s.active);

  // Recompute the full status only on download TRANSITIONS (start / finish /
  // model change), never on the 100ms progress ticks — computeSetupStatus
  // spawns the permissions sidecar, so a per-tick recompute would fork a
  // process ~10×/sec for the length of a model download. The live percentage
  // for display comes straight from the store slice below instead.
  const downloadKey = download?.modelId ?? null;

  const refresh = useCallback(async () => {
    const s = await computeSetupStatus().catch(() => null);
    setStatus(s);
  }, []);

  useEffect(() => {
    void refresh();
    const onFocus = () => void refresh();
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [refresh, downloadKey]);

  // Not resolved yet, or the pipeline is ready → render nothing.
  if (!status || status.pipelineReady) return null;

  // Download-progress variant: mic is granted and the sole remaining gap is a
  // model still downloading. Show progress instead of a nag. Derived from the
  // live slice (updates every tick) against the fetched status (stable).
  const dl =
    status.micGranted &&
    status.stt.kind === "local" &&
    !status.stt.working &&
    download &&
    download.modelId === status.stt.model
      ? download
      : undefined;
  const pct =
    dl && dl.total != null && dl.total > 0
      ? Math.min(100, Math.round((dl.received / dl.total) * 100))
      : null;

  const downloading = !!dl;
  const label = downloading
    ? pct != null
      ? `Downloading model — ${pct}%`
      : "Downloading model…"
    : "Finish setup";

  return (
    <button
      type="button"
      onClick={() => navigate("/onboarding")}
      title={downloading ? "Model downloading — finish setup" : "Finish setting up Humla"}
      className="no-drag mt-1 flex items-center gap-2.5 px-2.5 py-2 rounded-[var(--radius)] text-[13px] font-medium transition-colors"
      style={{
        color: "var(--color-warning-text)",
        background: "var(--color-accent-soft)",
      }}
    >
      {downloading ? (
        <Download size={15} strokeWidth={1.8} className="shrink-0" />
      ) : (
        <AlertCircle size={15} strokeWidth={1.8} className="shrink-0" />
      )}
      <span className="flex-1 truncate text-left">{label}</span>
    </button>
  );
}
