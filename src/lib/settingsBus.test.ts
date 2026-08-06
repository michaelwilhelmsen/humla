import { describe, it, expect, beforeEach } from "vitest";
import { renderHook, waitFor, act } from "@testing-library/react";
import { broadcastSettingChange, useLiveSetting } from "./settingsBus";
import { mockTauri } from "../test/tauri";

describe("useLiveSetting", () => {
  beforeEach(() => {
    mockTauri({ settings_get: () => "false" });
  });

  it("reads the stored value on mount", async () => {
    const { result } = renderHook(() => useLiveSetting("keep_audio"));
    await waitFor(() => expect(result.current).toBe("false"));
  });

  // The bug this exists for (#24, caught in a browser): the Note panel read
  // keep_audio once and never again, so flipping retention on in Settings left
  // the "Audio not stored on this device" hint up. Navigation can't be the cue —
  // App pins <Routes location={...}> while the Settings dialog is open, so the
  // view behind it observes no location change at all.
  it("adopts a value written elsewhere in the app, with no navigation", async () => {
    const { result } = renderHook(() => useLiveSetting("keep_audio"));
    await waitFor(() => expect(result.current).toBe("false"));

    act(() => broadcastSettingChange("keep_audio", "true"));

    expect(result.current).toBe("true");
  });

  it("ignores writes to other keys", async () => {
    const { result } = renderHook(() => useLiveSetting("keep_audio"));
    await waitFor(() => expect(result.current).toBe("false"));

    act(() => broadcastSettingChange("theme", "dark"));

    expect(result.current).toBe("false");
  });

  it("stops listening once unmounted", async () => {
    const { result, unmount } = renderHook(() => useLiveSetting("keep_audio"));
    await waitFor(() => expect(result.current).toBe("false"));
    unmount();
    // No throw, and nothing to assert on a torn-down hook — this guards the
    // removeEventListener, which a leaked listener would silently skip.
    expect(() => broadcastSettingChange("keep_audio", "true")).not.toThrow();
  });
});
