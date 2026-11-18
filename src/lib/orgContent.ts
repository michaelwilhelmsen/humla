import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { ipc, type SummaryPrompt } from "./ipc";
import { useCloudStore } from "./cloud";
import type { AdminContent, AdminOrg, OrgSummary } from "./orgPolicy";
import { SUMMARY_PRESETS, presetLabel } from "./presets";

export function isOrgTemplate(t: SummaryPrompt): boolean {
  return !!t.org_id;
}

/** The server's `client_id` of an organisation template, from its row id
 *  (`org_policy::template_row_id`). */
export function orgTemplateClientId(t: SummaryPrompt): string | null {
  const prefix = `org_${t.org_id}_`;
  return t.org_id && t.id.startsWith(prefix) ? t.id.slice(prefix.length) : null;
}

/** Whether an organisation template is offered in `workspaceId`. In Personal
 *  ("") that is a template for every department; in a workspace, only one of
 *  its organisation's departments that the template's list (empty is all of
 *  them) includes. A team workspace outside the organisation gets none. */
export function offeredIn(t: SummaryPrompt, workspaceId: string, orgs: OrgSummary[]): boolean {
  const departments = t.org_workspaces ?? [];
  if (workspaceId === "") return departments.length === 0;
  const org = orgs.find((o) => o.id === t.org_id);
  if (!org?.workspaces.includes(workspaceId)) return false;
  return departments.length === 0 || departments.includes(workspaceId);
}

/** The templates a picker in `workspaceId` lists: the organisations' it is
 *  offered, and the user's own. */
export function pickerTemplates(
  all: SummaryPrompt[],
  workspaceId: string,
  orgs: OrgSummary[],
): { org: SummaryPrompt[]; own: SummaryPrompt[] } {
  return {
    org: all.filter((t) => isOrgTemplate(t) && offeredIn(t, workspaceId, orgs)),
    own: all.filter((t) => !isOrgTemplate(t)),
  };
}

export type TemplateOption = { value: string; label: string; separatorBefore?: boolean };

/** A template picker's options in `workspaceId`: the organisations' templates
 *  offered there, then the built-ins, then the user's own, with a divider
 *  before each group after the first. */
export function templateOptions(
  all: SummaryPrompt[],
  workspaceId: string,
  orgs: OrgSummary[],
): TemplateOption[] {
  const { org, own } = pickerTemplates(all, workspaceId, orgs);
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
  return useOrgsState() ?? [];
}

/** [`useOrgs`], but `null` until the first read has answered. */
export function useOrgsState(): OrgSummary[] | null {
  const [orgs, setOrgs] = useState<OrgSummary[] | null>(null);
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

/** The organisations the signed-in user administers, asked of the server.
 *  Signed out, offline or on any failure there are none, so nothing offers to
 *  edit what can't be saved. */
export function useAdminOrgs(): AdminOrg[] {
  const [orgs, setOrgs] = useState<AdminOrg[]>([]);
  const loggedIn = useCloudStore((s) => s.status.logged_in);
  const userId = useCloudStore((s) => s.status.user?.id ?? "");
  useEffect(() => {
    if (!loggedIn) {
      setOrgs([]);
      return;
    }
    let cancelled = false;
    ipc
      .orgAdminOrgs()
      .then((next) => !cancelled && setOrgs(next ?? []))
      .catch(() => !cancelled && setOrgs([]));
    return () => {
      cancelled = true;
    };
  }, [loggedIn, userId]);
  return orgs;
}

/** Of `orgs`, the ones the user can edit here: administered, and not waiting
 *  on the two-factor authentication the organisation requires (`locked`). */
export function editableOrgs(
  orgs: OrgSummary[],
  admin: AdminOrg[],
  twoFactorOn: boolean,
): { editable: OrgSummary[]; locked: OrgSummary[] } {
  const adminOf = (o: OrgSummary) => admin.find((a) => a.id === o.id);
  return {
    editable: orgs.filter((o) => {
      const a = adminOf(o);
      return a && (!a.require_2fa || twoFactorOn);
    }),
    locked: orgs.filter((o) => {
      const a = adminOf(o);
      return a && a.require_2fa && !twoFactorOn;
    }),
  };
}

/** An administered organisation the user is in no department of. */
export type AdminOnlyOrg = { summary: OrgSummary; content: AdminContent | null; error: string | null };

/** The organisations in `admin` that the cached config doesn't list, with their
 *  templates and terms read from the server for editing. They are kept in this
 *  hook's state only: nothing caches them, applies them or offers them in a
 *  picker. Nothing is asked until the config has been read (`orgs` non-null),
 *  and organisations waiting on two-factor authentication are left out. */
export function useAdminOnlyOrgs(
  orgs: OrgSummary[] | null,
  admin: AdminOrg[],
  twoFactorOn: boolean,
): { list: AdminOnlyOrg[]; reload: () => void } {
  const [content, setContent] = useState<Record<string, { content: AdminContent | null; error: string | null }>>({});
  const [round, setRound] = useState(0);
  const missing =
    orgs === null
      ? []
      : admin.filter((a) => !orgs.some((o) => o.id === a.id) && (!a.require_2fa || twoFactorOn));
  const key = missing.map((a) => a.id).join(",");
  useEffect(() => {
    if (!key) return;
    let cancelled = false;
    for (const id of key.split(",")) {
      ipc
        .orgAdminContent(id)
        .then((c) => !cancelled && setContent((prev) => ({ ...prev, [id]: { content: c, error: null } })))
        .catch((e) => !cancelled && setContent((prev) => ({ ...prev, [id]: { content: null, error: String(e) } })));
    }
    return () => {
      cancelled = true;
    };
  }, [key, round]);
  return {
    list: missing.map((a) => ({
      summary: { id: a.id, name: a.name, reason: "", vocabulary: content[a.id]?.content?.vocabulary ?? [], workspaces: [] },
      content: content[a.id]?.content ?? null,
      error: content[a.id]?.error ?? null,
    })),
    reload: () => setRound((r) => r + 1),
  };
}
