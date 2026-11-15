/** The player's volume (#234): device-local, like the panel's width, and the
 * same for every note. Above 100% it is a boost, for takes recorded before
 * `playback.wav` was levelled. */

const KEY = "humla.playbackVolume";
export const VOLUME_MAX = 4;
export const VOLUME_DEFAULT = 1;

/** A drag that passes near 100% stops there, the way volume sliders with a
 * boost range hold at unity. Keyboard steps never snap, or 105% would be
 * unreachable from 100%. */
export const UNITY_DETENT = 0.08;
export function snapToUnity(volume: number): number {
  return Math.abs(volume - 1) < UNITY_DETENT ? 1 : volume;
}

export function resolveStoredVolume(raw: string | null): number {
  const v = raw == null || raw === "" ? NaN : Number(raw);
  return Number.isFinite(v) && v >= 0 && v <= VOLUME_MAX ? v : VOLUME_DEFAULT;
}

export function readPlaybackVolume(): number {
  try {
    return resolveStoredVolume(localStorage.getItem(KEY));
  } catch {
    return VOLUME_DEFAULT;
  }
}

export function writePlaybackVolume(volume: number): void {
  try {
    localStorage.setItem(KEY, String(volume));
  } catch {
    /* storage unavailable: the choice lasts this session only */
  }
}

type Graph = { ctx: AudioContext; gain: GainNode };
const graphs = new WeakMap<HTMLMediaElement, Graph>();

/** A media element's own volume stops at 100%, so a boost routes the element
 * through a gain stage and a limiter. That routing can't be undone for the
 * element, and plays silence unless the source answered CORS (the element has
 * `crossOrigin` set), so it is built only once a boost is asked for. */
export function applyPlaybackVolume(audio: HTMLMediaElement, volume: number): void {
  let graph = graphs.get(audio);
  if (!graph && volume > 1) graph = buildGraph(audio);
  if (!graph) {
    audio.volume = Math.min(1, volume);
    return;
  }
  audio.volume = 1;
  graph.gain.gain.value = volume;
  resumePlaybackVolume(audio);
}

/** An AudioContext may start suspended until a user gesture; pressing play is one. */
export function resumePlaybackVolume(audio: HTMLMediaElement): void {
  const graph = graphs.get(audio);
  if (graph?.ctx.state === "suspended") void graph.ctx.resume().catch(() => {});
}

function buildGraph(audio: HTMLMediaElement): Graph | undefined {
  const Ctx = typeof AudioContext === "undefined" ? undefined : AudioContext;
  if (!Ctx) return undefined;
  try {
    const ctx = new Ctx();
    const source = ctx.createMediaElementSource(audio);
    const gain = ctx.createGain();
    const limiter = ctx.createDynamicsCompressor();
    limiter.threshold.value = -3;
    limiter.knee.value = 0;
    limiter.ratio.value = 20;
    limiter.attack.value = 0.002;
    limiter.release.value = 0.1;
    source.connect(gain).connect(limiter).connect(ctx.destination);
    const graph = { ctx, gain };
    graphs.set(audio, graph);
    return graph;
  } catch {
    return undefined;
  }
}
