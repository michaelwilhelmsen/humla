import { useEffect, useState } from "react";
import {
  ipc,
  onDiarizeDownloadProgress,
  onLocalWhisperDownloadError,
  onLocalWhisperProgress,
  type ProviderConfig,
  type TranscribeConfig,
} from "../../lib/ipc";
import { broadcastSettingChange } from "../../lib/settingsBus";
import {
  DEFAULTS,
  EMPTY_DIARIZE_STATE,
  EMPTY_LOCAL_STATE,
  type DiarizeState,
  type EditableKey,
  type LocalState,
} from "./types";

// One hook to own every piece of Settings page state plus the handlers
// the tabs need. Pulled out of the page component so individual tabs
// can grab only the slices they care about, and so the page renders
// stay focused on layout.
export function useSettings() {
  const [local, setLocal] = useState<LocalState>(EMPTY_LOCAL_STATE);
  const [diarize, setDiarize] = useState<DiarizeState>(EMPTY_DIARIZE_STATE);
  // Parallel state for the Sortformer engine. Tracked independently of
  // community1 so each can be downloaded / deleted on its own. The active
  // engine is decided by the `diarize_model` setting; the manager UI
  // shows both rows so users can have one downloaded but the other active
  // while they decide.
  const [sortformer, setSortformer] = useState<DiarizeState>(EMPTY_DIARIZE_STATE);
  const [s, setS] = useState<Record<EditableKey, string>>(DEFAULTS);
  const [transcribeConfig, setTranscribeConfig] = useState<TranscribeConfig>({
    default: { provider: "openai", model: "whisper-1" },
    per_language: {},
  });

  useEffect(() => {
    let cancelled = false;
    (async () => {
      const [models, ds, ss, cfg] = await Promise.all([
        ipc.localWhisperModels(),
        ipc.diarizeStatus("community1").catch(() => null),
        ipc.diarizeStatus("sortformer").catch(() => null),
        ipc.getTranscribeConfig().catch(() => null),
      ]);
      if (cancelled) return;
      setLocal((p) => ({ ...p, models }));
      setDiarize((p) => ({ ...p, status: ds }));
      setSortformer((p) => ({ ...p, status: ss }));
      if (cfg) setTranscribeConfig(cfg);
      const entries = await Promise.all(
        (Object.keys(DEFAULTS) as EditableKey[]).map(
          async (key) => [key, (await ipc.getSetting(key)) ?? DEFAULTS[key]] as const,
        ),
      );
      if (cancelled) return;
      setS(Object.fromEntries(entries) as Record<EditableKey, string>);
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  // Tauri listen() is async; the .then() can resolve *after* a StrictMode
  // remount has already torn down this effect, leaking the listener. The
  // cancelled flag plus immediate-unsub on race protects against that.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    // Terminal events trigger an async model-status refetch; the seq guard
    // drops any refetch that isn't the newest (two terminal events can fire
    // back-to-back around the backend's atomic rename, and their fetches can
    // resolve out of order — a stale one must not re-add a cleared bar).
    let fetchSeq = 0;
    onLocalWhisperProgress((p) => {
      // Terminal event (received caught up with a known total): completion
      // must be derived HERE, from the event stream — the invoke promise in
      // downloadModel() dies with the mount that started it, and a download
      // that outlives a Settings visit would otherwise sit on a full
      // progress bar reading "Not downloaded" until app restart.
      if (p.total !== null && p.received >= p.total) {
        const seq = ++fetchSeq;
        void ipc
          .localWhisperModels()
          .then((models) => {
            if (cancelled || seq !== fetchSeq) return;
            setLocal((s) => {
              // The loop can emit received == total just before the atomic
              // rename; only clear the bar once the file is really in place.
              // The backend's post-rename event settles the race.
              const inPlace = models.find((m) => m.id === p.modelId)?.downloaded ?? false;
              const next = { ...s.downloading };
              if (inPlace) delete next[p.modelId];
              else next[p.modelId] = { received: p.received, total: p.total };
              return { ...s, models, downloading: next };
            });
          })
          .catch(() => {});
        return;
      }
      setLocal((s) => ({
        ...s,
        downloading: {
          ...s.downloading,
          [p.modelId]: { received: p.received, total: p.total },
        },
      }));
    }).then((u) => {
      if (cancelled) u();
      else unlisten = u;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  // Failure counterpart: without this, a download that errors after its
  // initiating mount is gone leaves a forever-progress bar.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    onLocalWhisperDownloadError((e) => {
      setLocal((s) => {
        const next = { ...s.downloading };
        delete next[e.modelId];
        return { ...s, downloading: next, error: e.message };
      });
    }).then((u) => {
      if (cancelled) u();
      else unlisten = u;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    onDiarizeDownloadProgress((p) => {
      // Route the progress event to whichever engine's state it belongs
      // to. Both engines share the channel; we filter by the engine
      // tag the backend includes in the payload.
      const update = (s: DiarizeState) => ({
        ...s,
        fraction: p.fraction,
        phase: p.phase,
      });
      if (p.engine === "sortformer") setSortformer(update);
      else setDiarize(update);
    }).then((u) => {
      if (cancelled) u();
      else unlisten = u;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  // Local-model flash helper. 8s clear (instead of the 4s used by other
  // flashes) — gives the user time to read + act on the
  // `suggest_language_override` affordance after downloading a
  // language-specific model. Identity-keyed clear: if a fresh action
  // replaces the toast, the stale timer doesn't wipe the new one.
  function flashLocal(flash: NonNullable<LocalState["flash"]>) {
    setLocal((p) => ({ ...p, flash }));
    window.setTimeout(() => {
      setLocal((p) => (p.flash === flash ? { ...p, flash: null } : p));
    }, 8000);
  }
  function flashDiarize(msg: string) {
    setDiarize((p) => ({ ...p, flash: msg }));
    window.setTimeout(() => {
      setDiarize((p) => (p.flash === msg ? { ...p, flash: null } : p));
    }, 4000);
  }

  function flashSortformer(msg: string) {
    setSortformer((p) => ({ ...p, flash: msg }));
    window.setTimeout(() => {
      setSortformer((p) => (p.flash === msg ? { ...p, flash: null } : p));
    }, 4000);
  }

  // Local-LLM model listing moved into OllamaConnect
  // (src/components/provider/), which owns probing/polling and the
  // empty-selection self-heal — nothing here tracks llm models anymore.

  async function downloadModel(modelId: string) {
    setLocal((p) => ({
      ...p,
      downloading: { ...p.downloading, [modelId]: { received: 0, total: null } },
      error: null,
      flash: null,
    }));
    try {
      await ipc.localWhisperDownload(modelId);
      const models = await ipc.localWhisperModels();
      setLocal((p) => {
        const next = { ...p.downloading };
        delete next[modelId];
        return { models, downloading: next, error: null, flash: null };
      });
      // First downloaded multilingual auto-becomes the default's
      // model_id. Language-specific models never become the default;
      // they're picked via per-language overrides. Only fires when the
      // user is on the local provider.
      const downloadedInfo = models.find((m) => m.id === modelId);
      if (
        downloadedInfo?.kind === "multilingual" &&
        models.filter((m) => m.kind === "multilingual" && m.downloaded).length === 1 &&
        transcribeConfig.default.provider === "local"
      ) {
        await setDefaultConfig({
          ...transcribeConfig.default,
          model_id: modelId,
        });
      }
      const downloaded = models.find((m) => m.id === modelId);
      const label = downloaded?.label ?? modelId;
      // For language-specific models, surface a one-click "Add as
      // <language> override?" affordance so the user gets the v0.23
      // auto-apply convenience back without the implicit routing.
      // Skip when an override for that language already exists.
      if (
        downloaded?.kind === "language_specific" &&
        downloaded.specificLanguage &&
        !(downloaded.specificLanguage in transcribeConfig.per_language)
      ) {
        flashLocal({
          kind: "suggest_language_override",
          message: `${label} downloaded.`,
          language: downloaded.specificLanguage,
          modelId,
        });
      } else {
        flashLocal({ kind: "info", message: `${label} downloaded` });
      }
    } catch (e) {
      const models = await ipc.localWhisperModels().catch(() => local.models);
      setLocal((p) => {
        const next = { ...p.downloading };
        delete next[modelId];
        return { models, downloading: next, error: String(e), flash: null };
      });
    }
  }

  async function deleteModel(modelId: string) {
    const before = local.models.find((m) => m.id === modelId);
    try {
      await ipc.localWhisperDelete(modelId);
      const models = await ipc.localWhisperModels();
      setLocal((p) => ({ ...p, models, error: null, flash: null }));
      flashLocal({
        kind: "info",
        message: before ? `Deleted ${before.label}` : "Whisper model deleted",
      });
      // If the deleted model was the default's model_id, fall back to
      // the first still-downloaded multilingual (or the registry default
      // if none). Language-specific entries aren't candidates here —
      // they're picked via per-language overrides.
      if (
        transcribeConfig.default.provider === "local" &&
        transcribeConfig.default.model_id === modelId
      ) {
        const fallback =
          models.find((m) => m.kind === "multilingual" && m.downloaded)?.id ??
          "large-v3-turbo-q5";
        await setDefaultConfig({
          ...transcribeConfig.default,
          model_id: fallback,
        });
      }
    } catch (e) {
      setLocal((p) => ({ ...p, error: String(e) }));
    }
  }

  async function downloadDiarize() {
    setDiarize({
      status: null,
      downloading: true,
      fraction: 0,
      phase: null,
      error: null,
      flash: null,
    });
    try {
      await ipc.diarizeDownload("community1");
      const status = await ipc.diarizeStatus("community1");
      setDiarize({
        status,
        downloading: false,
        fraction: 0,
        phase: null,
        error: null,
        flash: null,
      });
      flashDiarize("Community-1 model downloaded");
    } catch (e) {
      const status = await ipc.diarizeStatus("community1").catch(() => null);
      setDiarize({
        status,
        downloading: false,
        fraction: 0,
        phase: null,
        error: String(e),
        flash: null,
      });
    }
  }

  async function deleteDiarize() {
    const beforePath = diarize.status?.path;
    try {
      await ipc.diarizeDelete("community1");
      const status = await ipc.diarizeStatus("community1");
      setDiarize({
        status,
        downloading: false,
        fraction: 0,
        phase: null,
        error: null,
        flash: null,
      });
      flashDiarize(
        beforePath ? `Deleted ${beforePath}` : "Community-1 model deleted",
      );
    } catch (e) {
      setDiarize((p) => ({ ...p, error: String(e) }));
    }
  }

  async function downloadSortformer() {
    setSortformer({
      status: null,
      downloading: true,
      fraction: 0,
      phase: null,
      error: null,
      flash: null,
    });
    try {
      await ipc.diarizeDownload("sortformer");
      const status = await ipc.diarizeStatus("sortformer");
      setSortformer({
        status,
        downloading: false,
        fraction: 0,
        phase: null,
        error: null,
        flash: null,
      });
      flashSortformer("Sortformer model downloaded");
    } catch (e) {
      const status = await ipc.diarizeStatus("sortformer").catch(() => null);
      setSortformer({
        status,
        downloading: false,
        fraction: 0,
        phase: null,
        error: String(e),
        flash: null,
      });
    }
  }

  async function deleteSortformer() {
    const beforePath = sortformer.status?.path;
    try {
      await ipc.diarizeDelete("sortformer");
      const status = await ipc.diarizeStatus("sortformer");
      setSortformer({
        status,
        downloading: false,
        fraction: 0,
        phase: null,
        error: null,
        flash: null,
      });
      flashSortformer(
        beforePath ? `Deleted ${beforePath}` : "Sortformer model deleted",
      );
    } catch (e) {
      setSortformer((p) => ({ ...p, error: String(e) }));
    }
  }

  async function update(key: EditableKey, value: string) {
    setS((prev) => ({ ...prev, [key]: value }));
    await ipc.setSetting(key, value);
    // Tell views outside this dialog (see settingsBus for why they can't
    // notice on their own).
    broadcastSettingChange(key, value);
  }

  async function updateTranscribeConfig(cfg: TranscribeConfig) {
    setTranscribeConfig(cfg);
    try {
      await ipc.setTranscribeConfig(cfg);
    } catch (e) {
      console.warn("[settings] setTranscribeConfig failed:", e);
    }
  }

  // Convenience for the Default provider section. Only mutates the
  // `default` slot; per-language overrides untouched.
  async function setDefaultConfig(cfg: ProviderConfig) {
    await updateTranscribeConfig({ ...transcribeConfig, default: cfg });
  }

  // Add or replace a per-language override.
  async function setLanguageOverride(language: string, cfg: ProviderConfig) {
    await updateTranscribeConfig({
      ...transcribeConfig,
      per_language: { ...transcribeConfig.per_language, [language]: cfg },
    });
  }

  // Remove a per-language override entirely. No-op if the language
  // isn't currently mapped.
  async function removeLanguageOverride(language: string) {
    if (!(language in transcribeConfig.per_language)) return;
    const next = { ...transcribeConfig.per_language };
    delete next[language];
    await updateTranscribeConfig({ ...transcribeConfig, per_language: next });
  }

  // Provider API keys moved out of this hook entirely: ProviderKeyCard
  // (src/components/provider/) is self-contained against the keychain
  // commands, so nothing here loads or mutates key state anymore.

  return {
    s,
    update,
    transcribeConfig,
    updateTranscribeConfig,
    setDefaultConfig,
    setLanguageOverride,
    removeLanguageOverride,
    local,
    downloadModel,
    deleteModel,
    diarize,
    downloadDiarize,
    deleteDiarize,
    sortformer,
    downloadSortformer,
    deleteSortformer,
  };
}

export type SettingsHook = ReturnType<typeof useSettings>;
