import { useEffect, useState } from "react";
import { ipc } from "../../../lib/ipc";
import {
  DEFAULT_RECORD_HOTKEY,
  accelFromEvent,
  formatAccel,
  isModifierOnly,
} from "../../../lib/hotkey";

// Recorder for the global record shortcut (#21).
//
// Self-contained rather than driven by the settings map: this is the one
// control whose value has to be accepted by the OS before it can be stored, so
// it goes through `recordHotkeySet` (register, then persist) and has to be able
// to put the old value back when the combination is already taken. A row that
// showed a shortcut nothing had registered would be the worst outcome here —
// the user would press it and nothing would happen.
export function HotkeyField() {
  // `null` while loading, so the row doesn't flash "None" on the way in.
  const [accel, setAccel] = useState<string | null>(null);
  const [capturing, setCapturing] = useState(false);
  const [hint, setHint] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    ipc
      .recordHotkeyGet()
      .then((v) => {
        if (!cancelled) setAccel(v);
      })
      .catch(() => {
        if (!cancelled) setAccel("");
      });
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    if (!capturing) return;
    async function commit(next: string) {
      const previous = accel;
      setCapturing(false);
      setHint(null);
      setError(null);
      setAccel(next);
      try {
        await ipc.recordHotkeySet(next);
      } catch (e) {
        // The backend registers before it persists, so a rejection means the
        // *old* shortcut is still the live one. Show that, not the attempt.
        setAccel(previous);
        setError(String(e));
      }
    }
    function onKey(e: KeyboardEvent) {
      // Capture phase + stopPropagation: while armed, this row swallows the
      // keyboard so the app's own ⌘-shortcuts (and Escape-closes-the-dialog)
      // can't fire on the combination being recorded.
      e.preventDefault();
      e.stopPropagation();
      if (e.key === "Escape") {
        setCapturing(false);
        setHint(null);
        return;
      }
      const next = accelFromEvent(e);
      if (next) {
        void commit(next);
        return;
      }
      // A modifier on its own is the user mid-press — stay armed and quiet.
      if (isModifierOnly(e.code)) return;
      setHint(
        "A global shortcut needs ⌘, ⌃ or ⌥ — otherwise it would swallow that key in every app.",
      );
    }
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [capturing, accel]);

  async function apply(next: string) {
    const previous = accel;
    setError(null);
    setAccel(next);
    try {
      await ipc.recordHotkeySet(next);
    } catch (e) {
      setAccel(previous);
      setError(String(e));
    }
  }

  if (accel === null) return null;

  return (
    <div className="flex flex-col items-end gap-1">
      <div className="flex items-center gap-2">
        <button
          type="button"
          // Fixed width so arming the field doesn't shift the row: "Press a
          // shortcut…" is wider than any glyph combination it replaces. Reads
          // as a shortcut field rather than a button, which is what it is.
          className="nd-btn min-w-[134px] justify-center"
          onClick={() => {
            setCapturing(true);
            setHint(null);
            setError(null);
          }}
        >
          {capturing ? (
            <span className="text-[var(--color-text-muted)]">Press a shortcut…</span>
          ) : (
            formatAccel(accel)
          )}
        </button>
        {accel ? (
          <button type="button" className="nd-btn" onClick={() => void apply("")}>
            Turn off
          </button>
        ) : (
          <button
            type="button"
            className="nd-btn"
            onClick={() => void apply(DEFAULT_RECORD_HOTKEY)}
          >
            Use {formatAccel(DEFAULT_RECORD_HOTKEY)}
          </button>
        )}
      </div>
      {hint && (
        <p className="text-xs text-[var(--color-text-muted)] max-w-[280px] text-right">
          {hint}
        </p>
      )}
      {error && (
        <p className="text-xs text-[var(--color-danger)] max-w-[280px] text-right">
          {error}
        </p>
      )}
    </div>
  );
}
