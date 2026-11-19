import { describe, expect, it } from "vitest";
import type { PermissionsStatus } from "./ipc";
import { offersSystemAudioSwitch, systemAudioKind, systemAudioReady } from "./systemAudio";

function s(screen: string, systemAudio?: string): PermissionsStatus {
  return { microphone: "granted", screen, systemAudio, notifications: "granted" } as PermissionsStatus;
}

describe("system audio permission", () => {
  it("asks for System Audio Recording where macOS has it, Screen Recording elsewhere", () => {
    expect(systemAudioKind(s("denied", "not_determined"))).toBe("systemAudio");
    expect(systemAudioKind(s("granted", "denied"))).toBe("systemAudio");
    expect(systemAudioKind(s("granted", "unsupported"))).toBe("screen");
    expect(systemAudioKind(s("granted", "unknown"))).toBe("screen");
    expect(systemAudioKind(s("granted"))).toBe("screen");
  });

  it("is ready with either permission", () => {
    expect(systemAudioReady(s("denied", "granted"))).toBe(true);
    expect(systemAudioReady(s("granted", "not_determined"))).toBe(true);
    expect(systemAudioReady(s("denied", "not_determined"))).toBe(false);
    expect(systemAudioReady(s("denied", "unsupported"))).toBe(false);
  });

  it("offers the switch only to a Screen Recording user who was never asked", () => {
    expect(offersSystemAudioSwitch(s("granted", "not_determined"))).toBe(true);
    expect(offersSystemAudioSwitch(s("granted", "denied"))).toBe(false);
    expect(offersSystemAudioSwitch(s("granted", "granted"))).toBe(false);
    expect(offersSystemAudioSwitch(s("denied", "not_determined"))).toBe(false);
    expect(offersSystemAudioSwitch(s("granted", "unsupported"))).toBe(false);
  });
});
