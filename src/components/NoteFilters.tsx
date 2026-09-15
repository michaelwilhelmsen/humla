import { Building2, CircleDot, Folder as FolderIcon, User, X } from "lucide-react";
import { useShallow } from "zustand/react/shallow";
import { type Client, type Folder } from "../lib/ipc";
import { useCloudStore } from "../lib/cloud";
import {
  NOTE_STATE_LABEL,
  UNASSIGNED,
  filterActive,
  type NoteFilter,
  type NoteState,
} from "../lib/noteList";
import { SelectablePopover, type PopoverItem } from "./SelectablePopover";

// The filter bar above the note grid: one picker per axis a library is
// actually searched along — how far a note has got, whose client it is, who
// recorded it, which folder it sits in.
//
// Each axis is its own picker rather than a row of chips because the axes are
// independent and two of them (Client, Folder) are unbounded lists. The
// picker's "All …" row is the SelectablePopover none row, so clearing one axis
// is the same gesture as clearing any other.

const STATES: NoteState[] = ["empty", "notes", "recorded", "transcribed", "summarized"];

export function NoteFilters({
  value,
  onChange,
  clients,
  folders,
}: {
  value: NoteFilter;
  onChange: (next: NoteFilter) => void;
  clients: Client[];
  folders: Folder[];
}) {
  // Owner is a workspace question: a Personal library has one author, so the
  // picker would offer a single name that changes nothing.
  // `useShallow` because `Object.values` builds a fresh array each call.
  const members = useCloudStore(useShallow((s) => Object.values(s.members)));
  const myId = useCloudStore((s) => s.status.user?.id ?? null);
  const showOwner = members.length > 1;

  return (
    <div className="flex flex-wrap items-center gap-1 -mx-2">
      <FilterPicker
        ariaLabel="Status"
        icon={<CircleDot size={14} strokeWidth={1.6} />}
        allLabel="Any status"
        items={STATES.map((s) => ({ id: s, label: NOTE_STATE_LABEL[s] }))}
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
          items={members.map((m) => ({
            id: m.id,
            // "Created by me", not "My notes" — `owner` is who RECORDED the
            // note, not who attended it. Same wording as the chat pin (#103),
            // for the same reason: the false negative stays visible.
            label: m.id === myId ? "Me" : m.name || m.email,
          }))}
          active={value.owner}
          onSelect={(owner) => onChange({ ...value, owner })}
        />
      )}
      {folders.length > 0 && (
        <FilterPicker
          ariaLabel="Folder"
          icon={<FolderIcon size={14} strokeWidth={1.6} />}
          allLabel="Any folder"
          items={[
            { id: UNASSIGNED, label: "No folder" },
            ...folders.map((f) => ({ id: f.id, label: f.name })),
          ]}
          active={value.folder}
          onSelect={(folder) => onChange({ ...value, folder })}
        />
      )}
      {filterActive(value) && (
        <button
          type="button"
          className="nd-meta is-interactive no-drag"
          onClick={() => onChange({ state: null, client: null, folder: null, owner: null })}
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
        <span
          className="nd-meta is-interactive no-drag"
          // An active filter is hiding notes, so it must not read as the
          // resting state. The fill carries that, not the label.
          style={
            active !== null
              ? { background: "var(--color-accent-soft)", color: "var(--color-accent-text)" }
              : undefined
          }
        >
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
