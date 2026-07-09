import { useEffect, useState } from "react";
import { ipc } from "../../lib/ipc";
import { Btn } from "../../pages/settings/components/Btn";

export type KeyProvider = "openai" | "deepgram" | "groq";

// Self-contained per-provider API-key row: loads the stored-or-not state,
// saves, and tests — no state threading through the settings hook. The
// backend only ever reports a sentinel, never the key itself, so the input
// is write-only with a masked placeholder once a key exists.
//
// Shared surface: settings today, onboarding in a follow-up — keep it free
// of settings-page state dependencies.
export function ProviderKeyCard({
  provider,
  label,
  description,
  placeholder = "API key",
}: {
  provider: KeyProvider;
  label: string;
  description: string;
  placeholder?: string;
}) {
  const [hasKey, setHasKey] = useState(false);
  const [draft, setDraft] = useState("");
  const [testing, setTesting] = useState(false);
  const [result, setResult] = useState<
    { ok: true } | { ok: false; message: string } | null
  >(null);

  useEffect(() => {
    let cancelled = false;
    ipc
      .getProviderKey(provider)
      .then((v) => {
        if (!cancelled) setHasKey(v !== null);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [provider]);

  async function save() {
    const key = draft.trim();
    if (!key) return;
    await ipc.setProviderKey(provider, key);
    setDraft("");
    setHasKey(true);
    setResult(null);
  }

  async function test() {
    setTesting(true);
    try {
      const r = await ipc.testProviderKey(provider);
      setResult(
        r.ok
          ? { ok: true }
          : { ok: false, message: `${r.status}: ${r.error ?? "unknown error"}` },
      );
    } catch (e) {
      setResult({ ok: false, message: String(e) });
    } finally {
      setTesting(false);
    }
  }

  return (
    <div className="py-3.5">
      <div className="text-sm">{label}</div>
      <p className="text-xs text-[var(--color-text-muted)] mt-0.5 mb-2">
        {description}
      </p>
      <div className="flex gap-2">
        <input
          type="password"
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && save()}
          placeholder={hasKey ? "•••••••• stored" : placeholder}
          aria-label={`${label} API key`}
          className="flex-1 min-w-0 text-sm px-3 py-2 rounded-md border border-[var(--color-line-visible)] bg-[var(--color-surface)] focus:border-[var(--color-text-muted)] transition-colors"
        />
        <Btn onClick={save} disabled={!draft.trim()}>
          Save
        </Btn>
        <Btn onClick={test} disabled={!hasKey || testing}>
          {testing ? "Testing…" : "Test"}
        </Btn>
      </div>
      {result?.ok === true && (
        <p className="text-xs text-[var(--color-success)] mt-2">Connected ✓</p>
      )}
      {result?.ok === false && (
        <p className="text-xs text-[var(--color-danger)] mt-2 break-all">
          {result.message}
        </p>
      )}
    </div>
  );
}
