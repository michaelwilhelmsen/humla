import { Link, useNavigate, useLocation } from "react-router-dom";
import { useMemo, useState } from "react";
import {
  ChevronLeft,
  ChevronRight,
  ChevronDown,
  Folder as FolderIcon,
  FolderPlus,
  Home as HomeIcon,
  Plus,
  Search,
  Settings as SettingsIcon,
  Trash2,
} from "lucide-react";
import { useNotesStore } from "../lib/store";
import { ipc, type Folder, type Note } from "../lib/ipc";
import { cn } from "../lib/cn";

function group(notes: Note[]) {
  const today = new Date(); today.setHours(0, 0, 0, 0);
  const yest = new Date(today); yest.setDate(today.getDate() - 1);
  const week = new Date(today); week.setDate(today.getDate() - 7);

  const groups: { label: string; items: Note[] }[] = [
    { label: "Today", items: [] },
    { label: "Yesterday", items: [] },
    { label: "Earlier this week", items: [] },
    { label: "Older", items: [] },
  ];
  for (const n of notes) {
    const d = new Date(n.updated_at);
    if (d >= today) groups[0].items.push(n);
    else if (d >= yest) groups[1].items.push(n);
    else if (d >= week) groups[2].items.push(n);
    else groups[3].items.push(n);
  }
  return groups.filter((g) => g.items.length);
}

