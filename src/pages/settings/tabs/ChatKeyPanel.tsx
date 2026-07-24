import { useCallback, useEffect, useState } from "react";
import { AlertTriangle } from "lucide-react";
import { cloudApi, useCloudStore, type ChatKeyMeta, type CloudWorkspace } from "../../../lib/cloud";
import { ipc, type ChatUsage } from "../../../lib/ipc";
import { Btn } from "../components/Btn";

// Workspace chat activation (BYOK, issue #75). The body of the settings "Chat"
// section. Owner: masked key entry → server test-on-save → configured state
// (last-4, who/when) with Rotate + Remove (inline confirm — the Tauri webview
// blocks window.confirm). Member: read-only state, no entry, no actions. The key
// value is never persisted client-side — it lives only in the input until save,
// then is cleared; only metadata comes back.

const inputCls =
  "flex-1 min-w-0 text-sm px-3 py-2 rounded-md border border-[var(--color-line-visible)] bg-[var(--color-surface)] focus:border-[var(--color-text-muted)] transition-colors";

// Best-effort "when set" — the server may send an RFC3339 string or an epoch
// number. Returns "" when it can't be parsed (the caller then omits the date).
function whenSet(raw: string | null): string {
  if (!raw) return "";
  const d = /^\d+$/.test(raw.trim())
    ? new Date(Number(raw) < 1e12 ? Number(raw) * 1000 : Number(raw))
    : new Date(raw);
  return isNaN(d.getTime()) ? "" : d.toLocaleDateString();
}

