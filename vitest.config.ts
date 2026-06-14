import { defineConfig } from "vitest/config";

// Standalone test config (kept separate from vite.config.ts so the Tauri
// dev/build pipeline never pulls in the test runner). jsdom gives store/UI
// tests a `window`; pure-function tests don't need it but it's harmless.
export default defineConfig({
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.{ts,tsx}"],
  },
});
