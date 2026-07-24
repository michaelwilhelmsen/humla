import { useEffect, useState } from "react";
import { cloudApi, type ChatKeyMeta } from "../lib/cloud";
import { ipc } from "../lib/ipc";
import { Btn } from "../pages/settings/components/Btn";

// Shared masked key-entry for workspace BYOK activation (issues #75/#76). Used
// by both the workspace-settings panel (ChatKeyPanel) and the chat activation
// pane, so the test-on-save flow + security posture live in one place: the key
// is `type="password"`, sent straight to the server hook, cleared the instant
// it's saved, and never persisted client-side. The "use the key from Settings"
// shortcut reads the personal Keychain key entirely in Rust — it never enters
// the webview. On success the parent gets the fresh metadata via `onActivated`.

const inputCls =
  "flex-1 min-w-0 text-sm px-3 py-2 rounded-md border border-[var(--color-line-visible)] bg-[var(--color-surface)] focus:border-[var(--color-text-muted)] transition-colors";

export function ChatKeyEntry({
  workspaceId,
  onActivated,
  onCancel,
}: {
  workspaceId: string;
  onActivated: (meta: ChatKeyMeta) => void;
  /** When provided, shows a Cancel button (the rotate flow in settings). */
  onCancel?: () => void;
}) {
  const [keyDraft, setKeyDraft] = useState("");
  // Whether a personal OpenAI key is stored (Settings → Providers) — gates the
  // shortcut. The key itself is read Rust-side; only this boolean is here.
  const [hasPersonalKey, setHasPersonalKey] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    ipc
      .getProviderKey("openai")
      .then((k) => {
        if (!cancelled) setHasPersonalKey(k != null);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, []);

  async function save() {
    const key = keyDraft.trim();
    if (!key) return;
    setBusy(true);
    setError(null);
    try {
      const m = await cloudApi.chatKeySet(workspaceId, key);
      // Clear the key from memory the moment it's saved — never retained.
      setKeyDraft("");
      onActivated(m);
    } catch (e) {
      // The Rust layer maps reason codes to a short message (never the key).
      // Keep the draft so the owner can correct it.
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function useKeychain() {
    setBusy(true);
    setError(null);
    try {
      const m = await cloudApi.chatKeySetFromKeychain(workspaceId);
      setKeyDraft("");
      onActivated(m);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex flex-col gap-2">
      <div className="flex gap-2">
        <input
          type="password"
          className={inputCls}
          value={keyDraft}
          onChange={(e) => setKeyDraft(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && save()}
          placeholder="sk-…"
          aria-label="OpenAI API key"
          autoComplete="off"
        />
        <Btn onClick={save} disabled={busy || !keyDraft.trim()}>
          {busy ? "Saving…" : "Save"}
        </Btn>
        {onCancel && (
          <Btn
            onClick={() => {
              setKeyDraft("");
              setError(null);
              onCancel();
            }}
            disabled={busy}
          >
            Cancel
          </Btn>
        )}
      </div>
      <p className="text-xs text-[var(--color-text-muted)] leading-relaxed">
        Your key is sent straight to the server, tested against OpenAI, and stored encrypted — it's
        never kept on this device. Chat then runs on your OpenAI account, free and unmetered.
      </p>
      {hasPersonalKey && (
        <div className="flex flex-col gap-1 pt-1">
          <Btn onClick={useKeychain} disabled={busy}>
            {busy ? "Saving…" : "Use the OpenAI key from Settings"}
          </Btn>
          <p className="text-xs text-[var(--color-text-muted)] leading-relaxed">
            Shares the key from Settings → Providers with this workspace. Everyone's chat then runs
            on your OpenAI account.
          </p>
        </div>
      )}
      {error && <p className="text-xs text-[var(--color-danger)] break-words">{error}</p>}
    </div>
  );
}
