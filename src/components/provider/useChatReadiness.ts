import { useEffect, useState } from "react";
import { ipc } from "../../lib/ipc";
import { DEFAULTS } from "../../pages/settings/types";
import { isEmbeddingModel, isOllamaUrl } from "../../lib/localModels";
import { useOllamaProbe } from "./useOllamaProbe";
import { useProviderKey } from "./useProviderKey";

/** What still stands between a local chat provider and a working chat, or `""`
 *  when nothing does. Shared by the Settings tab and the Note pane so the two
 *  can't answer the same question differently; `where` is the only thing that
 *  varies, since one reader has the controls in front of them and the other
 *  does not.
 *
 *  Off Ollama's port the runtime is LM Studio, llama-server, vLLM or mlx, so
 *  the advice names the server the user actually runs (#179) — `ollama pull` is
 *  not a command they have. */
export function localChatHint({
  reachable,
  installed,
  model,
  baseUrl,
  where,
}: {
  reachable: boolean | null;
  installed: string[] | null;
  model: string;
  baseUrl: string;
  where: "above" | "settings";
}): string {
  const at = where === "above" ? "above" : "in Settings → Chat";
  if (reachable === false)
    return isOllamaUrl(baseUrl)
      ? "Start or install Ollama — it's detected automatically."
      : `Couldn't reach the local server at ${baseUrl} — start it, or check the URL ${at}.`;
  if (!model) return `Choose a chat model ${at}.`;
  if (isEmbeddingModel(model)) return `“${model}” is an embedding model — pick a chat model ${at}.`;
  if (installed && !installed.includes(model))
    return isOllamaUrl(baseUrl)
      ? `“${model}” isn't installed on the server — run ollama pull ${model}.`
      : `“${model}” isn't one of the models the local server lists — pick another ${at}.`;
  if (reachable === null) return "Checking the local server…";
  return "";
}

// Chat readiness for the Note's Chat tab (issue #44): what's still missing
// before a chat can run, so the panel shows a setup prompt instead of a dead
// input. Settings are read once on mount — they only change from the Settings
// dialog, which isn't open while chatting. Both provider hooks run
// unconditionally (rules of hooks); the local probe parks itself on cloud chat.
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
    hint = localChatHint({ reachable, installed, model, baseUrl, where: "settings" });
    ready = hint === "";
  } else {
    if (!key.hasKey) hint = "Add your OpenAI key in Settings → Chat to use chat.";
    else ready = true;
  }

  return { loading, ready, hint, provider, model, baseUrl };
}
