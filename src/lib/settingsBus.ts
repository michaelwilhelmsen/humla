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

/** Announce a settings write. Called by `useSettings.update`, nowhere else. */
export function broadcastSettingChange(key: SettingsKey, value: string) {
  window.dispatchEvent(new CustomEvent<Detail>(SETTING_CHANGED, { detail: { key, value } }));
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
    const onChange = (e: Event) => {
      const detail = (e as CustomEvent<Detail>).detail;
      if (detail?.key === key) setValue(detail.value);
    };
    window.addEventListener(SETTING_CHANGED, onChange);
    return () => {
      cancelled = true;
      window.removeEventListener(SETTING_CHANGED, onChange);
    };
  }, [key]);

  return value;
}
