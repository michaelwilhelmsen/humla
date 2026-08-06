import { useEffect, useState } from "react";
import { Permissions } from "../../../components/Permissions";
import { ipc, type StoredAudioStats } from "../../../lib/ipc";
import { formatBytes, s as plural } from "../components/format";
import { Row, Section } from "../components/Section";
import { Toggle } from "../components/Toggle";
import type { SettingsHook } from "../useSettings";

// Recording section: capture permissions + audio retention. Menu-bar mode
// and the global hotkey land here when that feature ships.
export function RecordingSection({
  s,
  update,
}: Pick<SettingsHook, "s" | "update">) {
  const keeping = s.keep_audio === "true";
  return (
    <>
      <Section title="Permissions">
        <div className="py-3.5">
          <Permissions />
        </div>
      </Section>

      <Section title="Audio retention">
        <Row
          label="Keep recorded audio"
          // The copy states which regime is in force rather than describing
          // the knob (#24). Before that issue this setting was a half-truth:
          // the mixed playback.wav was written either way, so "off keeps
          // nothing" was false. It's now literal, which is also why the off
          // text names the cost — no playback, no re-detection.
          description={
            keeping
              ? "Keep recordings for playback and speaker re-detection. Roughly 1 MB per minute per channel."
              : "No audio is stored on this Mac — transcript only; playback and speaker re-detection unavailable."
          }
          control={
            <Toggle
              label="Keep recorded audio"
              checked={keeping}
              onChange={(on) => update("keep_audio", on ? "true" : "false")}
            />
          }
        />
        <StoredAudioCleanup />
      </Section>
    </>
  );
}

// Turning the toggle off is going-forward only — existing notes keep their
// audio, since silently deleting a user's recordings on a settings change
// would be worse than the dishonesty #24 fixes. This is the explicit action
// for what's already on disk.
//
// Two-step inline confirm: Tauri's webview no-ops window.confirm (it would
// deadlock the main thread), and the deletion is irreversible, so the armed
// state spells out the count.
function StoredAudioCleanup() {
  const [stats, setStats] = useState<StoredAudioStats | null>(null);
  const [armed, setArmed] = useState(false);
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    ipc.storedAudioStats()
      .then((next) => {
        if (!cancelled) setStats(next);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, []);

  async function remove() {
    setBusy(true);
    setError(null);
    try {
      await ipc.deleteStoredAudio();
      setDone(true);
      setArmed(false);
      setStats(await ipc.storedAudioStats().catch(() => null));
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  // Nothing stored → no dangling affordance, except right after a sweep where
  // the confirmation is the whole point of having clicked.
  if (!stats || stats.notes === 0) {
    if (!done) return null;
    return (
      <Row label="Stored audio">
        <p className="text-xs text-[var(--color-text-muted)]">
          No audio stored — transcripts and speaker labels are untouched.
        </p>
      </Row>
    );
  }

  return (
    <Row
      label="Stored audio"
      description={`${stats.notes} note${plural(stats.notes)} · ${formatBytes(stats.bytes)}. Deleting keeps every transcript, speaker label and timeline — only the audio goes.`}
      control={
        <div className="flex flex-col items-end gap-1">
          {armed ? (
            <div className="flex items-center gap-2">
              <button
                type="button"
                className="nd-btn"
                onClick={() => setArmed(false)}
                disabled={busy}
              >
                Cancel
              </button>
              <button
                type="button"
                className="nd-btn text-[var(--color-danger)]"
                onClick={remove}
                disabled={busy}
              >
                {busy ? "Deleting…" : `Delete ${stats.files} files`}
              </button>
            </div>
          ) : (
            <button
              type="button"
              className="nd-btn"
              onClick={() => setArmed(true)}
            >
              Delete stored audio…
            </button>
          )}
          {error && (
            <p className="text-xs text-[var(--color-danger)]">{error}</p>
          )}
        </div>
      }
    />
  );
}
