import { useEffect } from "react";
import { create } from "zustand";
import { ipc } from "./ipc";

// The theme registry. One row per file in src/styles/themes/ — this is the only
// list; Settings renders from it (see settings/types.ts) rather than repeating
// the options, so adding a theme is one CSS file, one row here, and one @import
// in globals.css.
//
// A theme is a full design, not just a colour ramp: typeface, type scale,
// control metrics and colours all come from its token block (the contract lives
// in themeContract.ts). Both modes are always available — a recipe written for
// one mode carries a derived counterpart for the other — so this axis stays
// independent of the light/dark axis in theme.ts.
//
// `mode` records which mode the design was drawn in, purely so the picker can
// say so; it does not restrict anything.
export type PaletteDef = {
  id: string;
  label: string;
  description: string;
  mode: "light" | "dark";
};

// `satisfies` rather than a type annotation: the annotation would widen every
// `id` to `string` and take the literal union below with it.
export const PALETTE_REGISTRY = [
  {
    id: "warm",
    label: "Warm",
    description: "Humla's own: cream paper, Hanken Grotesk, gold accent.",
    mode: "light",
  },
  {
    id: "graphite",
    label: "Graphite",
    description: "SF Pro on white, DM Mono commands, compact ink chrome.",
    mode: "light",
  },
] as const satisfies readonly PaletteDef[];

export type Palette = (typeof PALETTE_REGISTRY)[number]["id"];

// The theme a document falls back to. warm.css also matches a document with no
// `data-palette` at all, so a stored value we don't recognise (an older build's
// "nothing", a hand-edited settings row) degrades to this design rather than to
// an unstyled app.
export const DEFAULT_PALETTE: Palette = "warm";

export function isPalette(value: unknown): value is Palette {
  return PALETTE_REGISTRY.some((p) => p.id === value);
}

type PaletteState = {
  palette: Palette;
  setPalette: (p: Palette) => Promise<void>;
  hydrate: () => Promise<void>;
};

function apply(palette: Palette) {
  document.documentElement.setAttribute("data-palette", palette);
  // Mirror into localStorage so the NEXT launch can paint the right design
  // before any IPC resolves. SQLite stays the source of truth; this is a cache
  // that hydrate() overwrites the moment the real value arrives.
  try {
    localStorage.setItem(CACHE_KEY, palette);
  } catch {
    /* private mode / quota — the cache is an optimisation, never load-bearing */
  }
}

const CACHE_KEY = "humla.palette";

// Called from main.tsx BEFORE the first render. Without it every non-default
// design flashes `warm` on launch: the settings read is async, so the first
// paint would happen with no `data-palette` attribute at all — which warm.css
// deliberately matches. Cheap enough to be unconditional, and wrong only for
// the one launch after the value is changed on another device (settings don't
// sync, so in practice never).
export function applyCachedPalette() {
  const cached = (() => {
    try {
      return localStorage.getItem(CACHE_KEY);
    } catch {
      return null;
    }
  })();
  if (isPalette(cached)) document.documentElement.setAttribute("data-palette", cached);
}

export const usePaletteStore = create<PaletteState>((set) => ({
  palette: DEFAULT_PALETTE,
  setPalette: async (palette) => {
    apply(palette);
    set({ palette });
    await ipc.setSetting("palette", palette);
  },
  hydrate: async () => {
    const stored = await ipc.getSetting("palette");
    const palette = isPalette(stored) ? stored : DEFAULT_PALETTE;
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
