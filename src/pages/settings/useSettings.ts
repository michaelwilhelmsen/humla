import { useEffect, useState } from "react";
import { ipc, onDiarizeDownloadProgress, onLocalWhisperProgress } from "../../lib/ipc";
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
  const [local, setLocal] = useState<LocalState>(EMPTY_LOCAL_STATE);
  const [diarize, setDiarize] = useState<DiarizeState>(EMPTY_DIARIZE_STATE);
  const [llmModels, setLlmModels] = useState<LlmModelsState>(EMPTY_LLM_MODELS_STATE);
  const [s, setS] = useState<Record<EditableKey, string>>(DEFAULTS);

  useEffect(() => {
    (async () => {
      const [k1, models, ds] = await Promise.all([
        ipc.getApiKey(),
        ipc.localWhisperModels(),
        ipc.diarizeStatus().catch(() => null),
      ]);
      setOpenaiKey((p) => ({ ...p, hasKey: !!k1 }));
      setLocal((p) => ({ ...p, models }));
      setDiarize((p) => ({ ...p, status: ds }));
      const entries = await Promise.all(
        (Object.keys(DEFAULTS) as EditableKey[]).map(
          async (key) => [key, (await ipc.getSetting(key)) ?? DEFAULTS[key]] as const,
        ),
      );
      setS(Object.fromEntries(entries) as Record<EditableKey, string>);
    })();
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
      setDiarize((s) => ({ ...s, fraction: p.fraction, phase: p.phase }));
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
      const downloadedInfo = models.find((m) => m.id === modelId);
      if (
        downloadedInfo?.kind === "primary" &&
        models.filter((m) => m.kind === "primary" && m.downloaded).length === 1
      ) {
        await update("local_whisper_model", modelId);
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
      if (s.local_whisper_model === modelId) {
        const fallback =
          models.find((m) => m.kind === "primary" && m.downloaded)?.id ??
          DEFAULTS.local_whisper_model;
        await update("local_whisper_model", fallback);
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
      await ipc.diarizeDownload();
      const status = await ipc.diarizeStatus();
      setDiarize({
        status,
        downloading: false,
        fraction: 0,
        phase: null,
        error: null,
        flash: null,
      });
      flashDiarize("Speaker diarization model downloaded");
    } catch (e) {
      const status = await ipc.diarizeStatus().catch(() => null);
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
      await ipc.diarizeDelete();
      const status = await ipc.diarizeStatus();
      setDiarize({
        status,
        downloading: false,
        fraction: 0,
        phase: null,
        error: null,
        flash: null,
      });
      flashDiarize(
        beforePath ? `Deleted ${beforePath}` : "Speaker diarization model deleted",
      );
    } catch (e) {
      setDiarize((p) => ({ ...p, error: String(e) }));
    }
  }

  async function update(key: EditableKey, value: string) {
    setS((prev) => ({ ...prev, [key]: value }));
    await ipc.setSetting(key, value);
  }

  async function saveKey() {
    if (!openaiKey.draft.trim()) return;
    await ipc.setApiKey(openaiKey.draft.trim());
    setOpenaiKey({ draft: "", hasKey: true, testing: false, result: null });
  }

  async function testKey() {
    setOpenaiKey((p) => ({ ...p, testing: true }));
    try {
      const r = await ipc.testApiKey();
      const result = r.ok
        ? ({ ok: true } as const)
        : ({ ok: false, message: `${r.status}: ${r.error ?? "unknown error"}` } as const);
      setOpenaiKey((p) => ({ ...p, testing: false, result }));
    } catch (e) {
      setOpenaiKey((p) => ({
        ...p,
        testing: false,
        result: { ok: false, message: String(e) },
      }));
    }
  }

  return {
    s,
    update,
    openaiKey,
    setOpenaiKey,
    saveKey,
    testKey,
    local,
    downloadModel,
    deleteModel,
    diarize,
    downloadDiarize,
    deleteDiarize,
    llmModels,
    refreshLlmModels,
  };
}

export type SettingsHook = ReturnType<typeof useSettings>;
