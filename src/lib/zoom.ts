import { useEffect } from "react";
import { create } from "zustand";
import { getCurrentWebview } from "@tauri-apps/api/webview";

const ZOOM_KEY = "humla.zoom";
const ZOOM_MIN = 0.5;
const ZOOM_MAX = 2.0;
const ZOOM_STEP = 0.1;
const ZOOM_DEFAULT = 1.0;

function clamp(v: number): number {
  return Math.min(ZOOM_MAX, Math.max(ZOOM_MIN, Math.round(v * 10) / 10));
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
    await getCurrentWebview().setZoom(z);
    set({ zoom: z });
    localStorage.setItem(ZOOM_KEY, String(z));
  },

  zoomIn: () => get().setZoom(get().zoom + ZOOM_STEP),
  zoomOut: () => get().setZoom(get().zoom - ZOOM_STEP),
  zoomReset: () => get().setZoom(ZOOM_DEFAULT),

  hydrate: async () => {
    const stored = parseFloat(localStorage.getItem(ZOOM_KEY) ?? "");
    const z = isNaN(stored) ? ZOOM_DEFAULT : clamp(stored);
    await getCurrentWebview().setZoom(z);
    set({ zoom: z });
  },
}));

export function useZoomBoot() {
  const hydrate = useZoomStore((s) => s.hydrate);
  useEffect(() => { hydrate(); }, [hydrate]);
}
