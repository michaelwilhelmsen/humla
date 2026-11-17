import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  plugins: [react(), tailwindcss()],
  clearScreen: false,
  build: {
    // The oldest webview the app runs in: WKWebView on macOS 13.0, the
    // bundle's minimumSystemVersion.
    target: "safari16",
    // The bundle loads from the app's own disk, not over a network, so Vite's
    // web-sized chunk warning doesn't apply.
    chunkSizeWarningLimit: 2000,
  },
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? { protocol: "ws", host, port: 1421 }
      : undefined,
    watch: {
      ignored: ["**/src-tauri/**", "**/audio-capture/**"],
    },
  },
});
