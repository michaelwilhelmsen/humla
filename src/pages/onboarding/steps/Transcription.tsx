// Step 4 — Transcription (design/ONBOARDING.md § 4. Transcription).
//
// The local-vs-cloud fork. TWO CARDS, not a provider list:
//
//   Card A — "On-device": private, free, offline. Model is chosen by the
//     step-3 `language` setting (Norwegian → nb-whisper-large-q5, else
//     large-v3-turbo-q5) and so is DERIVED, not stored. Whenever this card is
//     the selected one, an effect keeps transcribe_config.default in agreement
//     with that derived model and fetches it — including when the user goes
//     back and changes the language, which used to leave the card advertising
//     one model while the config still routed to another (#148). Clicking the
//     card selects it and asks for the model; the effect owns the config.
//   Card B — "Cloud API": provider dropdown (OpenAI / Deepgram / Groq) +
//     key field with Save + Test. A passed Test writes transcribe_config
//     .default = that provider with the same default model Settings uses.
//
// Intel check: on non-aarch64, the "Recommended" badge moves to Card B and
// a one-liner warns on-device is slow.
//
// Continue gates on: Card A selected (download may still run — fine) OR
// Card B has a key that passed Test. There's a quiet "Skip for now" too.
//
// On leaving forward (Continue or Skip), silently fire diarize_download if
// the diarize model isn't present yet — small, universally needed, errors
// non-fatal.
//
// Download survival across navigation: the download runs on the Rust side
// (local_whisper_download) independently of this component. Unmount doesn't
// cancel it. Progress, completion, and failure are read from the global
// useDownloadStore slice — fed by the app's single local_whisper_progress /
// download-error listener in bindBackendListeners — so navigating back
// mid-download re-attaches to the running download instead of double-starting
// it. Same pattern as ModelDownloadCard.
import { useCallback, useEffect, useRef, useState } from "react";
import {
  HardDriveDownload,
  Cloud,
  Check,
  Lock,
  Zap,
} from "lucide-react";
import {
  ipc,
  type LocalWhisperModelStatus,
  type ProviderConfig,
} from "../../../lib/ipc";
import {
  chosenCloudProvider,
  type CloudTranscribeProvider,
} from "../../../lib/transcribeDefault";
import { useDownloadStore } from "../../../lib/store";
import { useProviderKey } from "../../../components/provider/useProviderKey";
import type { StepContext } from "../types";
import { StepShell } from "../StepShell";
import { Select } from "../../../components/ui/Select";

// ---- Model routing (mirrors design/ONBOARDING.md § 4) ---------------------
// Norwegian meetings get the National Library's NB Whisper; everything else
// gets the multilingual turbo default. IDs match the backend registry
// (src-tauri/src/local_whisper.rs).
const NB_MODEL_ID = "nb-whisper-large-q5";
const DEFAULT_MODEL_ID = "large-v3-turbo-q5";

function modelIdForLanguage(language: string | null): string {
  const l = (language ?? "").toLowerCase();
  return l === "no" || l === "nb" || l === "nn" ? NB_MODEL_ID : DEFAULT_MODEL_ID;
}

// The local ProviderConfig Settings writes — same preset + gpu defaults.
function localConfig(modelId: string): ProviderConfig {
  return { provider: "local", model_id: modelId, preset: "quality", use_gpu: true };
}

// Same per-provider default models Settings → Transcription uses
// (see ProviderConfigForm.tsx / settings/types.ts).
function cloudConfig(provider: CloudTranscribeProvider): ProviderConfig {
  switch (provider) {
    case "openai":
      return { provider: "openai", model: "whisper-1" };
    case "deepgram":
      return { provider: "deepgram", model: "nova-3" };
    case "groq":
      return { provider: "groq", model: "whisper-large-v3-turbo" };
  }
}

const CLOUD_PROVIDERS: { value: CloudTranscribeProvider; label: string }[] = [
  { value: "openai", label: "OpenAI" },
  { value: "deepgram", label: "Deepgram" },
  { value: "groq", label: "Groq (Whisper Large v3 Turbo)" },
];

function keyPlaceholder(p: CloudTranscribeProvider): string {
  return p === "openai" ? "sk-…" : p === "deepgram" ? "Deepgram API key" : "gsk_…";
}