export function Sidebar({ onCollapse }: { onCollapse: () => void }) {
  const navigate = useNavigate();
  const location = useLocation();
  const notes = useNotesStore((s) => s.notes);
  const folders = useNotesStore((s) => s.folders);
  const upsert = useNotesStore((s) => s.upsertLocal);
  const removeLocal = useNotesStore((s) => s.removeLocal);
  const upsertFolder = useNotesStore((s) => s.upsertFolder);
  const removeFolder = useNotesStore((s) => s.removeFolder);
  const [q, setQ] = useState("");
  // Folders are open by default; collapse state stored locally per session.
  const [collapsedFolders, setCollapsedFolders] = useState<Set<string>>(new Set());
  const [creatingFolder, setCreatingFolder] = useState(false);
  const [newFolderName, setNewFolderName] = useState("");

  function toggleFolder(id: string) {
    setCollapsedFolders((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  async function deleteNote(e: React.MouseEvent, id: string) {
    e.preventDefault();
    e.stopPropagation();
    await ipc.deleteNote(id);
    removeLocal(id);
    if (location.pathname === `/note/${id}`) navigate("/");
  }

  async function newNote() {
    const note = await ipc.createNote();
    upsert(note);
    navigate(`/note/${note.id}`);
  }

  async function commitNewFolder() {
    const name = newFolderName.trim();
    if (!name) {
      setCreatingFolder(false);
      setNewFolderName("");
      return;
    }
    try {
      const folder = await ipc.createFolder(name);
      upsertFolder(folder);
    } finally {
      setCreatingFolder(false);
      setNewFolderName("");
    }
  }

  async function deleteFolder(e: React.MouseEvent, id: string) {
    e.preventDefault();
    e.stopPropagation();
    // Notes fall back to root rather than being deleted — recoverable, so
    // skip the confirm.
    await ipc.deleteFolder(id);
    removeFolder(id);
  }

  const filteredNotes = useMemo(() => {
    if (!q.trim()) return notes;
    const needle = q.toLowerCase();
    return notes.filter(
      (n) =>
        n.title.toLowerCase().includes(needle) ||
        n.body.toLowerCase().includes(needle) ||
        n.transcript.toLowerCase().includes(needle)
    );
  }, [notes, q]);

  const rootNotes = filteredNotes.filter((n) => n.folder_id == null);
  const folderNotes = useMemo(() => {
    const map = new Map<string, Note[]>();
    for (const f of folders) map.set(f.id, []);
    for (const n of filteredNotes) {
      if (n.folder_id && map.has(n.folder_id)) map.get(n.folder_id)!.push(n);
    }
    // Within a folder, newest first.
    for (const arr of map.values()) arr.sort((a, b) => b.updated_at - a.updated_at);
    return map;
  }, [filteredNotes, folders]);

  const rootGrouped = group(rootNotes);
  const empty = folders.length === 0 && rootGrouped.length === 0;

  return (
    <div className="h-full flex flex-col p-3 gap-2">
      <div data-tauri-drag-region className="h-8 flex items-center justify-end">
        <button
          onClick={onCollapse}
          className="no-drag p-1.5 rounded-md hover:bg-[var(--color-pill-hover)] text-[var(--color-text-muted)] hover:text-[var(--color-text)]"
          aria-label="Collapse sidebar"
          title="⌘\"
        >
          <ChevronLeft size={16} strokeWidth={1.5} />
        </button>
      </div>

      <div className="no-drag flex items-center gap-2 pl-3 pr-2 py-2 rounded-md border border-[var(--color-line)] bg-[var(--color-surface)] focus-within:border-[var(--color-text-muted)] transition-colors">
        <Search size={14} strokeWidth={1.5} className="text-[var(--color-text-muted)] shrink-0" />
        <input
          data-search-input
          value={q}
          onChange={(e) => setQ(e.target.value)}
          placeholder="Search"
          className="flex-1 text-sm min-w-0"
        />
        <kbd
          className="shrink-0 px-1.5 py-0.5 text-[10px] text-[var(--color-text-muted)] border border-[var(--color-line)] rounded tracking-[0.04em]"
          style={{ fontFamily: "var(--font-mono)" }}
        >
          ⌘K
        </kbd>
      </div>

      <Link
        to="/"
        className={cn(
          "no-drag flex items-center gap-2 px-2 py-2 rounded-md text-sm transition-colors",
          location.pathname === "/"
            ? "bg-[var(--color-pill-hover)] text-[var(--color-text)]"
            : "text-[var(--color-text-muted)] hover:bg-[var(--color-pill-hover)] hover:text-[var(--color-text)]"
        )}
      >
        <HomeIcon size={15} strokeWidth={1.5} />
        <span>Home</span>
      </Link>

      <button
        onClick={newNote}
        className="no-drag flex items-center gap-2 px-2 py-2 rounded-md text-sm text-left text-[var(--color-text-muted)] hover:bg-[var(--color-pill-hover)] hover:text-[var(--color-text)] transition-colors"
        title="⌘N"
      >
        <Plus size={15} strokeWidth={1.5} />
        <span>New note</span>
      </button>

      {creatingFolder ? (
        <div className="no-drag flex items-center gap-2 pl-2 pr-2 py-2 rounded-md border border-[var(--color-text-muted)] bg-[var(--color-surface)]">
          <FolderPlus size={15} strokeWidth={1.5} className="text-[var(--color-text-muted)] shrink-0" />
          <input
            autoFocus
            value={newFolderName}
            onChange={(e) => setNewFolderName(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") commitNewFolder();
              else if (e.key === "Escape") {
                setCreatingFolder(false);
                setNewFolderName("");
              }
            }}
            onBlur={commitNewFolder}
            placeholder="Folder name"
            className="flex-1 text-sm min-w-0"
          />
        </div>
      ) : (
        <button
          onClick={() => setCreatingFolder(true)}
          className="no-drag flex items-center gap-2 px-2 py-2 rounded-md text-sm text-left text-[var(--color-text-muted)] hover:bg-[var(--color-pill-hover)] hover:text-[var(--color-text)] transition-colors"
        >
          <FolderPlus size={15} strokeWidth={1.5} />
          <span>New folder</span>
        </button>
      )}

      <div className="flex-1 overflow-y-auto -mx-1 px-1 mt-2">
        {empty && (
          <div className="px-2 py-4 text-sm text-[var(--color-text-muted)]">No notes yet</div>
        )}

        {folders.map((f) => {
          const items = folderNotes.get(f.id) ?? [];
          const collapsed = collapsedFolders.has(f.id);
          return (
            <FolderSection
              key={f.id}
              folder={f}
              items={items}
              collapsed={collapsed}
              onToggle={() => toggleFolder(f.id)}
              onDelete={(e) => deleteFolder(e, f.id)}
              onDeleteNote={deleteNote}
              activePath={location.pathname}
            />
          );
        })}

        {rootGrouped.map((g) => (
          <div key={g.label} className="mb-3">
            <div className="nd-label px-2 py-1.5">{g.label}</div>
            {g.items.map((n) => (
              <NoteRow
                key={n.id}
                note={n}
                active={location.pathname === `/note/${n.id}`}
                onDelete={deleteNote}
              />
            ))}
          </div>
        ))}
      </div>

      <Link
        to="/settings"
        className="no-drag flex items-center gap-2 px-2 py-2 rounded-md text-sm text-[var(--color-text-muted)] hover:bg-[var(--color-pill-hover)] hover:text-[var(--color-text)] transition-colors"
        title="⌘,"
      >
        <SettingsIcon size={15} strokeWidth={1.5} />
        <span>Settings</span>
      </Link>
    </div>
  );
}

function FolderSection({
  folder,
  items,
  collapsed,
  onToggle,
  onDelete,
  onDeleteNote,
  activePath,
}: {
  folder: Folder;
  items: Note[];
  collapsed: boolean;
  onToggle: () => void;
  onDelete: (e: React.MouseEvent) => void;
  onDeleteNote: (e: React.MouseEvent, id: string) => void;
  activePath: string;
}) {
  return (
    <div className="mb-3">
      <button
        onClick={onToggle}
        className="no-drag group w-full flex items-center gap-1 pl-1 pr-1 py-1.5 rounded-md text-left text-[var(--color-text-muted)] hover:bg-[var(--color-pill-hover)] hover:text-[var(--color-text)] transition-colors"
      >
        <span className="shrink-0 w-4 flex justify-center">
          {collapsed ? <ChevronRight size={12} strokeWidth={1.5} /> : <ChevronDown size={12} strokeWidth={1.5} />}
        </span>
        <FolderIcon size={13} strokeWidth={1.5} className="shrink-0" />
        <span className="flex-1 truncate text-xs uppercase tracking-[0.06em]" style={{ fontFamily: "var(--font-mono)" }}>
          {folder.name}
        </span>
        {items.length > 0 && (
          <span className="text-[10px] text-[var(--color-text-disabled)] tabular-nums" style={{ fontFamily: "var(--font-mono)" }}>
            {items.length}
          </span>
        )}
        <span
          role="button"
          aria-label="Delete folder"
          title="Delete folder"
          onClick={onDelete}
          className="opacity-0 group-hover:opacity-100 shrink-0 p-1 rounded text-[var(--color-text-muted)] hover:text-[var(--color-accent)] hover:bg-[var(--color-pill-hover)] transition-colors"
        >
          <Trash2 size={12} strokeWidth={1.5} />
        </span>
      </button>
      {!collapsed && items.length > 0 && (
        <div className="ml-4">
          {items.map((n) => (
            <NoteRow
              key={n.id}
              note={n}
              active={activePath === `/note/${n.id}`}
              onDelete={onDeleteNote}
            />
          ))}
        </div>
      )}
      {!collapsed && items.length === 0 && (
        <div className="ml-4 px-2 py-1.5 text-xs text-[var(--color-text-disabled)] italic">Empty</div>
      )}
    </div>
  );
}

function NoteRow({
  note,
  active,
  onDelete,
}: {
  note: Note;
  active: boolean;
  onDelete: (e: React.MouseEvent, id: string) => void;
}) {
  return (
    <Link
      to={`/note/${note.id}`}
      className={cn(
        "no-drag group flex items-center gap-1 pl-2 pr-1 py-2 rounded-md text-sm transition-colors",
        active
          ? "bg-[var(--color-pill-hover)] text-[var(--color-text)]"
          : "text-[var(--color-text-muted)] hover:bg-[var(--color-pill-hover)] hover:text-[var(--color-text)]"
      )}
    >
      <span className="flex-1 truncate">{note.title.trim() || "Untitled"}</span>
      <button
        onClick={(e) => onDelete(e, note.id)}
        aria-label="Delete note"
        title="Delete"
        className="opacity-0 group-hover:opacity-100 focus:opacity-100 shrink-0 p-1 rounded text-[var(--color-text-muted)] hover:text-[var(--color-accent)] hover:bg-[var(--color-pill-hover)] transition-colors"
      >
        <Trash2 size={14} strokeWidth={1.5} />
      </button>
    </Link>
  );
}
