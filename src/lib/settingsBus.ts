import { useEffect, useState } from "react";
import { ipc, type SettingsKey } from "./ipc";

// A settings value is written in exactly one place (`useSettings.update`) but
// read in long-lived views that outlive the write — the Note panel's audio
// retention hint (#24) is the first.
//
// Navigation can't be the cue to re-read. `/settings` is a **dialog over a
// pinned router location**: `App`'s `<Routes location={displayLocation}>` keeps
// the background view mounted at its own location for as long as the dialog is
// open, so a component behind it never observes the trip to `/settings` and
// back — `useLocation()` there returns the same pathname throughout. (That is
// the whole point of the pin; it's what keeps the view alive underneath.) So
// the write announces itself instead.
const SETTING_CHANGED = "humla:setting-changed";

type Detail = { key: SettingsKey; value: string };

/** Announce a settings write. Called by `writeSetting`, nowhere else. */
export function broadcastSettingChange(key: SettingsKey, value: string) {
  window.dispatchEvent(new CustomEvent<Detail>(SETTING_CHANGED, { detail: { key, value } }));
}

/** Store a setting and announce it: the one write path for a value some other
 *  view may be showing. */
export async function writeSetting(key: SettingsKey, value: string): Promise<void> {
  await ipc.setSetting(key, value);
  broadcastSettingChange(key, value);
}

/** Hear every announced write. Returns the unsubscribe. */
export function onSettingChange(cb: (key: SettingsKey, value: string) => void): () => void {
  const onChange = (e: Event) => {
    const detail = (e as CustomEvent<Detail>).detail;
    if (detail) cb(detail.key, detail.value);
  };
  window.addEventListener(SETTING_CHANGED, onChange);
  return () => window.removeEventListener(SETTING_CHANGED, onChange);
}

/**
 * One settings value, read on mount and kept current as it's written elsewhere
 * in the app. Returns `null` until the first read resolves, so callers can tell
 * "not loaded yet" from "empty" — and shouldn't promise anything on the strength
 * of a value they don't have yet.
 */
export function useLiveSetting(key: SettingsKey): string | null {
  const [value, setValue] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    ipc.getSetting(key).then((v) => {
      if (!cancelled) setValue(v);
    });
    const off = onSettingChange((changed, v) => {
      if (changed === key) setValue(v);
    });
    return () => {
      cancelled = true;
      off();
    };
  }, [key]);

  return value;
}