export function ChatKeyPanel({ ws }: { ws: CloudWorkspace }) {
  const members = useCloudStore((s) => s.members);
  const isOwner = ws.role === "owner";

  const [meta, setMeta] = useState<ChatKeyMeta | null>(null);
  const [usage, setUsage] = useState<ChatUsage | null>(null);
  const [loading, setLoading] = useState(true);
  const [keyDraft, setKeyDraft] = useState("");
  const [rotating, setRotating] = useState(false);
  const [confirmRemove, setConfirmRemove] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  // `cancelled` guards a fast A→B workspace switch: if A's fetch resolves after
  // B's effect ran, A's cleanup has flipped the flag and we drop A's writes so
  // B's panel never shows A's metadata (same discipline as ChatPanel).
  const load = useCallback(
    async (cancelled: () => boolean) => {
      setLoading(true);
      setError(null);
      try {
        // Usage tells BYOK (unmetered → null) apart from the managed add-on
        // (metered → numbers); the meta drives the BYOK state itself.
        const [m, u] = await Promise.all([
          cloudApi.chatKeyMeta(ws.id),
          ipc.chatUsage().catch(() => null),
        ]);
        if (cancelled()) return;
        setMeta(m);
        setUsage(u);
      } catch (e) {
        if (cancelled()) return;
        setError(String(e));
        setMeta({ configured: false, last4: null, setBy: null, setAt: null, keyHealth: null });
      } finally {
        if (!cancelled()) setLoading(false);
      }
    },
    [ws.id],
  );

  useEffect(() => {
    let cancelled = false;
    setKeyDraft("");
    setRotating(false);
    setConfirmRemove(false);
    setNotice(null);
    void load(() => cancelled);
    return () => {
      cancelled = true;
    };
  }, [load]);

  const ownerMember = Object.values(members).find((m) => m.role === "owner");
  const ownerName = ownerMember ? ownerMember.name || ownerMember.email : "the workspace owner";
  const setter = meta?.setBy ? members[meta.setBy] : undefined;
  const setByName = setter ? setter.name || setter.email : ownerName;

  async function save() {
    const key = keyDraft.trim();
    if (!key) return;
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const m = await cloudApi.chatKeySet(ws.id, key);
      // Clear the key from memory the moment it's saved — never retained.
      setKeyDraft("");
      setRotating(false);
      setMeta(m);
      setUsage(null); // BYOK is unmetered
      setNotice("Key saved and verified.");
    } catch (e) {
      // The Rust layer maps reason codes to a short message; show it verbatim.
      // (Never contains the key.) Keep the draft so the owner can correct it.
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function remove() {
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const m = await cloudApi.chatKeyRemove(ws.id);
      setMeta(m);
      setConfirmRemove(false);
      // Removing the key may fall back to the managed add-on (if active) — reload
      // usage so the state reflects it.
      setUsage(await ipc.chatUsage().catch(() => null));
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  if (loading || !meta) {
    return <div className="py-3.5 text-sm text-[var(--color-text-muted)]">Loading…</div>;
  }

  const degraded = meta.configured && meta.keyHealth != null && meta.keyHealth !== "ok";

  // Masked entry form (owner only), shared by first-time activation and rotate.
  const entry = (
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
        {rotating && (
          <Btn onClick={() => { setRotating(false); setKeyDraft(""); setError(null); }} disabled={busy}>
            Cancel
          </Btn>
        )}
      </div>
      <p className="text-xs text-[var(--color-text-muted)] leading-relaxed">
        Your key is sent straight to the server, tested against OpenAI, and stored encrypted — it's
        never kept on this device. Chat then runs on your OpenAI account, free and unmetered.
      </p>
    </div>
  );

  return (
    <div className="flex flex-col gap-3 py-3.5">
      {meta.configured ? (
        <>
          <p className="text-sm">
            {isOwner
              ? "Chat runs on this workspace's OpenAI key."
              : `Chat runs on ${setByName}'s workspace key.`}
          </p>
          <p className="text-xs text-[var(--color-text-muted)]">
            Key ending {meta.last4 ?? "••••"}
            {isOwner && (
              <>
                {" · "}Set by {setByName}
                {whenSet(meta.setAt) && ` on ${whenSet(meta.setAt)}`}
              </>
            )}
          </p>

          {degraded && (
            <div className="flex items-start gap-2 rounded-md bg-[var(--color-pill-hover)] px-3 py-2 text-xs text-[var(--color-status-warning)]">
              <AlertTriangle size={13} strokeWidth={1.7} className="mt-px shrink-0" aria-hidden="true" />
              <span>
                Workspace key failing — semantic search degraded.{" "}
                {isOwner ? "Re-enter the key to fix it." : `Ask ${ownerName} to re-enter it.`}
              </span>
            </div>
          )}

          {isOwner &&
            (rotating ? (
              entry
            ) : (
              <div className="flex flex-wrap items-center gap-2">
                <Btn onClick={() => { setRotating(true); setNotice(null); }} disabled={busy}>
                  Rotate key
                </Btn>
                {!confirmRemove ? (
                  <Btn onClick={() => setConfirmRemove(true)} disabled={busy}>
                    Remove
                  </Btn>
                ) : (
                  <>
                    <span className="text-xs text-[var(--color-text-muted)]">Remove the workspace key?</span>
                    <button
                      onClick={remove}
                      disabled={busy}
                      className="px-3 py-2 rounded-md text-sm border border-[var(--color-danger)] disabled:opacity-50 transition-opacity hover:opacity-90"
                      style={{ background: "var(--color-danger)", color: "#fff" }}
                    >
                      Remove key
                    </button>
                    <Btn onClick={() => setConfirmRemove(false)} disabled={busy}>
                      Cancel
                    </Btn>
                  </>
                )}
              </div>
            ))}
        </>
      ) : usage ? (
        <>
          <p className="text-sm">Chat runs on Humla's managed key.</p>
          <p className="text-xs text-[var(--color-text-muted)]">
            {usage.used}/{usage.cap} turns this period.
          </p>
          {isOwner ? (
            <>
              <p className="text-xs text-[var(--color-text-muted)] leading-relaxed">
                Use your own OpenAI key instead to make chat free and unmetered.
              </p>
              {entry}
            </>
          ) : null}
        </>
      ) : (
        <>
          <p className="text-sm">
            {isOwner ? "Chat isn't activated for this workspace yet." : "Chat isn't activated yet."}
          </p>
          {isOwner ? (
            <>
              {entry}
              <p className="text-xs text-[var(--color-text-muted)] leading-relaxed">
                Prefer not to manage a key? The managed add-on runs chat on Humla's key with a
                per-period turn allowance.
              </p>
            </>
          ) : (
            <p className="text-xs text-[var(--color-text-muted)]">Ask {ownerName} to turn it on.</p>
          )}
        </>
      )}

      {notice && <p className="text-xs text-[var(--color-success)]">{notice}</p>}
      {error && <p className="text-xs text-[var(--color-danger)] break-words">{error}</p>}
    </div>
  );
}
