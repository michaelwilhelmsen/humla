import "@testing-library/jest-dom/vitest";
import { afterEach } from "vitest";
import { cleanup, configure } from "@testing-library/react";
import { mockIPC } from "@tauri-apps/api/mocks";

// findBy*'s default 1s can flake under parallel test workers when a
// component chains async loads (cloud status → members). Generous ceiling;
// passing tests don't get slower, only genuinely-absent elements do.
configure({ asyncUtilTimeout: 4000 });

// The event plugin unregisters listeners through this internal, which
// mockIPC doesn't provide.
(window as unknown as Record<string, unknown>).__TAURI_EVENT_PLUGIN_INTERNALS__ = {
  unregisterListener: () => {},
};

afterEach(() => {
  cleanup();
  // Don't clearMocks(): unmount cleanups (event unlisten etc.) resolve
  // async after afterEach and need a live invoke. Park a benign handler
  // instead; each test installs its own via mockTauri().
  mockIPC(async () => null);
});

// jsdom lacks matchMedia; theme/palette boot and CSS hooks touch it.
if (!window.matchMedia) {
  window.matchMedia = (query: string) =>
    ({
      matches: false,
      media: query,
      onchange: null,
      addListener: () => {},
      removeListener: () => {},
      addEventListener: () => {},
      removeEventListener: () => {},
      dispatchEvent: () => false,
    }) as MediaQueryList;
}
