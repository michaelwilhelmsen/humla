import { Outlet, useLocation } from "react-router-dom";
import { useEffect, useState } from "react";
import { Sidebar } from "./Sidebar";
import { SidebarCollapsed } from "./SidebarCollapsed";
import { TopBar } from "./TopBar";
import { Toaster } from "./Toaster";
import { Updater } from "./Updater";
import { PolishToast } from "./PolishToast";
import { ErrorBoundary } from "./ErrorBoundary";
import { bindBackendListeners } from "../lib/store";
import { cn } from "../lib/cn";

// Below this width (px) the main app sidebar + Settings inner sidebar +
// content all fight for room and the right-hand column starts wrapping
// inside its rows. Auto-collapsing at this threshold keeps the UI from
// going claustrophobic. Keep in sync with the equivalent threshold in
// any future responsive Settings logic.
const NARROW_VIEWPORT_PX = 900;

export function Layout() {
  const location = useLocation();
  // null means "no manual override — follow the auto-collapse rule".
  // A boolean means the user clicked the toggle and we honour it until
  // the route or viewport situation changes again.
  const [manualCollapsed, setManualCollapsed] = useState<boolean | null>(null);
  const [viewportWidth, setViewportWidth] = useState(
    typeof window !== "undefined" ? window.innerWidth : 1200,
  );

  useEffect(() => {
    bindBackendListeners();
  }, []);

  useEffect(() => {
    const onResize = () => setViewportWidth(window.innerWidth);
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);

  // Auto-collapse when we're on Settings (the page already has its own
  // navigation chrome — the main sidebar adds clutter without value) or
  // when the window is too narrow to fit both sidebars + content.
  const onSettings = location.pathname.startsWith("/settings");
  const tooNarrow = viewportWidth < NARROW_VIEWPORT_PX;
  const shouldAutoCollapse = onSettings || tooNarrow;

  // Drop any manual override when the auto trigger flips. If the user
  // navigates away from Settings to a Note in a wide window, we restart
  // from auto = expanded; they can collapse again with the toggle if they
  // want. Same the other way around — entering Settings collapses,
  // overriding any prior manual choice.
  useEffect(() => {
    setManualCollapsed(null);
  }, [shouldAutoCollapse]);

  const collapsed = manualCollapsed !== null ? manualCollapsed : shouldAutoCollapse;

  return (
    <div className="flex h-full">
      <aside
        className={cn(
          "shrink-0 border-r border-[var(--color-line)] transition-[width] duration-200 overflow-hidden bg-[var(--color-sidebar-bg)]",
          collapsed ? "w-12" : "w-64",
        )}
      >
        {/* Each variant of the sidebar provides its own drag strip
            below — Sidebar via its h-8 header div, SidebarCollapsed
            via the dedicated drag area. Putting another absolute
            drag strip here would sit on top of Sidebar's collapse
            button and intercept clicks. */}
        {collapsed ? (
          <SidebarCollapsed onExpand={() => setManualCollapsed(false)} />
        ) : (
          <Sidebar onCollapse={() => setManualCollapsed(true)} />
        )}
      </aside>
      <main className="flex-1 min-w-0 relative">
        {/* Drag strip on the main column too, so users can grab the
            window from anywhere along the title-bar zone, not just the
            sidebar. */}
        <div
          data-tauri-drag-region
          className="absolute top-0 left-0 right-0 h-9 z-20"
        />
        <TopBar />
        <ErrorBoundary key={location.pathname}>
          <Outlet />
        </ErrorBoundary>
      </main>
      <Toaster />
      <Updater />
      <PolishToast />
    </div>
  );
}
