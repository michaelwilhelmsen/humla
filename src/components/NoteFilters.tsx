import { Building2, CircleDot, FolderOpen, User, X } from "lucide-react";
import { useShallow } from "zustand/react/shallow";
import { type Client } from "../lib/ipc";
import { useCloudStore } from "../lib/cloud";
import {
  NOTE_FILTER_LABEL,
  NO_FILTER,
  UNASSIGNED,
  filterActive,
  type NoteFilter,
  type NoteState,
} from "../lib/noteList";
import { cn } from "../lib/cn";
import { SelectablePopover, type PopoverItem } from "./SelectablePopover";

// The filter bar above the note grid: how far a note has got, whose client it
// is, who recorded it, and whether it is filed anywhere.

const STATES: NoteState[] = ["empty", "notes", "recorded", "transcribed", "summarized"];

export function NoteFilters({
  value,
  onChange,
  clients,
  showUnfiled,
}: {
  value: NoteFilter;
  onChange: (next: NoteFilter) => void;
  clients: Client[];
  /** False where the toggle can't narrow anything: a library with no folders,
   *  and a folder view, whose every note is filed here. */
  showUnfiled: boolean;
}) {
  // `useShallow` because `Object.values` builds a fresh array each call.
  const members = useCloudStore(useShallow((s) => Object.values(s.members)));
  const myId = useCloudStore((s) => s.status.user?.id ?? null);
  // A Personal library has one author, so the picker would offer a single name
  // that changes nothing.
  const showOwner = members.length > 1;
  const unfiled = value.folder === UNASSIGNED;

  return (
    <div className="flex flex-wrap items-center gap-1 -mx-2">
      <FilterPicker
        ariaLabel="Status"
        icon={<CircleDot size={14} strokeWidth={1.6} />}
        allLabel="Any status"
        items={STATES.map((s) => ({ id: s, label: NOTE_FILTER_LABEL[s] }))}
        active={value.state}
        onSelect={(state) => onChange({ ...value, state: state as NoteState | null })}
      />
      {clients.length > 0 && (
        <FilterPicker
          ariaLabel="Client"
          icon={<Building2 size={14} strokeWidth={1.6} />}
          allLabel="Any client"
          items={[
            { id: UNASSIGNED, label: "No client" },
            ...clients.map((c) => ({ id: c.id, label: c.name })),
          ]}
          active={value.client}
          onSelect={(client) => onChange({ ...value, client })}
        />
      )}
      {showOwner && (
        <FilterPicker
          ariaLabel="Created by"
          icon={<User size={14} strokeWidth={1.6} />}
          allLabel="Anyone"
          // `owner` is who RECORDED a note, not who attended it — the same
          // wording the chat pin carries, so the two read as one idea.
          items={members.map((m) => ({
            id: m.id,
            label: m.id === myId ? "Me" : m.name || m.email,
          }))}
          active={value.owner}
          onSelect={(owner) => onChange({ ...value, owner })}
        />
      )}
      {/* A toggle, not a picker: the sidebar already navigates to a folder, so
          the only reach a folder axis adds here is the notes filed nowhere. */}
      {showUnfiled && (
      <button
        type="button"
        aria-pressed={unfiled}
        className={cn("nd-meta is-interactive no-drag", unfiled && "is-selected")}
        onClick={() => onChange({ ...value, folder: unfiled ? null : UNASSIGNED })}
      >
        <FolderOpen size={14} strokeWidth={1.6} />
        No folder
      </button>
      )}
      {filterActive(value) && (
        <button
          type="button"
          className="nd-meta is-interactive no-drag"
          onClick={() => onChange(NO_FILTER)}
        >
          <X size={14} strokeWidth={1.8} />
          Clear
        </button>
      )}
    </div>
  );
}

function FilterPicker({
  ariaLabel,
  icon,
  allLabel,
  items,
  active,
  onSelect,
}: {
  ariaLabel: string;
  icon: React.ReactNode;
  /** The none row — selecting it drops this axis from the filter. */
  allLabel: string;
  items: PopoverItem[];
  active: string | null;
  onSelect: (id: string | null) => void;
}) {
  const label = (active ? items.find((i) => i.id === active)?.label : null) ?? allLabel;
  return (
    <SelectablePopover
      ariaLabel={ariaLabel}
      trigger={
        <span className={cn("nd-meta is-interactive no-drag", active !== null && "is-selected")}>
          {icon}
          <span className="truncate" style={{ maxWidth: 150 }}>{label}</span>
        </span>
      }
      items={items}
      activeId={active}
      onSelect={onSelect}
      noneLabel={allLabel}
    />
  );
}
