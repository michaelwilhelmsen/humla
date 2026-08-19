// The global record hotkey (issue #21): the accelerator vocabulary shared with
// the backend, plus the "what should this key do right now" rule.
//
// The accelerator string is `global_hotkey`'s parse format — modifiers joined
// by `+`, then one key named the way the DOM's `KeyboardEvent.code` names it
// (`KeyH`, `Digit1`, `ArrowUp`). That overlap is not a coincidence we're
// leaning on by luck: it's why the recorder can hand `e.code` straight to Rust
// with no translation table between the two sides to keep in sync.

import type { RecordingPhase } from "./ipc";

// ⌃⌘R. See `DEFAULT_RECORD_HOTKEY` in `src-tauri/src/menubar.rs` for why this
// combination and not another — it is the half of the pair that carries the
// reasoning. Change both.
export const DEFAULT_RECORD_HOTKEY = "Command+Control+KeyR";

type ModifierState = {
  code: string;
  altKey: boolean;
  shiftKey: boolean;
  metaKey: boolean;
  ctrlKey: boolean;
};

// Codes that only ever name a modifier. Held down on their own they produce a
// keydown whose `code` is one of these, which is not a shortcut — it's the
// user still in the middle of pressing one.
const MODIFIER_CODES = /^(Shift|Alt|Control|Meta)(Left|Right)$/;

/**
 * Whether this `KeyboardEvent.code` names a modifier and nothing else — the
 * user still in the middle of pressing a combination.
 *
 * The recorder needs this to tell "keep waiting" apart from "that isn't a
 * shortcut": both come back from [`accelFromEvent`] as `null`, but only the
 * second one is worth complaining about.
 */
export function isModifierOnly(code: string): boolean {
  return MODIFIER_CODES.test(code);
}

/** The accelerator for a keydown, or `null` if it isn't a combination we allow. */
export function accelFromEvent(e: ModifierState): string | null {
  if (!e.code || MODIFIER_CODES.test(e.code)) return null;

  const mods: string[] = [];
  if (e.metaKey) mods.push("Command");
  if (e.ctrlKey) mods.push("Control");
  if (e.altKey) mods.push("Alt");
  if (e.shiftKey) mods.push("Shift");

  // At least one modifier that isn't Shift. A global hotkey with no modifier
  // swallows that key in every app on the Mac, and ⇧H is just how you type an
  // H — neither is a shortcut the user can live with.
  if (!e.metaKey && !e.ctrlKey && !e.altKey) return null;

  return [...mods, e.code].join("+");
}

const MOD_GLYPHS: Record<string, string> = {
  // macOS renders modifiers in this order regardless of how they're written,
  // hence the explicit rank rather than the accel's own order.
  Control: "⌃",
  Ctrl: "⌃",
  Alt: "⌥",
  Option: "⌥",
  Shift: "⇧",
  Command: "⌘",
  Cmd: "⌘",
  Super: "⌘",
};
const MOD_ORDER = ["⌃", "⌥", "⇧", "⌘"];

const KEY_GLYPHS: Record<string, string> = {
  ArrowUp: "↑",
  ArrowDown: "↓",
  ArrowLeft: "←",
  ArrowRight: "→",
  Enter: "↩",
  Tab: "⇥",
  Escape: "⎋",
  Backspace: "⌫",
  Delete: "⌦",
  Comma: ",",
  Period: ".",
  Slash: "/",
  Backslash: "\\",
  Minus: "-",
  Equal: "=",
  Semicolon: ";",
  Quote: "'",
  Backquote: "`",
  BracketLeft: "[",
  BracketRight: "]",
};

/** An accelerator as macOS would print it — `"Alt+Shift+KeyH"` → `"⌥⇧H"`. */
export function formatAccel(accel: string): string {
  if (!accel.trim()) return "None";
  const tokens = accel.split("+").map((t) => t.trim()).filter(Boolean);
  const glyphs: string[] = [];
  let key = "";
  for (const token of tokens) {
    const glyph = MOD_GLYPHS[token];
    if (glyph) {
      if (!glyphs.includes(glyph)) glyphs.push(glyph);
      continue;
    }
    key = keyGlyph(token);
  }
  glyphs.sort((a, b) => MOD_ORDER.indexOf(a) - MOD_ORDER.indexOf(b));
  return glyphs.join("") + key;
}

function keyGlyph(code: string): string {
  if (KEY_GLYPHS[code]) return KEY_GLYPHS[code];
  if (/^Key[A-Z]$/.test(code)) return code.slice(3);
  if (/^Digit[0-9]$/.test(code)) return code.slice(5);
  return code;
}

export type HotkeyAction =
  | { kind: "start"; noteId: string }
  | { kind: "startNew" }
  | { kind: "stop" }
  | { kind: "ignore" };

/**
 * What the record hotkey should do, given what's on screen and what the
 * pipeline is doing.
 *
 * The hotkey is deliberately context-sensitive: with a note open it is that
 * note's Record button, and with nothing open it starts a fresh note — one key
 * that does what the screen implies. This is the visible-window half of the
 * rule; when the window is hidden the backend runs the headless equivalent
 * (`menubar::headless_start`) and never gets here.
 */
export function resolveHotkeyAction(ctx: {
  phase: RecordingPhase;
  activeNoteId: string | null;
  routeNoteId: string | null;
  routeReadOnly: boolean;
}): HotkeyAction {
  if (ctx.phase === "recording" || ctx.phase === "paused") return { kind: "stop" };
  // Anything other than a clean idle is the pipeline mid-transition —
  // including `idle` that still carries a note id, which is the post-stop
  // chain finishing. Starting there would race it.
  if (ctx.phase !== "idle" || ctx.activeNoteId !== null) return { kind: "ignore" };
  if (ctx.routeNoteId && !ctx.routeReadOnly) return { kind: "start", noteId: ctx.routeNoteId };
  return { kind: "startNew" };
}
