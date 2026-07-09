// Two-pane settings chrome inside the settings dialog: a vertical sidebar
// of section rows and a scrolling content area.
//
// `?tab=<id>` in the URL is the source of truth for the active section —
// legacy tab ids from old deep links resolve through resolveSectionId so
// none of them 404 (e.g. `?tab=keys` lands on Transcription).

import type { ReactNode } from "react";
import { useSearchParams } from "react-router-dom";
import { resolveSectionId, type SectionId } from "./sections";

export type SettingsSection = {
  id: SectionId;
  label: string;
  content: ReactNode;
};

export function SettingsLayout({ sections }: { sections: SettingsSection[] }) {
  const [params, setParams] = useSearchParams();
  const activeId = resolveSectionId(params.get("tab"));
  const active = sections.find((s) => s.id === activeId) ?? sections[0];

  return (
    <div className="h-full flex">
      <aside className="w-56 shrink-0 border-r border-[var(--color-line)] py-8 pl-6 pr-3">
        {/* No search field until the cross-section filter actually works —
            a dead search box promises more than the dialog delivers. */}
        <h1 className="text-2xl font-light tracking-[-0.02em] mb-6 px-3">
          Settings
        </h1>
        <nav className="flex flex-col gap-0.5" role="tablist">
          {sections.map((section) => {
            const isActive = section.id === active?.id;
            return (
              <button
                key={section.id}
                role="tab"
                aria-selected={isActive}
                onClick={() => setParams({ tab: section.id }, { replace: true })}
                className={
                  "text-left px-3 py-1.5 rounded-md text-sm transition-colors " +
                  (isActive
                    ? "bg-[var(--color-pill-hover)] text-[var(--color-text)]"
                    : "text-[var(--color-text-muted)] hover:text-[var(--color-text)] hover:bg-[var(--color-pill-hover)]")
                }
              >
                {section.label}
              </button>
            );
          })}
        </nav>
      </aside>
      <div className="flex-1 h-full overflow-y-auto">
        {/* Claude-Desktop-ish density: ~32px gutters, not the page-scale 48px
            the old full-screen settings used. */}
        <div className="max-w-2xl mx-auto px-8 py-7" role="tabpanel">
          {active?.content}
        </div>
      </div>
    </div>
  );
}
