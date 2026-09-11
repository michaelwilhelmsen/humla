import { useEffect, useState } from "react";
import { useRecordingStore } from "./store";

/**
 * The capture's elapsed seconds, read off the store's clock rather than kept
 * by any one component. The reading is shown in several places that mount and
 * unmount independently — the note's bar, the compact pill in `Layout`, a note
 * card — and a count kept from mount would restart with each of them.
 */
export function useCaptureElapsed(): number {
  const activeSince = useRecordingStore((s) => s.activeSince);
  const activeAccumMs = useRecordingStore((s) => s.activeAccumMs);
  const [, tick] = useState(0);
  useEffect(() => {
    if (activeSince === null) return; // banked and frozen: nothing to advance
    const t = window.setInterval(() => tick((n) => n + 1), 250);
    return () => window.clearInterval(t);
  }, [activeSince]);
  const active = activeAccumMs + (activeSince !== null ? Date.now() - activeSince : 0);
  return Math.floor(active / 1000);
}

export function formatTime(s: number): string {
  const m = Math.floor(s / 60);
  const r = s % 60;
  return `${m}:${r.toString().padStart(2, "0")}`;
}
