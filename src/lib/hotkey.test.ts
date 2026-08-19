import { describe, expect, it } from "vitest";
import {
  DEFAULT_RECORD_HOTKEY,
  accelFromEvent,
  formatAccel,
  isModifierOnly,
  resolveHotkeyAction,
} from "./hotkey";

// Minimal stand-in for a KeyboardEvent — only the fields accelFromEvent reads.
function key(
  code: string,
  mods: Partial<Record<"altKey" | "shiftKey" | "metaKey" | "ctrlKey", boolean>> = {},
) {
  return {
    code,
    altKey: false,
    shiftKey: false,
    metaKey: false,
    ctrlKey: false,
    ...mods,
  };
}

describe("accelFromEvent", () => {
  it("builds the accelerator the Rust parser expects", () => {
    expect(accelFromEvent(key("KeyH", { altKey: true, shiftKey: true }))).toBe(
      "Alt+Shift+KeyH",
    );
  });

  it("emits modifiers in a stable order regardless of which are held", () => {
    const all = key("KeyJ", {
      metaKey: true,
      ctrlKey: true,
      altKey: true,
      shiftKey: true,
    });
    expect(accelFromEvent(all)).toBe("Command+Control+Alt+Shift+KeyJ");
  });

  it("accepts digits and other Code-named keys", () => {
    expect(accelFromEvent(key("Digit1", { metaKey: true, altKey: true }))).toBe(
      "Command+Alt+Digit1",
    );
    expect(accelFromEvent(key("Space", { ctrlKey: true, altKey: true }))).toBe(
      "Control+Alt+Space",
    );
  });

  // A global hotkey with no modifier would swallow that key in every other
  // app on the Mac, so it is not a combination we let the user record.
  it("rejects a bare key with no modifier", () => {
    expect(accelFromEvent(key("KeyH"))).toBeNull();
  });

  // Shift alone is not enough either: ⇧H is what typing an H does.
  it("rejects Shift as the only modifier", () => {
    expect(accelFromEvent(key("KeyH", { shiftKey: true }))).toBeNull();
  });

  it("ignores a modifier pressed on its own", () => {
    expect(accelFromEvent(key("ShiftLeft", { shiftKey: true }))).toBeNull();
    expect(accelFromEvent(key("AltRight", { altKey: true }))).toBeNull();
    expect(accelFromEvent(key("MetaLeft", { metaKey: true }))).toBeNull();
    expect(accelFromEvent(key("ControlLeft", { ctrlKey: true }))).toBeNull();
  });

  it("rejects a key it has no name for", () => {
    expect(accelFromEvent(key("", { altKey: true }))).toBeNull();
  });
});

describe("formatAccel", () => {
  it("renders the default as macOS glyphs", () => {
    expect(formatAccel(DEFAULT_RECORD_HOTKEY)).toBe("⌃⌘R");
  });

  it("orders glyphs the way macOS does, whatever order the accel is in", () => {
    expect(formatAccel("Shift+Alt+Command+Control+KeyJ")).toBe("⌃⌥⇧⌘J");
  });

  it("names keys that have no single-character form", () => {
    expect(formatAccel("Alt+Shift+Space")).toBe("⌥⇧Space");
    expect(formatAccel("Command+Alt+Digit1")).toBe("⌥⌘1");
    expect(formatAccel("Alt+Shift+ArrowUp")).toBe("⌥⇧↑");
  });

  it("says so when there is no shortcut at all", () => {
    expect(formatAccel("")).toBe("None");
  });
});

describe("resolveHotkeyAction", () => {
  const idle = { phase: "idle" as const, activeNoteId: null, routeNoteId: null, routeReadOnly: false };

  it("starts on the open note when one is open", () => {
    expect(resolveHotkeyAction({ ...idle, routeNoteId: "n1" })).toEqual({
      kind: "start",
      noteId: "n1",
    });
  });

  it("starts a fresh note when no note is open", () => {
    expect(resolveHotkeyAction(idle)).toEqual({ kind: "startNew" });
  });

  it("stops a recording in flight", () => {
    expect(
      resolveHotkeyAction({ ...idle, phase: "recording", activeNoteId: "n1", routeNoteId: "n1" }),
    ).toEqual({ kind: "stop" });
  });

  // The hotkey is a start/stop toggle (issue #21), not the in-app ⌘R
  // pause/resume — a paused recording stops rather than resuming.
  it("stops a paused recording rather than resuming it", () => {
    expect(
      resolveHotkeyAction({ ...idle, phase: "paused", activeNoteId: "n1" }),
    ).toEqual({ kind: "stop" });
  });

  it("does nothing while the pipeline is mid-transition", () => {
    for (const phase of ["starting", "stopping", "diarizing", "importing"] as const) {
      expect(resolveHotkeyAction({ ...idle, phase })).toEqual({ kind: "ignore" });
    }
  });

  // `idle` with a note id still attached is the post-stop chain finishing up;
  // starting another recording there would race it.
  it("does nothing while a note is still finishing up", () => {
    expect(resolveHotkeyAction({ ...idle, activeNoteId: "n1" })).toEqual({ kind: "ignore" });
  });

  // The record button is hidden for viewers on a shared note, so the
  // keyboard path has to be gated the same way — but a new personal note is
  // always fair game.
  it("starts a fresh note instead of a read-only one", () => {
    expect(
      resolveHotkeyAction({ ...idle, routeNoteId: "n1", routeReadOnly: true }),
    ).toEqual({ kind: "startNew" });
  });
});

describe("isModifierOnly", () => {
  it("recognises a modifier held on its own", () => {
    for (const code of ["ShiftLeft", "ShiftRight", "AltLeft", "ControlRight", "MetaLeft"]) {
      expect(isModifierOnly(code)).toBe(true);
    }
  });

  it("does not claim real keys", () => {
    for (const code of ["KeyH", "Digit1", "Space", "Escape", ""]) {
      expect(isModifierOnly(code)).toBe(false);
    }
  });
});
