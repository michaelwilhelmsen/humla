import { useEffect } from "react";
import { create } from "zustand";
import { getCurrentWebview } from "@tauri-apps/api/webview";

const ZOOM_KEY = "humla.zoom";
const ZOOM_MIN = 0.5;
const ZOOM_MAX = 2.0;
const ZOOM_STEP = 0.1;
const ZOOM_DEFAULT = 1.0;

/** Native traffic-light row height in device px — CSS spacer scales by 1/zoom. */
export const TRAFFIC_LIGHT_CLEARANCE_PX = 34;

export function clamp(v: number): number {
  return Math.min(ZOOM_MAX, Math.max(ZOOM_MIN, Math.round(v * 10) / 10));
}

/** Parse a localStorage value into a clamped zoom; garbage → default. */
export function resolveStoredZoom(raw: string | null): number {
  const stored = parseFloat(raw ?? "");
  return Number.isNaN(stored) ? ZOOM_DEFAULT : clamp(stored);
}

/** CSS px for the traffic-light spacer so device height stays constant under zoom. */
export function trafficLightSpacerCssPx(zoom: number): number {
  return TRAFFIC_LIGHT_CLEARANCE_PX / zoom;
}

type ZoomState = {
  zoom: number;
  setZoom: (level: number) => Promise<void>;
  zoomIn: () => Promise<void>;
  zoomOut: () => Promise<void>;
  zoomReset: () => Promise<void>;
  hydrate: () => Promise<void>;
};

export const useZoomStore = create<ZoomState>((set, get) => ({
  zoom: ZOOM_DEFAULT,

  setZoom: async (level) => {
    const z = clamp(level);
    try {
      await getCurrentWebview().setZoom(z);
    } catch {
      // Tests / non-Tauri shells lack webview metadata; shortcuts must not reject.
      return;
    }
    set({ zoom: z });
    localStorage.setItem(ZOOM_KEY, String(z));
  },

  zoomIn: () => get().setZoom(get().zoom + ZOOM_STEP),
  zoomOut: () => get().setZoom(get().zoom - ZOOM_STEP),
  zoomReset: () => get().setZoom(ZOOM_DEFAULT),

  hydrate: async () => {
    const z = resolveStoredZoom(localStorage.getItem(ZOOM_KEY));
    try {
      await getCurrentWebview().setZoom(z);
    } catch {
      // Same gap as setZoom — still sync the store so the spacer can react.
    }
    set({ zoom: z });
  },
}));

export function useZoomBoot() {
  const hydrate = useZoomStore((s) => s.hydrate);
  useEffect(() => {
    void hydrate();
  }, [hydrate]);
}
