import { useEffect } from "react";
import { create } from "zustand";
import { ipc } from "./ipc";

// Two palettes. "warm" is the default — cream-paper canvas with a slightly
// darker cream sidebar and white sidebar hover/active. "nothing" is the
// original Nothing-design neutral-gray palette.
export type Palette = "warm" | "nothing";

type PaletteState = {
  palette: Palette;
  setPalette: (p: Palette) => Promise<void>;
  hydrate: () => Promise<void>;
};

function apply(palette: Palette) {
  document.documentElement.setAttribute("data-palette", palette);
}

export const usePaletteStore = create<PaletteState>((set) => ({
  palette: "warm",
  setPalette: async (palette) => {
    apply(palette);
    set({ palette });
    await ipc.setSetting("palette", palette);
  },
  hydrate: async () => {
    const stored = (await ipc.getSetting("palette")) as Palette | null;
    const palette: Palette = stored === "nothing" || stored === "warm" ? stored : "warm";
    apply(palette);
    set({ palette });
  },
}));

export function usePaletteBoot() {
  const hydrate = usePaletteStore((s) => s.hydrate);
  useEffect(() => {
    hydrate();
  }, [hydrate]);
}
