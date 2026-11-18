import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { ipc } from "../lib/ipc";
import type { AudioSweep } from "../lib/orgPolicy";
import { Modal } from "../pages/settings/components/Modal";

// The one-time notice for an organisation's `audio_forbidden`. The audio gate
// is closed from the moment the policy arrives; this deletes what is already
// stored. It returns at every launch until its button is pressed.
export function OrgAudioNotice() {
  const [due, setDue] = useState<AudioSweep | null>(null);
  const [dismissed, setDismissed] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    const load = () =>
      ipc.orgAudioNotice().then(
        (d) => !cancelled && setDue(d ?? null),
        () => !cancelled && setDue(null),
      );
    void load();
    let unlisten: (() => void) | undefined;
    void listen("org_policy_changed", () => void load()).then((u) => {
      if (cancelled) u();
      else unlisten = u;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  async function sweep() {
    if (!due) return;
    setBusy(true);
    setError(null);
    try {
      await ipc.orgAudioSweep(due.org_id);
      setDue(await ipc.orgAudioNotice().catch(() => null));
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Modal
      open={due !== null && !dismissed}
      onClose={() => setDismissed(true)}
      title="Stored audio"
      showHeader={false}
      size="sm"
    >
      {due && (
        <div className="flex flex-col gap-4">
          <p className="text-sm leading-relaxed">
            {due.org_name} doesn't allow stored audio.{" "}
            {due.all ? "Audio on this Mac will be deleted." : "Audio of its notes on this Mac will be deleted."}
          </p>
          {error && <p className="text-xs text-[var(--color-danger)]">{error}</p>}
          <div className="flex justify-end">
            <button type="button" className="nd-btn nd-btn-primary" onClick={() => void sweep()} disabled={busy}>
              {busy ? "Deleting…" : "Delete audio"}
            </button>
          </div>
        </div>
      )}
    </Modal>
  );
}
