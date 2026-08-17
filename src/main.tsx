import React from "react";
import ReactDOM from "react-dom/client";
import { BrowserRouter } from "react-router-dom";
import App from "./App";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { applyCachedPalette } from "./lib/palette";
// Every typeface a theme can ask for, self-hosted via @fontsource — no external
// Google Fonts request, so the app renders offline and the CSP stays
// font-src 'self'. A theme picks one through --font-sans / --font-code; the
// others cost nothing at runtime because @font-face only fetches what a
// rendered glyph needs. Adding a theme with a new typeface means adding its
// package here AND in mockBoot.tsx.
//
// Hanken Grotesk (`warm`) — weights mirror the prototype: 400/500/600/700.
import "@fontsource/hanken-grotesk/400.css";
import "@fontsource/hanken-grotesk/500.css";
import "@fontsource/hanken-grotesk/600.css";
import "@fontsource/hanken-grotesk/700.css";
// DM Mono (`graphite`, commands only).
import "@fontsource/dm-mono/400.css";
// `graphite`'s body face is SF Pro, requested through the system stack rather
// than bundled — it ships with macOS and cannot be redistributed.
import "./styles/globals.css";

// Before the first paint, not in an effect: the stored design is read over IPC,
// and a first frame without `data-palette` renders as `warm` whatever the user
// picked. See applyCachedPalette().
applyCachedPalette();

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <ErrorBoundary>
      <BrowserRouter>
        <App />
      </BrowserRouter>
    </ErrorBoundary>
  </React.StrictMode>
);
