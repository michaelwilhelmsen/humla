import { Outlet, useLocation } from "react-router-dom";
import { useEffect, useState } from "react";
import { Sidebar } from "./Sidebar";
import { Toaster } from "./Toaster";
import { ErrorBoundary } from "./ErrorBoundary";
import { bindBackendListeners } from "../lib/store";
import { cn } from "../lib/cn";

export function Layout() {
  const [collapsed, setCollapsed] = useState(false);
  const location = useLocation();

  useEffect(() => {
    bindBackendListeners();
  }, []);

  return (
    <div className="flex h-full">
      <aside
        className={cn(
          "shrink-0 border-r border-[var(--color-line)] transition-[width] duration-200 overflow-hidden",
          collapsed ? "w-0" : "w-64"
        )}
      >
        <Sidebar onCollapse={() => setCollapsed(true)} />
      </aside>
      <main className="flex-1 min-w-0 relative">
        {/* Window drag strip — Tauri 2's native attribute, not the CSS region.
            Always present at the top of the main area so the window can be
            grabbed from any route, and tall enough to clear the traffic-light
            hot zone. */}
        <div
          data-tauri-drag-region
          className="absolute top-0 left-0 right-0 h-9 z-20"
        />
        {collapsed && (
          <button
            onClick={() => setCollapsed(false)}
            className="no-drag absolute top-3 left-3 z-30 px-2 py-1 rounded-md hover:bg-[var(--color-pill-hover)] text-[var(--color-text-muted)]"
            aria-label="Open sidebar"
          >
            ☰
          </button>
        )}
        <ErrorBoundary key={location.pathname}>
          <Outlet />
        </ErrorBoundary>
      </main>
      <Toaster />
    </div>
  );
}
