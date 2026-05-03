import { useEffect, useState } from "react";
import { ipc } from "./ipc";

// Custom DOM event used to broadcast developer-mode toggles across the
// app without wiring a Zustand slice or a context. The toggle UI lives
// in About.tsx; consumers (DiagnosticsLinks, threshold sliders) listen
// for the event so they react immediately instead of waiting for their
// next mount.
export const DEV_MODE_EVENT = "humla:dev-mode-changed";

export function useDeveloperMode(): boolean {
  const [on, setOn] = useState(false);
  useEffect(() => {
    let cancelled = false;
    ipc
      .getSetting("developer_mode")
      .then((v) => {
        if (!cancelled) setOn(v === "true");
      })
      .catch(() => {});
    const handler = (e: Event) => {
      const detail = (e as CustomEvent<boolean>).detail;
      if (typeof detail === "boolean") setOn(detail);
    };
    window.addEventListener(DEV_MODE_EVENT, handler);
    return () => {
      cancelled = true;
      window.removeEventListener(DEV_MODE_EVENT, handler);
    };
  }, []);
  return on;
}

export function emitDeveloperModeChange(on: boolean) {
  window.dispatchEvent(new CustomEvent(DEV_MODE_EVENT, { detail: on }));
}
