// Two-pane settings layout with a vertical sidebar of tabs and a
// scrolling content area. Tabs are configured per-instance — the page
// just maps tab id → content component and lets the layout handle the
// chrome.
//
// Pattern follows macOS Ventura+ System Settings: vertical sidebar of
// labels with the active row highlighted, content fills the rest. Easier
// to scan than top tabs once the count grows past a couple, and matches
// what Mac users expect for app-level preferences.

import { useState, type ReactNode } from "react";

export type SettingsTab = {
  id: string;
  label: string;
  content: ReactNode;
};

export function SettingsLayout({
  tabs,
  defaultTabId,
}: {
  tabs: SettingsTab[];
  defaultTabId?: string;
}) {
  const [activeId, setActiveId] = useState<string>(defaultTabId ?? tabs[0]?.id ?? "");
  const active = tabs.find((t) => t.id === activeId) ?? tabs[0];

  return (
    <div className="h-full flex">
      <aside className="w-56 shrink-0 border-r border-[var(--color-line)] py-12 pl-6 pr-3">
        <h1 className="text-2xl font-light tracking-[-0.02em] mb-8 px-3">
          Settings
        </h1>
        <nav className="flex flex-col gap-0.5" role="tablist">
          {tabs.map((tab) => {
            const isActive = tab.id === active?.id;
            return (
              <button
                key={tab.id}
                role="tab"
                aria-selected={isActive}
                onClick={() => setActiveId(tab.id)}
                className={
                  "text-left px-3 py-1.5 rounded-md text-sm transition-colors " +
                  (isActive
                    ? "bg-[var(--color-pill-hover)] text-[var(--color-text)]"
                    : "text-[var(--color-text-muted)] hover:text-[var(--color-text)] hover:bg-[var(--color-pill-hover)]")
                }
              >
                {tab.label}
              </button>
            );
          })}
        </nav>
      </aside>
      <div className="flex-1 h-full overflow-y-auto">
        <div className="max-w-2xl mx-auto px-12 py-12" role="tabpanel">
          {active?.content}
        </div>
      </div>
    </div>
  );
}
