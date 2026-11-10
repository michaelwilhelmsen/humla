import { useState } from "react";
import { Building2, Check, Lock, Search, Users, X } from "lucide-react";
import { Popover, PopoverContent, PopoverTrigger } from "./ui/Popover";
import { useCloudStore, type CloudMember } from "../lib/cloud";
import { accessLabel, addableMembers, withMember, withoutMember, type NoteAccess } from "../lib/noteAccess";
import { cn } from "../lib/cn";

/** Who can read a note inside its workspace (#191, #232), and — for its author —
 *  the place to change it. Anyone else sees the same list without the controls:
 *  the server refuses a non-author's change either way. */
export function NoteAccessPicker({
  access,
  owner,
  editable,
  disabled,
  onChange,
}: {
  access: NoteAccess;
  owner: string;
  editable: boolean;
  disabled?: boolean;
  onChange: (next: NoteAccess) => void;
}) {
  const members = useCloudStore((s) => s.members);
  const me = useCloudStore((s) => s.status.user);
  const workspace = useCloudStore((s) => s.status.current_workspace?.name);
  const [query, setQuery] = useState("");

  // The roster may leave out the caller, so the caller's own row falls back to
  // their account.
  const known: Record<string, CloudMember> =
    me && !members[me.id] ? { ...members, [me.id]: { id: me.id, name: me.name, email: me.email, role: "member" } } : members;
  const ownerId = owner || me?.id || "";
  const label = accessLabel(access, ownerId, me?.id, known, workspace);
  const matches = editable && query.trim() ? addableMembers(known, ownerId, access, query) : [];
  const Icon = !access.private ? Building2 : access.shared_with.length ? Users : Lock;

  function add(id: string) {
    onChange(withMember(access, id));
    setQuery("");
  }

  return (
    <Popover onOpenChange={(open) => !open && setQuery("")}>
      <PopoverTrigger
        className="nd-meta is-interactive"
        disabled={disabled}
        aria-label="Who can read this note"
        title="Who can read this note"
      >
        <Icon size={14} strokeWidth={1.7} />
        <span>{label}</span>
      </PopoverTrigger>
      <PopoverContent aria-label="Who can read this note" className="w-[300px] p-3" maxHeight={460}>
        {editable && (
          <div className="mb-3">
            <label className="flex items-center gap-2 px-2.5 py-1.5 rounded-md border border-[var(--color-line-visible)] text-sm">
              <Search size={14} strokeWidth={1.7} aria-hidden className="shrink-0 text-[var(--color-text-muted)]" />
              <input
                aria-label="Add people"
                className="nd-bare flex-1 min-w-0 bg-transparent outline-none"
                placeholder={workspace ? `Add people from ${workspace}` : "Add people"}
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && matches[0]) {
                    e.preventDefault();
                    add(matches[0].id);
                  }
                }}
              />
            </label>
            {matches.length > 0 && (
              <ul aria-label="Matching members" className="mt-1">
                {matches.map((m) => (
                  <li key={m.id}>
                    <button
                      type="button"
                      className="flex w-full items-center gap-2 px-2 py-1.5 rounded-md text-sm text-left hover:bg-[var(--color-pill-hover)]"
                      onClick={() => add(m.id)}
                    >
                      <Avatar member={m} />
                      <span className="truncate">{m.name || m.email}</span>
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </div>
        )}

        <div className="nd-label mb-1">People with access</div>
        <ul aria-label="People with access">
          <PersonRow member={known[ownerId]} you={ownerId === me?.id} trailing="Owner" />
          {access.private ? (
            access.shared_with.map((id) => (
              <PersonRow
                key={id}
                member={known[id]}
                you={id === me?.id}
                trailing={
                  editable ? (
                    <button
                      type="button"
                      className="nd-btn-icon-sm"
                      aria-label={`Remove ${known[id]?.name || known[id]?.email || "this person"}`}
                      onClick={() => onChange(withoutMember(access, id))}
                    >
                      <X size={14} strokeWidth={1.7} />
                    </button>
                  ) : null
                }
              />
            ))
          ) : (
            <li className="flex items-center gap-2 py-1.5 text-sm">
              <span className="grid place-items-center w-6 h-6 rounded-full shrink-0 bg-[var(--color-pill-hover)]">
                <Users size={13} strokeWidth={1.7} aria-hidden />
              </span>
              <span className="truncate">
                Everyone in {workspace ?? "the workspace"}
                <span className="text-[var(--color-text-muted)]"> · {Object.keys(known).length} people</span>
              </span>
            </li>
          )}
        </ul>

        <div className="mt-3 pt-3 border-t border-[var(--color-line)]">
          <div className="nd-label mb-1">General access</div>
          {editable ? (
            <div role="radiogroup" aria-label="General access">
              <GeneralOption
                checked={!access.private}
                icon={<Building2 size={14} strokeWidth={1.7} aria-hidden />}
                label={`Everyone in ${workspace ?? "the workspace"}`}
                hint="All members can find and open it"
                onSelect={() => onChange({ private: false, shared_with: [] })}
              />
              <GeneralOption
                checked={access.private}
                icon={<Lock size={14} strokeWidth={1.7} aria-hidden />}
                label="Only people added"
                hint="Others in the workspace can't see it"
                onSelect={() => onChange({ private: true, shared_with: access.shared_with })}
              />
            </div>
          ) : (
            <p className="text-sm">
              {access.private ? "Only people added" : `Everyone in ${workspace ?? "the workspace"}`}
            </p>
          )}
          {editable && access.private && access.shared_with.length > 0 && (
            <p className="mt-2 text-xs text-[var(--color-text-muted)]">
              Anyone you remove loses their copy on their next sync.
            </p>
          )}
        </div>
      </PopoverContent>
    </Popover>
  );
}

function Avatar({ member }: { member: CloudMember | undefined }) {
  const initial = (member?.name || member?.email || "?").trim().charAt(0).toUpperCase();
  return (
    <span
      aria-hidden
      className="grid place-items-center w-6 h-6 rounded-full shrink-0 text-[11px] font-semibold"
      style={{ background: "var(--color-surface-raised)", color: "var(--color-text)" }}
    >
      {initial}
    </span>
  );
}

function PersonRow({
  member,
  you,
  trailing,
}: {
  member: CloudMember | undefined;
  you: boolean;
  trailing?: React.ReactNode;
}) {
  return (
    <li className="flex items-center gap-2 py-1.5 text-sm">
      <Avatar member={member} />
      <span className="flex-1 min-w-0 truncate">
        {member ? member.name || member.email : "Former member"}
        {you && <span className="text-[var(--color-text-muted)]"> (you)</span>}
      </span>
      {typeof trailing === "string" ? (
        <span className="text-xs text-[var(--color-text-muted)]">{trailing}</span>
      ) : (
        trailing
      )}
    </li>
  );
}

function GeneralOption({
  checked,
  icon,
  label,
  hint,
  onSelect,
}: {
  checked: boolean;
  icon: React.ReactNode;
  label: string;
  hint: string;
  onSelect: () => void;
}) {
  return (
    <button
      type="button"
      role="radio"
      aria-checked={checked}
      onClick={onSelect}
      className={cn(
        "flex w-full items-start gap-2 px-2 py-1.5 rounded-md text-left hover:bg-[var(--color-pill-hover)]",
      )}
    >
      <span className="mt-0.5 shrink-0">{icon}</span>
      <span className="flex-1 min-w-0">
        <span className="block text-sm">{label}</span>
        <span className="block text-xs text-[var(--color-text-muted)]">{hint}</span>
      </span>
      {checked && <Check size={14} strokeWidth={2} aria-hidden className="mt-0.5 shrink-0 text-[var(--color-accent-text)]" />}
    </button>
  );
}
