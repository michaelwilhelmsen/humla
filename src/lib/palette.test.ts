// The theme axis: which design is selected, how an unknown stored value
// degrades, and the synchronous cache that keeps a non-default design from
// flashing `warm` on launch.

import { beforeEach, describe, expect, it, vi } from "vitest";

// vi.hoisted, because vi.mock is lifted above every other statement in the file
// and would otherwise reference the spies before they exist.
const { getSetting, setSetting } = vi.hoisted(() => ({
  getSetting: vi.fn(),
  setSetting: vi.fn(),
}));
vi.mock("./ipc", () => ({ ipc: { getSetting, setSetting } }));

import {
  DEFAULT_PALETTE,
  PALETTE_REGISTRY,
  applyCachedPalette,
  isPalette,
  usePaletteStore,
} from "./palette";

function paletteAttr() {
  return document.documentElement.getAttribute("data-palette");
}

beforeEach(() => {
  getSetting.mockReset();
  setSetting.mockReset().mockResolvedValue(undefined);
  localStorage.clear();
  document.documentElement.removeAttribute("data-palette");
  usePaletteStore.setState({ palette: DEFAULT_PALETTE });
});

describe("the registry", () => {
  it("holds the default", () => {
    expect(isPalette(DEFAULT_PALETTE)).toBe(true);
  });

  it("accepts only registered ids", () => {
    // The two values most likely to be sitting in a real install's settings row:
    // "nothing" predates the split, and "ember" shipped in it and was withdrawn.
    // Neither may resolve to anything but the default.
    expect(isPalette("nothing")).toBe(false);
    expect(isPalette("ember")).toBe(false);
    expect(isPalette("")).toBe(false);
    expect(isPalette(undefined)).toBe(false);
    for (const p of PALETTE_REGISTRY) expect(isPalette(p.id)).toBe(true);
  });
});

describe("hydrate", () => {
  it("applies a stored design", async () => {
    getSetting.mockResolvedValue("graphite");
    await usePaletteStore.getState().hydrate();
    expect(paletteAttr()).toBe("graphite");
    expect(usePaletteStore.getState().palette).toBe("graphite");
  });

  it.each(["nothing", "ember"])("falls back to the default for a retired design (%s)", async (stale) => {
    getSetting.mockResolvedValue(stale);
    await usePaletteStore.getState().hydrate();
    expect(paletteAttr()).toBe(DEFAULT_PALETTE);
  });

  it("falls back to the default when nothing is stored", async () => {
    getSetting.mockResolvedValue(null);
    await usePaletteStore.getState().hydrate();
    expect(paletteAttr()).toBe(DEFAULT_PALETTE);
  });
});

describe("setPalette", () => {
  it("applies, stores, and caches", async () => {
    await usePaletteStore.getState().setPalette("graphite");
    expect(paletteAttr()).toBe("graphite");
    expect(setSetting).toHaveBeenCalledWith("palette", "graphite");
    expect(localStorage.getItem("humla.palette")).toBe("graphite");
  });

  it("paints before the write resolves — the attribute is not awaited on IPC", async () => {
    let release: () => void = () => {};
    setSetting.mockImplementation(() => new Promise<void>((r) => (release = r)));
    const pending = usePaletteStore.getState().setPalette("graphite");
    expect(paletteAttr()).toBe("graphite");
    release();
    await pending;
  });
});

describe("applyCachedPalette", () => {
  it("applies the cached design synchronously", () => {
    localStorage.setItem("humla.palette", "graphite");
    applyCachedPalette();
    expect(paletteAttr()).toBe("graphite");
  });

  it("leaves the attribute unset when there is no cache", () => {
    // Deliberate: with no attribute, warm.css's :root fallback paints the
    // default. Writing "warm" explicitly would be the same result by a longer
    // route — and would be wrong if the default ever changed.
    applyCachedPalette();
    expect(paletteAttr()).toBeNull();
  });

  it("ignores a cached value that is no longer a real design", () => {
    // The launch after a design is withdrawn: the cache still names it, and the
    // attribute must stay unset so warm.css's :root fallback paints instead.
    localStorage.setItem("humla.palette", "ember");
    applyCachedPalette();
    expect(paletteAttr()).toBeNull();
  });
});
