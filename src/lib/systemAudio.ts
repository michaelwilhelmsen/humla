import type { PermissionKind, PermissionsStatus } from "./ipc";

// On macOS 14.2+ the other side of a call is recorded through the System
// Audio Recording permission; before that, through Screen Recording. Which
// one a recording actually uses is decided in `system_audio_source`
// (src-tauri/src/commands/permissions.rs).

const ASKABLE = new Set(["granted", "denied", "not_determined"]);

/** The permission to show and ask for on this Mac. */
export function systemAudioKind(s: PermissionsStatus): Extract<PermissionKind, "systemAudio" | "screen"> {
  return ASKABLE.has(s.systemAudio ?? "") ? "systemAudio" : "screen";
}

/** A recording will capture the other side of a call. */
export function systemAudioReady(s: PermissionsStatus): boolean {
  return s.systemAudio === "granted" || s.screen === "granted";
}

/** Records through Screen Recording and has never been asked for system audio. */
export function offersSystemAudioSwitch(s: PermissionsStatus): boolean {
  return s.systemAudio === "not_determined" && s.screen === "granted";
}
