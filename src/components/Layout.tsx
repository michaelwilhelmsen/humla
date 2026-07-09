import { Outlet, useLocation } from "react-router-dom";
import { useEffect, useState } from "react";
import { PanelLeft } from "lucide-react";
import { Sidebar } from "./Sidebar";
import { TopBar } from "./TopBar";
import { Toaster } from "./Toaster";
import { Updater } from "./Updater";
import { PolishToast } from "./PolishToast";
import { ErrorBoundary } from "./ErrorBoundary";
import { bindBackendListeners } from "../lib/store";

// Passed down to routed pages via <Outlet context>. The Note toolbar uses it
// to inset its top-left content clear of the floating expand button + the
// macOS traffic lights when the sidebar is collapsed (no nav card to host them).
export type LayoutOutletContext = { sidebarCollapsed: boolean };

// Below this width (px) the sidebar + content fight for room and the
// right-hand column starts wrapping inside its rows. Auto-collapsing at
// this threshold keeps the UI from going claustrophobic.
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

  // Auto-collapse when the window is too narrow to fit sidebar + content.
  // (Settings no longer collapses anything — it renders as a dialog over
  // the shell, so Layout never even sees a /settings location.)
  const shouldAutoCollapse = viewportWidth < NARROW_VIEWPORT_PX;

  // Drop any manual override when the auto trigger flips. If the viewport
  // situation changes, we restart from the auto rule; the user can
  // re-collapse with the toggle if they want.
  useEffect(() => {
    setManualCollapsed(null);
  }, [shouldAutoCollapse]);

  const collapsed = manualCollapsed !== null ? manualCollapsed : shouldAutoCollapse;

  return (
    <div className="flex h-full p-1.5 gap-1.5 bg-[var(--color-canvas)]">
      {/* Left nav is an inset rounded card floating on the canvas. The macOS
          traffic lights land in its top-left corner; the Sidebar's own top
          padding clears them. When collapsed the card is removed entirely —
          a sliver would be narrower than the traffic-light row and they'd
          overflow it — and a floating expand button beside the window
          controls takes over. */}
      {!collapsed && (
        <aside className="shrink-0 w-64 overflow-hidden rounded-[var(--radius-card)] bg-[var(--color-sidebar-bg)] shadow-[var(--shadow-card)]">
          <Sidebar onCollapse={() => setManualCollapsed(true)} />
        </aside>
      )}
      <main className="flex-1 min-w-0 relative">
        {/* Drag strip on the main column too, so users can grab the
            window from anywhere along the title-bar zone, not just the
            sidebar. */}
        <div
          data-tauri-drag-region
          className="absolute top-0 left-0 right-0 h-9 z-20"
        />
        {/* Collapsed → expand toggle in the title-bar zone, just right of
            the traffic lights (Claude-desktop style). */}
        {collapsed && (
          <button
            type="button"
            onClick={() => setManualCollapsed(false)}
            data-tauri-drag-region="false"
            aria-label="Open sidebar"
            title="Open sidebar"
            className="no-drag absolute top-1.5 left-[72px] z-40 grid place-items-center w-9 h-9 rounded-full text-[var(--color-text-muted)] hover:bg-[var(--color-pill-hover)] hover:text-[var(--color-text)] transition-colors"
          >
            <PanelLeft size={17} strokeWidth={1.7} />
          </button>
        )}
        <TopBar />
        <ErrorBoundary key={location.pathname}>
          <Outlet context={{ sidebarCollapsed: collapsed }} />
        </ErrorBoundary>
      </main>
      <Toaster />
      <Updater />
      <PolishToast />
    </div>
  );
}
