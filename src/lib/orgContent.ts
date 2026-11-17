import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { ipc, type SummaryPrompt } from "./ipc";
import { useCloudStore } from "./cloud";
import type { OrgSummary } from "./orgPolicy";
import { SUMMARY_PRESETS, presetLabel } from "./presets";

export function isOrgTemplate(t: SummaryPrompt): boolean {
  return !!t.org_id;
}

/** Whether an organisation template is offered in `workspaceId` ("" is
 *  Personal): its department list is empty or names that workspace. */
export function offeredIn(t: SummaryPrompt, workspaceId: string): boolean {
  const departments = t.org_workspaces ?? [];
  return departments.length === 0 || departments.includes(workspaceId);
}

/** The templates a picker in `workspaceId` lists: the organisations' it is
 *  offered, and the user's own. */
export function pickerTemplates(
  all: SummaryPrompt[],
  workspaceId: string,
): { org: SummaryPrompt[]; own: SummaryPrompt[] } {
  return {
    org: all.filter((t) => isOrgTemplate(t) && offeredIn(t, workspaceId)),
    own: all.filter((t) => !isOrgTemplate(t)),
  };
}

export type TemplateOption = { value: string; label: string; separatorBefore?: boolean };

/** A template picker's options in `workspaceId`: the organisations' templates
 *  offered there, then the built-ins, then the user's own, with a divider
 *  before each group after the first. */
export function templateOptions(all: SummaryPrompt[], workspaceId: string): TemplateOption[] {
  const { org, own } = pickerTemplates(all, workspaceId);
  return [
    ...org.map((t) => ({ value: `custom:${t.id}`, label: t.name })),
    ...SUMMARY_PRESETS.map((p, i) => ({
      value: p.value,
      label: presetLabel(p),
      ...(i === 0 && org.length > 0 ? { separatorBefore: true } : {}),
    })),
    ...own.map((t, i) => ({ value: `custom:${t.id}`, label: t.name, ...(i === 0 ? { separatorBefore: true } : {}) })),
  ];
}

/** Every organisation's terms, without duplicates regardless of case. */
export function orgTerms(orgs: OrgSummary[]): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const raw of orgs.flatMap((o) => o.vocabulary)) {
    const term = raw.trim();
    const key = term.toLowerCase();
    if (!term || seen.has(key)) continue;
    seen.add(key);
    out.push(term);
  }
  return out;
}

/** The active workspace's id; "" in Personal. */
export function useActiveWorkspaceId(): string {
  return useCloudStore((s) => s.status.current_workspace?.id ?? s.status.locked_current_workspace?.id ?? "");
}

/** Counts `org_content_changed` events, so a list can reload when an
 *  organisation's templates or terms change. */
export function useOrgContentVersion(): number {
  const [version, setVersion] = useState(0);
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    void listen("org_content_changed", () => setVersion((v) => v + 1)).then((u) => {
      if (cancelled) u();
      else unlisten = u;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);
  return version;
}

/** The organisations whose templates and terms apply here. A failed read
 *  shows none. */
export function useOrgs(): OrgSummary[] {
  const [orgs, setOrgs] = useState<OrgSummary[]>([]);
  const cloudStatus = useCloudStore((s) => s.status);
  const version = useOrgContentVersion();
  useEffect(() => {
    let cancelled = false;
    ipc
      .orgStatus()
      .then((s) => !cancelled && setOrgs(s?.orgs ?? []))
      .catch(() => !cancelled && setOrgs([]));
    return () => {
      cancelled = true;
    };
  }, [cloudStatus, version]);
  return orgs;
}
