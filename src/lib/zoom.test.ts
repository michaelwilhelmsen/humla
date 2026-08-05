import { describe, it, expect, beforeEach } from "vitest";
import {
  clamp,
  resolveStoredZoom,
  trafficLightSpacerCssPx,
  TRAFFIC_LIGHT_CLEARANCE_PX,
  useZoomStore,
} from "./zoom";

describe("clamp", () => {
  it("keeps values inside 0.5–2.0", () => {
    expect(clamp(0.5)).toBe(0.5);
    expect(clamp(1)).toBe(1);
    expect(clamp(2)).toBe(2);
  });

  it("floors below the min and ceilings above the max", () => {
    expect(clamp(0.1)).toBe(0.5);
    expect(clamp(0)).toBe(0.5);
    expect(clamp(-1)).toBe(0.5);
    expect(clamp(2.5)).toBe(2);
    expect(clamp(10)).toBe(2);
  });

  it("rounds to one decimal to kill float drift on 0.1 steps", () => {
    expect(clamp(1.0000001)).toBe(1);
    // 0.1 steps accumulate binary float noise (e.g. 1 + 0.1*3 ≠ 1.3 exactly).
    expect(clamp(1 + 0.1 + 0.1 + 0.1)).toBe(1.3);
    expect(clamp(1.149)).toBe(1.1);
    expect(clamp(1.15)).toBe(1.2);
  });
});

describe("resolveStoredZoom", () => {
  it("returns the default for missing or garbage values", () => {
    expect(resolveStoredZoom(null)).toBe(1);
    expect(resolveStoredZoom("")).toBe(1);
    expect(resolveStoredZoom("nope")).toBe(1);
    expect(resolveStoredZoom("NaN")).toBe(1);
  });

  it("clamps stored values that sit outside the allowed range", () => {
    expect(resolveStoredZoom("0.2")).toBe(0.5);
    expect(resolveStoredZoom("3")).toBe(2);
    expect(resolveStoredZoom("1.5")).toBe(1.5);
  });
});

describe("trafficLightSpacerCssPx", () => {
  it("keeps device clearance constant under zoom", () => {
    expect(trafficLightSpacerCssPx(1)).toBe(TRAFFIC_LIGHT_CLEARANCE_PX);
    expect(trafficLightSpacerCssPx(0.5)).toBe(TRAFFIC_LIGHT_CLEARANCE_PX / 0.5);
    expect(trafficLightSpacerCssPx(2)).toBe(TRAFFIC_LIGHT_CLEARANCE_PX / 2);
  });
});

describe("hydrate", () => {
  beforeEach(() => {
    localStorage.clear();
    useZoomStore.setState({ zoom: 1 });
  });

  it("loads a clamped stored level even when setZoom is unavailable", async () => {
    localStorage.setItem("humla.zoom", "1.7");
    await useZoomStore.getState().hydrate();
    expect(useZoomStore.getState().zoom).toBe(1.7);
  });

  it("falls back to 1 when localStorage holds garbage", async () => {
    localStorage.setItem("humla.zoom", "banana");
    await useZoomStore.getState().hydrate();
    expect(useZoomStore.getState().zoom).toBe(1);
  });

  it("does not throw when the Tauri webview API is missing", async () => {
    await expect(useZoomStore.getState().hydrate()).resolves.toBeUndefined();
  });
});

describe("setZoom / zoomIn / zoomOut / zoomReset", () => {
  beforeEach(() => {
    localStorage.clear();
    useZoomStore.setState({ zoom: 1 });
  });

  // This is the exact path the global keydown handler awaits (shortcuts.ts) —
  // the original bug report was an unhandled rejection from here, not from hydrate.
  it("does not reject when the webview call fails", async () => {
    await expect(useZoomStore.getState().zoomIn()).resolves.toBeUndefined();
  });

  it("commits the clamped zoom even when the webview call fails", async () => {
    // setZoom must commit state (and persist it) *before* awaiting the native
    // call, not after — a held-down key fires the next keydown before the
    // previous call's native round-trip resolves, and zoomIn/zoomOut compute
    // their step from get().zoom. Committing after the await would let two
    // overlapping calls read the same stale zoom and collapse into one step.
    await useZoomStore.getState().setZoom(1.4);
    expect(useZoomStore.getState().zoom).toBe(1.4);
    expect(localStorage.getItem("humla.zoom")).toBe("1.4");
  });

  it("zoomIn and zoomOut step from the current zoom", async () => {
    await useZoomStore.getState().zoomIn();
    expect(useZoomStore.getState().zoom).toBe(1.1);
    await useZoomStore.getState().zoomOut();
    expect(useZoomStore.getState().zoom).toBe(1);
  });

  it("zoomReset returns to the default", async () => {
    await useZoomStore.getState().setZoom(1.7);
    await useZoomStore.getState().zoomReset();
    expect(useZoomStore.getState().zoom).toBe(1);
  });
});
