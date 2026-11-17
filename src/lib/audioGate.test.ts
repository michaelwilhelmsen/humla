import { describe, expect, it } from "vitest";
import sessions from "../../src-tauri/src/sessions.rs?raw";
import vectors from "./audioGateVectors.json";
import { retainsAudio } from "./audioGate";

describe("retainsAudio", () => {
  it("matches the vectors shared with sessions::retain_audio", () => {
    expect(vectors.length).toBeGreaterThanOrEqual(8);
    for (const v of vectors) {
      expect(retainsAudio(v.setting, v.forbidden), JSON.stringify(v)).toBe(v.retains);
    }
  });

  it("is the gate the Rust side pins to the same file", () => {
    expect(sessions).toContain('include_str!("../../src/lib/audioGateVectors.json")');
    expect(sessions).toMatch(/pub fn retain_audio\(keep_setting: Option<&str>, forbidden: bool\) -> bool/);
  });
});
