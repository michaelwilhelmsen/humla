// The wizard's telemetry disclosure — and its control, in the same line.
//
// It lives in the wizard CANVAS rather than in a step, for two reasons. A returning
// user resumes at the first *incomplete* step (see `firstIncompleteStep`), so a notice
// attached to `welcome` would be skipped by exactly the people who left and came back;
// and someone who reads it on step one should still be able to act on it on step four.
//
// The sequencing is the point. Mounting this component is what writes
// `telemetry_enabled`, so the disclosure is on screen before any event can pass the
// backend's gate — never the reverse. That is why the copy does not say "you can turn
// this off in Settings": during setup, Settings is unreachable, so pointing there
// would name a control the reader cannot use. The control is here. Settings takes over
// afterwards, and the README says so.
import { useEffect, useState } from "react";
import { ipc } from "../../lib/ipc";

export function TelemetryNotice() {
  // null = still reading; true/false = known.
  const [on, setOn] = useState<boolean | null>(null);

  useEffect(() => {
    let cancelled = false;
    ipc
      .getSetting("telemetry_enabled")
      .then((v) => {
        if (cancelled) return;
        if (v === null) {
          // First time through the wizard: this render IS the disclosure, so opt the
          // install in and report the first launch. An install that never reaches
          // this footer is never enrolled.
          setOn(true);
          void ipc.setSetting("telemetry_enabled", "true").then(() => {
            void ipc.telemetryEvent("app_first_run");
          });
          return;
        }
        setOn(v === "true");
      })
      .catch(() => {
        // Unreadable setting → show nothing rather than claim a state we don't know.
        if (!cancelled) setOn(null);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  if (on === null) return null;

  function toggle() {
    const next = !on;
    setOn(next);
    void ipc.setSetting("telemetry_enabled", next ? "true" : "false").then(() => {
      // Turning it back on mid-wizard should still record the launch; the backend's
      // once-per-install marker keeps this from double counting.
      if (next) void ipc.telemetryEvent("app_first_run");
    });
  }

  return (
    <p className="text-[11px] leading-relaxed text-[var(--color-text-muted)] text-center max-w-md">
      {on
        ? "Anonymous setup counters are on — no identifier, nothing about your notes."
        : "Anonymous setup counters are off."}{" "}
      <button
        type="button"
        onClick={toggle}
        className="no-drag underline underline-offset-2 hover:text-[var(--color-text)] transition-colors"
      >
        {on ? "Turn off" : "Turn on"}
      </button>
    </p>
  );
}
