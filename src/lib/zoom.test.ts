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
