import type { CloudMember } from "./cloud";

// Who can read a note inside its workspace (#191, #232): everyone, or its author
// plus the members in `shared_with`. The Rust side (`notes_set_access`) keeps the
// list free of the author and empty on a note everyone can read.

export type NoteAccess = { private: boolean; shared_with: string[] };

/** A member's first name, for a chip that has room for two. */
export function firstName(member: Pick<CloudMember, "name" | "email"> | undefined): string {
  if (!member) return "Someone";
  const name = member.name.trim();
  if (name) return name.split(/\s+/)[0];
  return member.email.split("@")[0] || "Someone";
}

/**
 * What the meta-bar chip reads: the workspace's name when everyone can read the
 * note, "Only you" when its author alone can, and otherwise the people who can —
 * the author included — by first name up to two, then as a count.
 */
export function accessLabel(
  access: NoteAccess,
  owner: string,
  me: string | undefined,
  members: Record<string, CloudMember>,
  workspaceName: string | undefined,
): string {
  if (!access.private) return workspaceName ?? "Workspace";
  const people = [owner, ...access.shared_with.filter((id) => id !== owner)].filter(Boolean);
  if (people.length <= 1) return !owner || owner === me ? "Only you" : firstName(members[owner]);
  if (people.length === 2) return people.map((id) => firstName(members[id])).join(", ");
  return `${people.length} people`;
}

/**
 * Members who could be added: everyone in the roster who doesn't already have
 * access, matching `query` against name or email. An empty query matches all.
 */
export function addableMembers(
  members: Record<string, CloudMember>,
  owner: string,
  access: NoteAccess,
  query: string,
): CloudMember[] {
  const q = query.trim().toLowerCase();
  const has = new Set([owner, ...(access.private ? access.shared_with : [])]);
  return Object.values(members)
    .filter((m) => !has.has(m.id))
    .filter((m) => !q || m.name.toLowerCase().includes(q) || m.email.toLowerCase().includes(q))
    .sort((a, b) => (a.name || a.email).localeCompare(b.name || b.email));
}

/** Give one more member access. Adding to a note everyone reads restricts it. */
export function withMember(access: NoteAccess, id: string): NoteAccess {
  const current = access.private ? access.shared_with : [];
  return { private: true, shared_with: current.includes(id) ? current : [...current, id] };
}

export function withoutMember(access: NoteAccess, id: string): NoteAccess {
  return { private: true, shared_with: access.shared_with.filter((x) => x !== id) };
}
