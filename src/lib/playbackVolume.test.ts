import { afterEach, describe, expect, it, vi } from "vitest";
import { applyPlaybackVolume, resolveStoredVolume, snapToUnity, VOLUME_MAX } from "./playbackVolume";

describe("resolveStoredVolume", () => {
  it("defaults to 100% when nothing usable is stored", () => {
    for (const raw of [null, "", "loud", "-0.5", String(VOLUME_MAX + 1), "NaN"]) {
      expect(resolveStoredVolume(raw)).toBe(1);
    }
  });

  it("keeps a stored level, silence and a boost included", () => {
    expect(resolveStoredVolume("0")).toBe(0);
    expect(resolveStoredVolume("0.4")).toBe(0.4);
    expect(resolveStoredVolume("2.5")).toBe(2.5);
  });
});

describe("snapToUnity", () => {
  it("holds a drag that passes near 100% at 100%", () => {
    expect(snapToUnity(0.95)).toBe(1);
    expect(snapToUnity(1.05)).toBe(1);
  });

  it("leaves values away from 100% alone", () => {
    expect(snapToUnity(0.85)).toBe(0.85);
    expect(snapToUnity(1.2)).toBe(1.2);
    expect(snapToUnity(0)).toBe(0);
  });
});

function fakeContext() {
  const nodes = { connected: [] as string[], gain: { gain: { value: 1 } } };
  const node = (name: string, extra = {}) => ({
    ...extra,
    connect(next: { name?: string }) {
      nodes.connected.push(`${name}->${next.name}`);
      return next;
    },
    name,
  });
  const param = () => ({ value: 0 });
  class Ctx {
    state = "suspended";
    destination = { name: "destination" };
    resume = vi.fn(async () => {
      this.state = "running";
    });
    createMediaElementSource = vi.fn(() => node("source"));
    createGain = () => Object.assign(node("gain"), nodes.gain);
    createDynamicsCompressor = () =>
      node("limiter", { threshold: param(), knee: param(), ratio: param(), attack: param(), release: param() });
  }
  return { Ctx, nodes };
}

describe("applyPlaybackVolume", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("uses the element's own volume up to 100%, without routing it", () => {
    const { Ctx } = fakeContext();
    const create = vi.fn();
    vi.stubGlobal("AudioContext", class extends Ctx { constructor() { super(); create(); } });
    const audio = document.createElement("audio");
    applyPlaybackVolume(audio, 0.6);
    expect(audio.volume).toBeCloseTo(0.6);
    expect(create).not.toHaveBeenCalled();
  });

  it("boosts past 100% through a gain stage and a limiter", () => {
    const { Ctx, nodes } = fakeContext();
    vi.stubGlobal("AudioContext", Ctx);
    const audio = document.createElement("audio");
    applyPlaybackVolume(audio, 3);
    expect(audio.volume).toBe(1);
    expect(nodes.gain.gain.value).toBe(3);
    expect(nodes.connected).toEqual(["source->gain", "gain->limiter", "limiter->destination"]);
  });

  it("keeps the routing once built, and turns down through it", () => {
    const { Ctx, nodes } = fakeContext();
    vi.stubGlobal("AudioContext", Ctx);
    const audio = document.createElement("audio");
    applyPlaybackVolume(audio, 2);
    applyPlaybackVolume(audio, 0.5);
    expect(audio.volume).toBe(1);
    expect(nodes.gain.gain.value).toBe(0.5);
  });

  it("falls back to full element volume where Web Audio is missing", () => {
    vi.stubGlobal("AudioContext", undefined);
    const audio = document.createElement("audio");
    applyPlaybackVolume(audio, 3);
    expect(audio.volume).toBe(1);
  });
});
