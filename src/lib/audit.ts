import { ipc } from "./ipc";

/** Each organisation workspace, with whether opening a note there is recorded.
 *  Mirrors `OrgStatus.audit` (`org_audit::scope`). */
export type AuditScope = Record<string, boolean>;

type AuditedNote = { id: string; workspace_id?: string | null };

/** Whether `workspaceId`'s organisation records exports and copies. A Personal
 *  note (blank workspace) never is. */
export function recordsExports(scope: AuditScope, workspaceId: string | null | undefined): boolean {
  return !!workspaceId?.trim() && Object.prototype.hasOwnProperty.call(scope, workspaceId);
}

/** Whether opening a note in `workspaceId` is recorded (`audit_reads`). */
export function recordsOpens(scope: AuditScope, workspaceId: string | null | undefined): boolean {
  return recordsExports(scope, workspaceId) && scope[workspaceId as string] === true;
}

// The backend decides again from its own cache and debounces opens; checking
// here only spares a user in no organisation the call.

/** A note was shown; callers check [`recordsOpens`] first, once per note. */
export function reportOpened(noteId: string): void {
  void ipc.auditNoteOpened(noteId).catch(() => {});
}

export function reportCopied(scope: AuditScope, note: AuditedNote): void {
  if (recordsExports(scope, note.workspace_id)) void ipc.auditNoteCopied(note.id).catch(() => {});
}
