import { useEffect, useState } from "react";
import { Permissions } from "../../../components/Permissions";
import { ipc, type StoredAudioStats } from "../../../lib/ipc";
import { formatBytes, s as plural } from "../components/format";
import { HotkeyField } from "../components/HotkeyField";
import { LaunchAtLoginField } from "../components/LaunchAtLoginField";
import { Row, Section } from "../components/Section";
import { Toggle } from "../components/Toggle";
import type { SettingsHook } from "../useSettings";
import { retainsAudio } from "../../../lib/audioGate";
import { policySource, useOrgPolicy } from "../../../lib/orgPolicy";
import { ManagedRow } from "../ManagedRow";

// Recording section: capture permissions, audio retention, and menu-bar mode
// (#21) — the triggers that start a recording from outside the window.
export function RecordingSection({
  s,
  update,
}: Pick<SettingsHook, "s" | "update">) {
  const policy = useOrgPolicy();
  const audioManaged = policySource(policy, "audio_forbidden");
  const keeping = retainsAudio(s.keep_audio, policy.audio_forbidden);
  const manualTranscription = s.transcribe_manually === "true";
  const closeToTray = s.close_to_tray === "true";
  const keepAwake = s.keep_awake !== "false";
  return (
    <>
      <Section title="Permissions">
        <Permissions />
      </Section>

      <Section title="Audio retention">
        {audioManaged ? (
          <ManagedRow
            label="Keep recorded audio"
            source={audioManaged.source}
            reason={audioManaged.reason}
            control={<Toggle label="Keep recorded audio" checked={false} onChange={() => {}} disabled />}
          />
        ) : (
          <Row
            label="Keep recorded audio"
            // The copy states which regime is in force rather than describing
            // the knob (#24). Before that issue this setting was a half-truth:
            // the mixed playback audio was written either way, so "off keeps
            // nothing" was false. It's now literal, which is also why the off
            // text names the cost — no playback, no re-detection.
            description={
              keeping
                ? "For playback and speaker re-detection. About 2 MB per minute per source."
                : "Transcript only. No playback or speaker re-detection."
            }
            control={
              <Toggle
                label="Keep recorded audio"
                checked={keeping}
                onChange={(on) => update("keep_audio", on ? "true" : "false")}
              />
            }
          />
        )}
        {/* Progressive disclosure, not a disabled control (#146): deferring
            transcription only makes sense when the audio survives the
            recording, so with retention off there is nothing here to explain
            — and a greyed-out switch would invite the user to wonder why.
            `keep_audio` also gates it in the backend
            (`sessions::defer_transcription`), so turning retention off doesn't
            silently leave a regime running that the UI no longer shows. */}
        {keeping && (
          <Row
            label="Transcribe manually"
            description="Lighter on your Mac while recording."
            control={
              <Toggle
                label="Transcribe manually"
                checked={manualTranscription}
                onChange={(on) =>
                  update("transcribe_manually", on ? "true" : "false")
                }
              />
            }
          />
        )}
        <StoredAudioCleanup />
      </Section>

      <Section title="Power">
        <Row
          label="Keep Mac awake while recording"
          description="Stops your Mac from going to sleep during a recording."
          control={
            <Toggle
              label="Keep Mac awake while recording"
              checked={keepAwake}
              onChange={(on) => update("keep_awake", on ? "true" : "false")}
            />
          }
        />
      </Section>

      <Section title="Menu bar">
        <Row
          label="Open at login"
          description="Starts in the menu bar, without a window. If Humla is in Login Items in System Settings, remove it there."
          control={<LaunchAtLoginField />}
        />
        <Row
          label="Keep running in the menu bar"
          // Off by default, so closing the window still quits until the user
          // opts in — and the copy states which of the two regimes is in force
          // rather than describing the switch.
          description={
            closeToTray
              ? "Closing the window keeps Humla in the menu bar."
              : "Closing the window quits Humla."
          }
          control={
            <Toggle
              label="Keep running in the menu bar"
              checked={closeToTray}
              onChange={(on) => update("close_to_tray", on ? "true" : "false")}
            />
          }
        />
        <Row
          label="Record shortcut"
          description="Start and stop a recording from any app."
          control={<HotkeyField />}
        />
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
          No audio stored.
        </p>
      </Row>
    );
  }

  return (
    <Row
      label="Stored audio"
      description={`${stats.notes} note${plural(stats.notes)} · ${formatBytes(stats.bytes)}. Transcripts stay.`}
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
              {/* Inline style, not a text-* utility: `.nd-btn` is unlayered, so
                  it wins the cascade over Tailwind's layered utilities and the
                  destructive red silently doesn't render. Same pattern as the
                  Sidebar's delete confirm. */}
              <button
                type="button"
                className="nd-btn"
                style={{ color: "var(--color-danger)" }}
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
