// Two-pane settings chrome inside the settings dialog: a sidebar of grouped
// section rows and a scrolling content area. `?tab=<id>` in the URL is the
// source of truth for the active section.

import type { ReactNode } from "react";
import type { LucideIcon } from "lucide-react";
import { useSearchParams } from "react-router-dom";
import { SECTION_GROUPS, resolveSectionId, type SectionId } from "./sections";

export type SettingsSection = {
  id: SectionId;
  label: string;
  icon: LucideIcon;
  content: ReactNode;
};

export function SettingsLayout({ sections }: { sections: SettingsSection[] }) {
  const [params, setParams] = useSearchParams();
  const activeId = resolveSectionId(params.get("tab"));
  const active = sections.find((s) => s.id === activeId) ?? sections[0];
  const groups = SECTION_GROUPS.map((g) => ({
    label: g.label,
    sections: g.sections.flatMap((spec) => sections.filter((s) => s.id === spec.id)),
  })).filter((g) => g.sections.length > 0);

  return (
    <div className="h-full flex">
      <aside className="w-56 shrink-0 bg-[var(--color-sidebar-bg)] p-6 overflow-y-auto">
        {/* No search field until the cross-section filter actually works —
            a dead search box promises more than the dialog delivers. */}
        <h1 className="text-2xl font-light tracking-[-0.02em] mb-4 px-3">
          Settings
        </h1>
        <nav className="flex flex-col" role="tablist" aria-orientation="vertical">
          {groups.map((group, gi) => (
            <div
              key={group.label}
              role="group"
              aria-label={group.label}
              className={gi > 0 ? "mt-4" : ""}
            >
              <div className="px-3 pb-1 text-xs font-medium text-[var(--color-text-muted)]">
                {group.label}
              </div>
              <div className="flex flex-col gap-0.5">
                {group.sections.map((section) => {
                  const isActive = section.id === active?.id;
                  const Icon = section.icon;
                  return (
                    <button
                      key={section.id}
                      type="button"
                      role="tab"
                      aria-selected={isActive}
                      onClick={() => setParams({ tab: section.id }, { replace: true })}
                      className={"nd-navrow text-left" + (isActive ? " is-active font-medium" : "")}
                    >
                      <Icon size={16} strokeWidth={1.6} aria-hidden className="shrink-0 opacity-85" />
                      <span className="truncate">{section.label}</span>
                    </button>
                  );
                })}
              </div>
            </div>
          ))}
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
