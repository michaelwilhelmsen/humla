import { useEffect, useState } from "react";
import { ipc } from "../../lib/ipc";
import { DEFAULTS } from "../../pages/settings/types";
import { isEmbeddingModel } from "../../lib/localModels";
import { useOllamaProbe } from "./useOllamaProbe";
import { useProviderKey } from "./useProviderKey";

// Chat readiness for the Note's Chat tab. Mirrors the Settings → Chat tab's
// readiness (issue #44): reports exactly what's still missing before a chat can
// run, so the panel can show the same setup prompt instead of a dead input.
//
// Settings are read once on mount (they only change from the Settings dialog,
// which isn't open while chatting). Both provider hooks run unconditionally
// (rules of hooks); the Ollama probe parks itself when chat isn't on Ollama.
export function useChatReadiness() {
  const [loading, setLoading] = useState(true);
  const [provider, setProvider] = useState(DEFAULTS.chat_provider);
  const [model, setModel] = useState(DEFAULTS.chat_model);
  const [baseUrl, setBaseUrl] = useState(DEFAULTS.local_llm_base_url);

  useEffect(() => {
    let cancelled = false;
    Promise.all([
      ipc.getSetting("chat_provider"),
      ipc.getSetting("chat_model"),
      ipc.getSetting("local_llm_base_url"),
    ])
      .then(([p, m, b]) => {
        if (cancelled) return;
        setProvider(p || DEFAULTS.chat_provider);
        setModel(m || DEFAULTS.chat_model);
        setBaseUrl(b || DEFAULTS.local_llm_base_url);
        setLoading(false);
      })
      .catch(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const isOllama = provider === "ollama";
  const key = useProviderKey("openai");
  const { reachable, installed } = useOllamaProbe(baseUrl, { enabled: isOllama });

  let ready = false;
  let hint = "";
  if (loading) {
    hint = "";
  } else if (isOllama) {
    if (reachable === false) hint = "Start or install Ollama — it's detected automatically.";
    else if (!model) hint = "Choose a chat model in Settings → Chat.";
    else if (isEmbeddingModel(model))
      hint = `“${model}” is an embedding model — pick a chat model in Settings → Chat.`;
    else if (installed && !installed.includes(model))
      hint = `“${model}” isn't installed on the server — run ollama pull ${model}.`;
    else if (reachable === null) hint = "Checking the local server…";
    else ready = true;
  } else {
    if (!key.hasKey) hint = "Add your OpenAI key in Settings → Chat to use chat.";
    else ready = true;
  }

  return { loading, ready, hint, provider, model };
}