type Selection = "local" | "cloud" | null;

// Set transcribe_config.default, preserving whatever per-language overrides the
// user already has. Both commit points (the on-device model and a passed cloud
// key Test) need exactly this, and per-language overrides are easy to drop by
// writing a bare config — so there's one place that gets it right.
async function writeDefaultProvider(next: ProviderConfig) {
  const cfg = await ipc.getTranscribeConfig().catch(() => null);
  await ipc.setTranscribeConfig({
    default: next,
    per_language: cfg?.per_language ?? {},
  });
}

export function TranscriptionStep({ ctx }: { ctx: StepContext }) {
  const [language, setLanguage] = useState<string | null>(null);
  // Whether `language` reflects a successful read. Only then may the reconcile
  // effect rewrite a stored config on its own initiative.
  const [languageKnown, setLanguageKnown] = useState(false);
  const [arch, setArch] = useState<string | null>(null);
  const [models, setModels] = useState<LocalWhisperModelStatus[] | null>(null);

  const [selection, setSelection] = useState<Selection>(null);

  // Local-download UI state. Live progress and terminal failure come from the
  // global download slice (survives this step's unmount); `downloadStarted`
  // bridges the gap between clicking the card and the first progress event,
  // and `downloadDone` records on-disk presence.
  const [downloadStarted, setDownloadStarted] = useState(false);
  const [downloadDone, setDownloadDone] = useState(false);
  // A failed config write. Surfaced on the card: Continue is NOT gated on it,
  // so without this the step would silently disagree with what it displays.
  const [configError, setConfigError] = useState<string | null>(null);
  const activeDownload = useDownloadStore((s) => s.active);
  const downloadFailure = useDownloadStore((s) => s.error);

  // Cloud card state. Key mechanics come from the shared hook (#22) — it
  // resets the surface itself when the provider changes; the step wraps
  // test() with its commit point (write the provider as the default).
  const [cloudProvider, setCloudProvider] = useState<CloudTranscribeProvider>("openai");
  const key = useProviderKey(cloudProvider);

  const modelId = modelIdForLanguage(language);
  const chosenModel = models?.find((m) => m.id === modelId) ?? null;
  const isNorwegian = modelId === NB_MODEL_ID;
  const isIntel = arch !== null && arch !== "aarch64";

  // This step's slice of the global download state.
  const mine = activeDownload?.modelId === modelId ? activeDownload : null;
  const myError = downloadFailure?.modelId === modelId ? downloadFailure.message : null;

  // Load language, arch, and the model registry (for honest sizes + the
  // already-downloaded check). Then reconcile local state: if the chosen
  // model is already downloaded, treat local as complete; if a download is
  // in flight (navigated back), reflect that.
  useEffect(() => {
    let cancelled = false;
    Promise.all([
      // `read: false` distinguishes "the read failed" from "the setting is
      // unset". Both used to collapse to null, and null routes to the
      // multilingual default — so a failed read would have let the reconcile
      // effect below "correct" a perfectly good Norwegian config down to the
      // default model and fetch a gigabyte to do it.
      ipc
        .getSetting("language")
        .then((v) => ({ read: true, value: v }))
        .catch(() => ({ read: false, value: null as string | null })),
      ipc.systemArch().catch(() => "aarch64"),
      ipc.localWhisperModels().catch(() => [] as LocalWhisperModelStatus[]),
      ipc.getTranscribeConfig().catch(() => null),
    ]).then(async ([langRead, a, ms, cfg]) => {
      if (cancelled) return;
      const lang = langRead.value;
      setLanguage(lang);
      setLanguageKnown(langRead.read);
      setArch(a);
      setModels(ms);

      const wantId = modelIdForLanguage(lang);
      const already = ms.find((m) => m.id === wantId)?.downloaded ?? false;

      // Pre-select from live config so a resuming user sees their prior
      // choice. An in-flight download needs no special-casing here: the
      // store-transition effect below reflects it the moment it renders.
      // Display-only: the commit points (below) still own every write.
      if (cfg?.default.provider === "local") {
        setSelection("local");
        // Seed the commit guard with what's actually stored, so the reconcile
        // effect can tell "already agrees" (write nothing) from "the language
        // moved on since" (rewrite + fetch the new model).
        committedModelRef.current = cfg.default.model_id;
        if (already) setDownloadDone(true);
        return;
      }

      // A cloud default is only evidence of a choice once its key is stored —
      // see transcribeDefault.ts for why, and computeSetupStatus for the other
      // reader of the same rule. The old guard here was "any cloud provider
      // except openai", which could never resume the likeliest cloud pick
      // (#149). Null covers no-config, a local default, and an unreadable
      // key alike; all three leave the step in its fresh-install state.
      const chosen = await chosenCloudProvider(cfg?.default);
      if (cancelled || !chosen) return;
      setSelection("cloud");
      setCloudProvider(chosen);
    });
    return () => {
      cancelled = true;
    };
  }, []);

  // Store-transition tracking. A live download for this step's model means
  // the panel must show (covers navigating back mid-download — the job the
  // old module-level in-flight flag did); the terminal transition (slice
  // cleared on completion or failure) triggers a presence refetch to learn
  // the outcome, exactly like ModelDownloadCard. Completion via this path is
  // what lets a user who navigated away and back still see "Model ready" —
  // the startDownload() promise dies with the mount that created it.
  const wasMine = useRef(false);
  useEffect(() => {
    if (mine) {
      wasMine.current = true;
      setDownloadStarted(true);
    } else if (wasMine.current) {
      wasMine.current = false;
      ipc
        .localWhisperModels()
        .then((ms) => {
          if (!ms) return;
          setModels(ms);
          if (ms.find((m) => m.id === modelId)?.downloaded) setDownloadDone(true);
        })
        .catch(() => {});
    }
  }, [mine, modelId]);

  // Start (or resume awareness of) the on-device model download.
  const startDownload = useCallback(
    async (id: string) => {
      // Already on disk — nothing to download.
      const status = await ipc.localWhisperModels().catch(() => null);
      if (status?.find((m) => m.id === id)?.downloaded) {
        setModels(status);
        setDownloadDone(true);
        return;
      }
      const downloads = useDownloadStore.getState();
      if (downloads.active !== null) {
        // A download is already running (this step's or another surface's —
        // one at a time app-wide); just reflect the in-flight state.
        setDownloadStarted(true);
        return;
      }
      // Retrying after a failure: drop the stale error before re-invoking.
      downloads.clear();
      setDownloadStarted(true);
      try {
        await ipc.localWhisperDownload(id);
        // The command resolves when the file is fully written + renamed —
        // authoritative while this mount lives (the store transition above
        // covers the navigated-away case).
        setDownloadDone(true);
        const refreshed = await ipc.localWhisperModels().catch(() => null);
        if (refreshed) setModels(refreshed);
      } catch (e) {
        // Immediate spawn failure. The backend emits a download-error event
        // for this too; failing the slice directly just makes the outcome
        // independent of listener timing (same terminal state either way).
        useDownloadStore.getState().fail({ modelId: id, message: String(e) });
      }
    },
    [],
  );

  // The model whose config has been persisted and whose download has been
  // kicked off. Seeded from the stored config on mount, so resuming a step
  // that already agrees with the language writes nothing.
  const committedModelRef = useRef<string | null>(null);

  // Per-mount dedup for download starts. `startDownload` already no-ops for an
  // on-disk model and merely reflects an in-flight one, but two calls racing
  // each other before the store's `active` slice is set would both invoke.
  const requestedDownloadRef = useRef<string | null>(null);
  const requestDownload = useCallback(
    (id: string) => {
      if (requestedDownloadRef.current === id) return;
      requestedDownloadRef.current = id;
      void startDownload(id);
    },
    [startDownload],
  );

  // Persist transcribe_config.default = local + `id`, preserving any
  // per-language overrides, and fetch that model. Driven by the effect below,
  // so choosing the card and having the language change under it are the same
  // code path.
  const commitLocalModel = useCallback(
    async (id: string) => {
      if (committedModelRef.current === id) return;
      committedModelRef.current = id;
      try {
        await writeDefaultProvider(localConfig(id));
        setConfigError(null);
      } catch (e) {
        // Clear the claim so re-selecting the card retries, and SAY SO on the
        // card. Swallowing this was the whole bug: a card advertising NB Whisper
        // over a config still routing to the default model IS #148 again.
        // Continue stays enabled regardless — deliberately, it must not become
        // harder to proceed than before — so silence here would let the user
        // walk out with the wrong model and nothing on screen to say so.
        committedModelRef.current = null;
        setConfigError(String(e));
        console.warn("[onboarding] failed to write local transcribe_config:", e);
      }
      // Newly configured model → it has to actually be on disk. NOT done when
      // the config already agreed: merely mounting must never kick off a
      // download, because `startDownload` clears the global download slice to
      // drop a stale error, which would erase a failure the user still needs
      // to see. Wanting the model is the click's job, below.
      requestDownload(id);
    },
    [requestDownload],
  );

  function selectLocal() {
    setSelection("local");
    // A click is explicit intent AND the only retry affordance on this card, so
    // it drops the download claim: a failed fetch has to be re-attemptable, and
    // before this the guard made "Download failed" a dead end for the life of
    // the mount. `startDownload` still refuses to double-start something already
    // in flight or on disk, so dropping the claim is safe.
    requestedDownloadRef.current = null;
    // Also covers a wizard abandoned mid-download: the config already names the
    // model, so the effect won't act, but nothing is on disk. commitLocalModel
    // no-ops when the config already agrees, and retries it when it didn't take.
    void commitLocalModel(modelId);
    requestDownload(modelId);
  }

  // #148 — the advertised model is DERIVED from the meeting language, but the
  // config used to be written only by the click handler. Changing the language
  // after choosing On-device therefore left the card promising NB Whisper while
  // the stored config still routed to the multilingual turbo model, Continue
  // enabled and NB Whisper never fetched — silently the wrong model. Keeping
  // the two in agreement is this effect's whole job; `committedModelRef` makes
  // it a no-op whenever they already agree, which matters because this step
  // re-renders on every download-store tick.
  useEffect(() => {
    if (selection !== "local") return;
    // Only reconcile on our own initiative once the language is actually known.
    // An unread language falls back to the multilingual default, which would
    // otherwise "correct" a good Norwegian config down to it. A click still
    // commits, so a failed read costs nothing the user asked for.
    if (!languageKnown) return;
    void commitLocalModel(modelId);
  }, [selection, modelId, languageKnown, commitLocalModel]);

  function selectCloud() {
    setSelection("cloud");
  }

  async function testKey() {
    if (!(await key.test())) return;
    // A passed Test is the commit point: write the cloud provider as
    // the default, preserving per-language overrides.
    try {
      await writeDefaultProvider(cloudConfig(cloudProvider));
    } catch (e) {
      console.warn("[onboarding] failed to write cloud transcribe_config:", e);
    }
  }

  // Each provider has its own Keychain slot + Test; the hook resets its
  // surface when this changes.
  function changeCloudProvider(p: CloudTranscribeProvider) {
    setCloudProvider(p);
  }

  // Fire the diarize download once when leaving the step forward. Non-fatal.
  const diarizeFiredRef = useRef(false);
  async function fireDiarizeDownload() {
    if (diarizeFiredRef.current) return;
    diarizeFiredRef.current = true;
    try {
      const status = await ipc.diarizeStatus("community1");
      if (!status.downloaded) {
        // Fire-and-forget; the command runs to completion on the backend.
        void ipc.diarizeDownload("community1").catch((e) => {
          console.warn("[onboarding] diarize download failed:", e);
        });
      }
    } catch (e) {
      console.warn("[onboarding] diarize status check failed:", e);
    }
  }

  function proceed() {
    void fireDiarizeDownload();
    ctx.goNext();
  }

  // Continue enables when: local selected (download may still run) OR a
  // cloud key passed Test.
  const canContinue =
    selection === "local" || (selection === "cloud" && key.result?.ok === true);

  // arch and models always resolve (their fetches fall back on error), so
  // this only covers the initial IPC round-trip. `language` is deliberately
  // NOT part of the gate: it stays null when the setting is unset or the
  // read fails, and modelIdForLanguage(null) already falls back to the
  // multilingual default — gating on it disabled the On-device card forever
  // on fresh installs.
  const loading = arch === null || models === null;

  return (
    <StepShell
      icon={<HardDriveDownload size={26} strokeWidth={1.6} />}
      title="How should Humla transcribe?"
      subtitle="Run it privately on your Mac, or use a cloud API. You can change this any time in Settings."
    >
      <div className="w-full max-w-md flex flex-col gap-3 text-left">
        {/* Card A — On-device */}
        <button
          type="button"
          onClick={selectLocal}
          disabled={loading}
          aria-busy={loading}
          className={
            "text-left rounded-[var(--radius)] border px-4 py-4 transition-colors disabled:opacity-60 disabled:cursor-wait " +
            (selection === "local"
              ? "border-[var(--color-accent)] bg-[var(--color-accent-soft)]"
              : "border-[var(--color-line)] bg-[var(--color-surface)] hover:border-[var(--color-line-visible)]")
          }
        >
          <div className="flex items-start gap-3">
            <Lock
              size={18}
              strokeWidth={1.8}
              className="mt-0.5 shrink-0 text-[var(--color-text-muted)]"
            />
            <div className="min-w-0 flex-1">
              <div className="flex items-center gap-2 flex-wrap">
                <span className="text-sm font-semibold text-[var(--color-text)]">
                  On-device
                </span>
                {!isIntel && <RecommendedBadge />}
                {selection === "local" && (
                  <Check size={15} strokeWidth={2.5} className="text-[var(--color-accent-text)]" />
                )}
              </div>
              <p className="mt-1 text-xs leading-relaxed text-[var(--color-text-muted)]">
                Private, free, works offline. Downloads a{" "}
                {chosenModel
                  ? `~${formatSize(chosenModel.sizeBytesHint)}`
                  : "one-time"}{" "}
                model.
              </p>
              {isNorwegian && (
                <p className="mt-1 text-xs leading-relaxed text-[var(--color-text-muted)]">
                  Norwegian model — trained by the National Library of Norway.
                </p>
              )}
              {isIntel && (
                <p className="mt-1 text-xs leading-relaxed text-[var(--color-warning-text)]">
                  On-device transcription is slow on Intel Macs.
                </p>
              )}

              {/* A config write that didn't land. Shown even though Continue
                  stays open, because the alternative is a card advertising a
                  model the stored config doesn't route to — #148 all over. */}
              {selection === "local" && configError !== null && (
                <p className="mt-3 text-xs text-[var(--color-danger)] break-all">
                  Couldn't save this choice: {configError}. Select the card again
                  to retry, or set the model in Settings → Transcription.
                </p>
              )}

              {/* Inline download progress. */}
              {selection === "local" && (downloadStarted || downloadDone || myError !== null) && (
                <div className="mt-3">
                  {myError && !downloadDone ? (
                    <p className="text-xs text-[var(--color-danger)] break-all">
                      Download failed: {myError}
                    </p>
                  ) : downloadDone ? (
                    <p className="text-xs text-[var(--color-success)] flex items-center gap-1.5">
                      <Check size={13} strokeWidth={2.5} />
                      Model ready
                    </p>
                  ) : (
                    <>
                      <div className="text-xs text-[var(--color-text-muted)] mb-1">
                        Downloading
                        {mine?.total
                          ? ` ${formatSize(mine.received)} / ${formatSize(mine.total)}`
                          : mine
                          ? ` ${formatSize(mine.received)}`
                          : ""}
                        … you can continue while it finishes.
                      </div>
                      <div className="h-1 rounded bg-[var(--color-pill-hover)] overflow-hidden">
                        <div
                          className="h-full bg-[var(--color-accent)] transition-[width] duration-150"
                          style={{
                            width:
                              mine?.total
                                ? `${Math.min(100, (mine.received / mine.total) * 100)}%`
                                : "25%",
                          }}
                        />
                      </div>
                    </>
                  )}
                </div>
              )}
            </div>
          </div>
        </button>

        {/* Card B — Cloud API */}
        <div
          className={
            "rounded-[var(--radius)] border px-4 py-4 transition-colors " +
            (selection === "cloud"
              ? "border-[var(--color-accent)] bg-[var(--color-accent-soft)]"
              : "border-[var(--color-line)] bg-[var(--color-surface)]")
          }
        >
          <button
            type="button"
            onClick={selectCloud}
            className="text-left w-full"
          >
            <div className="flex items-start gap-3">
              <Cloud
                size={18}
                strokeWidth={1.8}
                className="mt-0.5 shrink-0 text-[var(--color-text-muted)]"
              />
              <div className="min-w-0 flex-1">
                <div className="flex items-center gap-2 flex-wrap">
                  <span className="text-sm font-semibold text-[var(--color-text)]">
                    Cloud API
                  </span>
                  {isIntel && <RecommendedBadge />}
                  {selection === "cloud" && key.result?.ok === true && (
                    <Check
                      size={15}
                      strokeWidth={2.5}
                      className="text-[var(--color-accent-text)]"
                    />
                  )}
                </div>
                <p className="mt-1 text-xs leading-relaxed text-[var(--color-text-muted)]">
                  Faster setup, no download. Needs an API key.
                </p>
              </div>
            </div>
          </button>

          {/* Provider dropdown + key field, revealed on selection. */}
          {selection === "cloud" && (
            <div className="mt-4 flex flex-col gap-3">
              <Select
                ariaLabel="Provider"
                value={cloudProvider}
                onChange={(v) => changeCloudProvider(v as CloudTranscribeProvider)}
                options={CLOUD_PROVIDERS.map((p) => ({ value: p.value, label: p.label }))}
                className="w-full max-w-none justify-between px-3 py-2 bg-[var(--color-input-bg)] border-[var(--color-line)] hover:bg-[var(--color-input-bg)] focus:border-[var(--color-text-muted)]"
              />

              <div className="flex gap-2">
                <input
                  type="password"
                  value={key.draft}
                  onChange={(e) => key.setDraft(e.target.value)}
                  placeholder={key.hasKey ? "•••••••• stored" : keyPlaceholder(cloudProvider)}
                  className="flex-1 min-w-0 px-3 py-2 rounded-md text-sm bg-[var(--color-input-bg)] border border-[var(--color-line)] focus:border-[var(--color-text-muted)]"
                />
                <button
                  type="button"
                  onClick={key.save}
                  disabled={!key.draft.trim()}
                  className="nd-btn"
                >
                  Save
                </button>
                <button
                  type="button"
                  onClick={testKey}
                  disabled={!key.hasKey || key.testing}
                  className="nd-btn nd-btn-primary"
                >
                  {key.testing ? "Testing…" : "Test"}
                </button>
              </div>

              {key.result?.ok === true && (
                <p className="text-xs text-[var(--color-success)] flex items-center gap-1.5">
                  <Check size={13} strokeWidth={2.5} />
                  Connected — {CLOUD_PROVIDERS.find((p) => p.value === cloudProvider)?.label}
                </p>
              )}
              {key.result?.ok === false && (
                <p className="text-xs text-[var(--color-danger)] break-all">
                  {key.result.message}
                </p>
              )}
            </div>
          )}
        </div>
      </div>

      {/* Continue + Skip. */}
      <div className="mt-8 w-full max-w-md flex flex-col items-center gap-3">
        <button
          type="button"
          className="nd-btn nd-btn-primary"
          disabled={!canContinue}
          onClick={proceed}
        >
          Continue
        </button>

        <button
          type="button"
          onClick={proceed}
          className="text-xs text-[var(--color-text-muted)] hover:text-[var(--color-text)] transition-colors"
        >
          Skip for now
        </button>
      </div>
    </StepShell>
  );
}

function RecommendedBadge() {
  return (
    <span
      className="inline-flex items-center gap-1 text-[10px] font-semibold uppercase tracking-wide px-1.5 py-0.5 rounded"
      style={{
        color: "var(--color-accent-text)",
        background: "var(--color-accent-soft)",
      }}
    >
      <Zap size={10} strokeWidth={2.5} />
      Recommended
    </span>
  );
}

// Compact human-readable bytes (e.g. "602 MB", "1.16 GB"). Local to this
// step so onboarding doesn't reach into the Settings page internals.
function formatSize(bytes: number): string {
  if (bytes >= 1_000_000_000) return `${(bytes / 1_000_000_000).toFixed(2)} GB`;
  if (bytes >= 1_000_000) return `${Math.round(bytes / 1_000_000)} MB`;
  if (bytes >= 1_000) return `${Math.round(bytes / 1_000)} KB`;
  return `${bytes} B`;
}
