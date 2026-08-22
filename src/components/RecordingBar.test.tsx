import { describe, expect, it } from "vitest";
import { noAudioWarning } from "./RecordingBar";

// #174. The warning fired correctly and said nothing useful: a pair of
// headphones in another room held the macOS default input, and "check your
// microphone" pointed at the one device that wasn't the problem.
describe("noAudioWarning", () => {
  it("names the device it isn't hearing", () => {
    expect(noAudioWarning("AirPods Pro")).toBe(
      "No audio from AirPods Pro — check your input device in System Settings",
    );
  });

  it("falls back to the device-less copy when the name is unavailable", () => {
    // An older sidecar reports no device, and CoreAudio can fail to name one.
    // Naming nothing is better than naming a guess.
    for (const absent of [undefined, null, "", "   "]) {
      expect(noAudioWarning(absent)).toBe("No audio detected — check your microphone");
    }
  });

  it("trims a padded name rather than rendering the padding", () => {
    expect(noAudioWarning("  Studio Display Microphone  ")).toContain(
      "No audio from Studio Display Microphone —",
    );
  });

  it("clamps without splitting a character in half", () => {
    // Device names are user-authored, and people put emoji in them. Slicing by
    // UTF-16 code unit can cut a surrogate pair down the middle, which renders
    // as U+FFFD — a replacement glyph in a warning reads as a second bug.
    const msg = noAudioWarning(`${"a".repeat(38)}🎧🎧🎧`);
    expect(msg).not.toContain("\uFFFD");
    expect(msg).toContain("…");
    // No lone surrogate survived the clamp.
    for (const ch of msg) expect(ch.codePointAt(0)! & 0xf800).not.toBe(0xd800);
  });

  it("does not leave a space stranded before the ellipsis", () => {
    expect(noAudioWarning(`${"b".repeat(38)} tail`)).toContain("…");
    expect(noAudioWarning(`${"b".repeat(38)} tail`)).not.toContain(" …");
  });

  it("clamps a pathologically long name so the pill keeps its shape", () => {
    // Device names are user-authored, so the length is not ours to trust. The
    // pill wraps, so an unclamped name costs height rather than overflowing —
    // but it still has to stop somewhere.
    const msg = noAudioWarning("M".repeat(200));
    expect(msg.length).toBeLessThan(120);
    expect(msg).toContain("…");
  });
});
