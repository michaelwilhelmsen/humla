// Shared setup-status predicate — the ONE source of truth for "is Humla
// actually usable?" (design/ONBOARDING.md § "You're all set" + § "Nag chip").
//
// Four consumers evaluate the SAME computed picture and MUST NOT drift:
//   - the wizard's final "You're all set" recap (steps/Done.tsx),
//   - the sidebar nag chip (components/SetupNag.tsx),
//   - firstIncompleteStep (onboarding/types.ts) — where a manual re-run lands,
//   - the record pre-flight gate (pages/Note.tsx's Record button).
//
// The gate/nag predicate is deliberately narrow: `pipelineReady = micGranted &&
// stt.working`. Screen recording, AI summary, and cloud are ALL informational
// only — they never gate recording (the spec: mic-only is a supported mode,
// summary is optional, cloud never nags).

import { ipc, type TranscribeProvider } from "./ipc";
import { cloudApi } from "./cloud";

// A model download the chip / recap can surface as "Downloading — NN%".
// Sourced from the global download slice (store.ts), not recomputed here.
export type DownloadInfo = {
  modelId: string;
  received: number;
  total: number | null;
};

export type SttStatus = {
  // The single predicate the gate/nag cares about: a transcription path that
  // will actually work right now.
  working: boolean;
  // What KIND of path the default config points at, for display:
  //   "local"  — on-device Whisper (working iff the model file is present)
  //   "cloud"  — a cloud provider (working iff its API key is stored)
  //   "none"   — the fresh-install fallback (openai, no key) → not working
  kind: "local" | "cloud" | "none";
  // The provider id from transcribe_config.default (for display).
  provider: TranscribeProvider;
  // The default model id / name (for display).
  model: string;
  // Populated only while `working` is false because a local model is still
  // downloading — lets the chip show progress instead of nagging.
  downloading?: DownloadInfo;
};

export type SetupStatus = {
  micGranted: boolean;
  // Informational — never gates. (mic-only / in-person is a supported mode.)
  screenGranted: boolean;
  stt: SttStatus;
  // Informational — never gates. (summaries are optional.)
  summaryConfigured: boolean;
  summaryProvider: string;
  // Informational. Workspace name, or null when local-only / signed out.
  cloudWorkspace: string | null;
  // The meeting language code (e.g. "no", "en", "auto").
  language: string;
  // THE nag/gate predicate: micGranted && stt.working.
  pipelineReady: boolean;
};

// A cloud provider whose key lives in the Keychain and whose presence means
// "working". Local has no key; openai/deepgram/groq do.
function isCloudProvider(p: TranscribeProvider): p is "openai" | "deepgram" | "groq" {
  return p === "openai" || p === "deepgram" || p === "groq";
}

// Compute the full setup picture from live backend state. Every read is
// individually fault-tolerant so a single failing IPC (e.g. a sidecar that
// isn't present in some build config) degrades to "not configured" rather than
// throwing — the gate then simply nags, which is the safe direction.
//
// NOTE: this spawns the permissions sidecar per call — callers must only
// invoke it on real state transitions (mount, focus, download start/finish),
// never per download-progress tick. Live download progress is merged in at
// render time by the consumers (SetupNag, Done) from the store slice; the
// `stt.downloading` field exists for that merge.
export async function computeSetupStatus(): Promise<SetupStatus> {
  const [perm, cfg, models, language, summaryProvider] = await Promise.all([
    ipc.permissionsStatus().catch(() => null),
    ipc.getTranscribeConfig().catch(() => null),
    ipc.localWhisperModels().catch(() => []),
    ipc.getSetting("language").catch(() => null),
    ipc.getSetting("summary_provider").catch(() => null),
  ]);

  const micGranted = perm?.microphone === "granted";
  const screenGranted = perm?.screen === "granted";

  // ---- STT working? -------------------------------------------------------
  // Default config drives it. Local → the model file must be downloaded.
  // Cloud → its provider key must be stored. Fresh-install fallback (openai,
  // no key) resolves to not-working via the cloud branch (key read is null).
  const def = cfg?.default ?? null;
  let stt: SttStatus;
  if (def?.provider === "local") {
    const modelId = def.model_id;
    const downloaded = models.find((m) => m.id === modelId)?.downloaded ?? false;
    stt = {
      working: downloaded,
      kind: "local",
      provider: "local",
      model: modelId,
    };
  } else if (def && isCloudProvider(def.provider)) {
    const key = await ipc.getProviderKey(def.provider).catch(() => null);
    const working = !!key;
    stt = {
      working,
      // A cloud default with no key is the "none" (fresh-install) state — the
      // record path is not functional and the nag must fire.
      kind: working ? "cloud" : "none",
      provider: def.provider,
      model: def.model,
    };
  } else {
    // No config at all — treat as the fresh-install fallback.
    stt = { working: false, kind: "none", provider: "openai", model: "" };
  }

  // ---- Summary configured? (informational) --------------------------------
  // openai → a key exists; local → a model name is set. Never gates.
  let summaryConfigured = false;
  if (summaryProvider === "local") {
    const model = await ipc.getSetting("local_llm_model").catch(() => null);
    summaryConfigured = !!model && model.trim().length > 0;
  } else {
    // Default provider is openai — configured iff its key is present. (This
    // read may duplicate the STT one above; it's cheap and keeps the branches
    // independent.)
    const key = await ipc.getProviderKey("openai").catch(() => null);
    summaryConfigured = !!key;
  }

  // ---- Cloud workspace (informational) ------------------------------------
  // Read via the cloud command directly rather than the React store so this
  // function has no store dependency and can run outside a component.
  let cloudWorkspace: string | null = null;
  try {
    const status = await cloudApi.status();
    cloudWorkspace = status.current_workspace?.name ?? null;
  } catch {
    cloudWorkspace = null;
  }

  return {
    micGranted,
    screenGranted,
    stt,
    summaryConfigured,
    summaryProvider: summaryProvider ?? "openai",
    cloudWorkspace,
    language: language ?? "auto",
    pipelineReady: micGranted && stt.working,
  };
}
