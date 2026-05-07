import { useEffect, useState } from "react";
import {
  ipc,
  onDiarizeDownloadProgress,
  onLocalWhisperProgress,
  type ProviderConfig,
  type TranscribeProvider,
} from "../../lib/ipc";
import {
  DEFAULTS,
  EMPTY_DIARIZE_STATE,
  EMPTY_KEY_STATE,
  EMPTY_LLM_MODELS_STATE,
  EMPTY_LOCAL_STATE,
  type DiarizeState,
  type EditableKey,
  type KeyState,
  type LlmModelsState,
  type LocalState,
} from "./types";

// One hook to own every piece of Settings page state plus the handlers
// the tabs need. Pulled out of the page component so individual tabs
// can grab only the slices they care about, and so the page renders
// stay focused on layout.
export function useSettings() {
  const [openaiKey, setOpenaiKey] = useState<KeyState>(EMPTY_KEY_STATE);
  const [deepgramKey, setDeepgramKey] = useState<KeyState>(EMPTY_KEY_STATE);
  const [groqKey, setGroqKey] = useState<KeyState>(EMPTY_KEY_STATE);
  const [local, setLocal] = useState<LocalState>(EMPTY_LOCAL_STATE);
  const [diarize, setDiarize] = useState<DiarizeState>(EMPTY_DIARIZE_STATE);
  // Parallel state for the Sortformer engine. Tracked independently of
  // community1 so each can be downloaded / deleted on its own. The active
  // engine is decided by the `diarize_model` setting; the manager UI
  // shows both rows so users can have one downloaded but the other active
  // while they decide.
  const [sortformer, setSortformer] = useState<DiarizeState>(EMPTY_DIARIZE_STATE);
  const [llmModels, setLlmModels] = useState<LlmModelsState>(EMPTY_LLM_MODELS_STATE);
  const [s, setS] = useState<Record<EditableKey, string>>(DEFAULTS);
  const [providerConfig, setProviderConfig] = useState<ProviderConfig>({
    provider: "openai",
    model: "whisper-1",
  });

  useEffect(() => {
    let cancelled = false;
    (async () => {
      const [k1, kdg, kgrq, models, ds, ss, cfg] = await Promise.all([
        ipc.getProviderKey("openai").catch(() => null),
        ipc.getProviderKey("deepgram").catch(() => null),
        ipc.getProviderKey("groq").catch(() => null),
        ipc.localWhisperModels(),
        ipc.diarizeStatus("community1").catch(() => null),
        ipc.diarizeStatus("sortformer").catch(() => null),
        ipc.getProviderConfig().catch(() => null),
      ]);
      if (cancelled) return;
      setOpenaiKey((p) => ({ ...p, hasKey: !!k1 }));
      setDeepgramKey((p) => ({ ...p, hasKey: !!kdg }));
      setGroqKey((p) => ({ ...p, hasKey: !!kgrq }));
      setLocal((p) => ({ ...p, models }));
      setDiarize((p) => ({ ...p, status: ds }));
      setSortformer((p) => ({ ...p, status: ss }));
      if (cfg) setProviderConfig(cfg);
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
    onLocalWhisperProgress((p) => {
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

  // Generic flash helper — schedules a 4s clear that only fires if the same
  // message is still showing (so a fresh action doesn't get its toast wiped
  // by a stale timer).
  function flashLocal(msg: string) {
    setLocal((p) => ({ ...p, flash: msg }));
    window.setTimeout(() => {
      setLocal((p) => (p.flash === msg ? { ...p, flash: null } : p));
    }, 4000);
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

  // Hit the user-configured local server's /v1/models endpoint and populate
  // the model dropdown. Triggered by the Refresh button + automatically when
  // the user first picks Local provider.
  async function refreshLlmModels(baseUrl: string) {
    setLlmModels({ list: null, loading: true, error: null });
    try {
      const list = await ipc.localLlmListModels(baseUrl);
      list.sort();
      setLlmModels({ list, loading: false, error: null });
      // Auto-pick the first model when (a) the user hasn't picked anything
      // yet, or (b) the previously-saved choice is no longer on the server
      // (they ran `ollama rm` between sessions). Without this, the <select>
      // shows the first option due to HTML's default-fallback rendering but
      // s.local_llm_model stays empty — summary calls fail with "model not
      // configured" even though the dropdown looks fine.
      if (
        list.length > 0 &&
        (!s.local_llm_model || !list.includes(s.local_llm_model))
      ) {
        await update("local_llm_model", list[0]);
      }
    } catch (e) {
      // reqwest's connection-refused error shows up as "error sending request
      // for url (...)" which is opaque to non-technical users. Replace it
      // with a clearer prompt that names the likely cause and the fix.
      const raw = String(e);
      const friendly = /error sending request|connection refused|failed to connect/i.test(raw)
        ? `Couldn't reach the server at ${baseUrl}. Is your local-LLM tool running?`
        : raw;
      setLlmModels({ list: null, loading: false, error: friendly });
    }
  }

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
      // First downloaded primary auto-becomes active. Addons never become
      // the active primary — they auto-apply via language match instead.
      // Only fires when the user is on the local provider; otherwise
      // we don't silently switch them.
      const downloadedInfo = models.find((m) => m.id === modelId);
      if (
        downloadedInfo?.kind === "multilingual" &&
        models.filter((m) => m.kind === "multilingual" && m.downloaded).length === 1 &&
        providerConfig.provider === "local"
      ) {
        await updateProviderConfig({ ...providerConfig, model_id: modelId });
      }
      const label = models.find((m) => m.id === modelId)?.label ?? modelId;
      flashLocal(`${label} downloaded`);
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
      flashLocal(before ? `Deleted ${before.label}` : "Whisper model deleted");
      // If the deleted model was the active primary, fall back to the
      // first still-downloaded primary (or the registry default if none).
      // Addons aren't candidates — they're auto-applied, not user-active.
      if (
        providerConfig.provider === "local" &&
        providerConfig.model_id === modelId
      ) {
        const fallback =
          models.find((m) => m.kind === "multilingual" && m.downloaded)?.id ??
          "large-v3-turbo-q5";
        await updateProviderConfig({ ...providerConfig, model_id: fallback });
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
  }

  async function updateProviderConfig(cfg: ProviderConfig) {
    setProviderConfig(cfg);
    try {
      await ipc.setProviderConfig(cfg);
    } catch (e) {
      console.warn("[settings] setProviderConfig failed:", e);
    }
  }

  async function saveProviderKey(provider: TranscribeProvider) {
    const slot =
      provider === "openai" ? openaiKey
      : provider === "deepgram" ? deepgramKey
      : provider === "groq" ? groqKey
      : null;
    const setter =
      provider === "openai" ? setOpenaiKey
      : provider === "deepgram" ? setDeepgramKey
      : provider === "groq" ? setGroqKey
      : null;
    if (!slot || !setter || !slot.draft.trim()) return;
    await ipc.setProviderKey(provider, slot.draft.trim());
    setter({ draft: "", hasKey: true, testing: false, result: null });
  }

  async function testProviderKey(provider: TranscribeProvider) {
    const setter =
      provider === "openai" ? setOpenaiKey
      : provider === "deepgram" ? setDeepgramKey
      : provider === "groq" ? setGroqKey
      : null;
    if (!setter) return;
    setter((p) => ({ ...p, testing: true }));
    try {
      const r = await ipc.testProviderKey(provider);
      const result = r.ok
        ? ({ ok: true } as const)
        : ({ ok: false, message: `${r.status}: ${r.error ?? "unknown error"}` } as const);
      setter((p) => ({ ...p, testing: false, result }));
    } catch (e) {
      setter((p) => ({
        ...p,
        testing: false,
        result: { ok: false, message: String(e) },
      }));
    }
  }

  return {
    s,
    update,
    providerConfig,
    updateProviderConfig,
    openaiKey,
    setOpenaiKey,
    deepgramKey,
    setDeepgramKey,
    groqKey,
    setGroqKey,
    saveProviderKey,
    testProviderKey,
    local,
    downloadModel,
    deleteModel,
    diarize,
    downloadDiarize,
    deleteDiarize,
    sortformer,
    downloadSortformer,
    deleteSortformer,
    llmModels,
    refreshLlmModels,
  };
}

export type SettingsHook = ReturnType<typeof useSettings>;
