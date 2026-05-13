import { Link, useNavigate, useLocation } from "react-router-dom";
import { useMemo, useState } from "react";
import {
  ChevronLeft,
  Folder as FolderIcon,
  FolderPlus,
  Home as HomeIcon,
  Search,
  Settings as SettingsIcon,
  Trash2,
} from "lucide-react";
import { useNotesStore } from "../lib/store";
import { ipc, type Folder, type Note } from "../lib/ipc";
import { cn } from "../lib/cn";
import { ContextMenu, ContextMenuItem } from "./ContextMenu";

// Humla mark sourced from humla-small.svg — single-path silhouette of
// the bee's head + antennae arc. Uses currentColor so it inherits text
// color from its parent (set on the brand row).
function HumlaMark({ size = 16 }: { size?: number }) {
  return (
    <svg
      width={size}
      height={(size * 92) / 120}
      viewBox="0 0 120 92"
      aria-hidden="true"
      className="shrink-0"
    >
      <path
        fillRule="evenodd"
        clipRule="evenodd"
        d="M20.3123 1.16238C14.6123 4.36238 15.5123 7.96238 23.7123 15.6624C30.1123 21.5624 38.7123 32.4624 37.7123 33.3624C37.5123 33.4624 34.4123 35.1624 30.8123 37.0624C17.0123 44.3624 4.21234 61.7624 0.912338 77.7624C-0.787662 85.8624 -1.08766 85.4624 8.91234 87.3624C40.2123 93.3624 76.5123 93.5624 108.112 87.8624C121.712 85.3624 121.512 85.6624 117.712 73.3624C112.812 57.3624 104.212 46.1624 90.2123 37.6624C86.8123 35.6624 83.5123 33.9624 82.9123 33.9624C82.3123 33.9624 81.8123 33.5624 81.8123 33.0624C81.8123 31.0624 90.9123 19.9624 96.7123 14.9624C103.412 9.06238 104.512 5.66238 100.812 1.96238C97.6123 -1.23762 94.0123 -0.537622 89.5123 4.16238C85.3123 8.56238 74.4123 24.6624 73.3123 27.8624C72.8123 29.4624 71.5123 29.6624 60.2123 29.6624H47.7123L44.2123 23.5624C39.4123 15.4624 31.2123 4.46238 28.1123 1.96238C25.2123 -0.337619 23.2123 -0.537622 20.3123 1.16238Z"
        fill="currentColor"
      />
    </svg>
  );
}

export function Sidebar({ onCollapse }: { onCollapse: () => void }) {
  const navigate = useNavigate();
  const location = useLocation();
  const notes = useNotesStore((s) => s.notes);
  const folders = useNotesStore((s) => s.folders);
  const removeLocal = useNotesStore((s) => s.removeLocal);
  const upsertFolder = useNotesStore((s) => s.upsertFolder);
  const [q, setQ] = useState("");
  const [creatingFolder, setCreatingFolder] = useState(false);
  const [newFolderName, setNewFolderName] = useState("");

  async function deleteNote(e: React.MouseEvent, id: string) {
    e.preventDefault();
    e.stopPropagation();
    await ipc.deleteNote(id);
    removeLocal(id);
    if (location.pathname === `/note/${id}`) navigate("/");
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

  const needle = q.trim().toLowerCase();
  const searching = needle.length > 0;

  const noteMatches = (n: Note) =>
    n.title.toLowerCase().includes(needle) ||
    n.body.toLowerCase().includes(needle) ||
    n.transcript.toLowerCase().includes(needle);

  // When searching, surface a flat list of all matching notes regardless
  // of folder — folder context shows as a small chip on each row. When
  // not searching, folder rows + root note groups render normally.
  const searchResults = useMemo(() => {
    if (!searching) return [] as Note[];
    return notes
      .filter(noteMatches)
      .sort((a, b) => b.updated_at - a.updated_at);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [notes, needle]);

  const folderCounts = useMemo(() => {
    const map = new Map<string, number>();
    for (const n of notes) {
      if (n.folder_id) map.set(n.folder_id, (map.get(n.folder_id) ?? 0) + 1);
    }
    return map;
  }, [notes]);

  const folderById = useMemo(() => {
    const map = new Map<string, Folder>();
    for (const f of folders) map.set(f.id, f);
    return map;
  }, [folders]);

  const empty = folders.length === 0 && notes.length === 0;
  const noResults = searching && searchResults.length === 0;

  return (
    <div className="h-full flex flex-col px-3 pb-3 gap-2">
      {/* Title-bar-aligned drag strip — clears the macOS traffic-light row
          so the brand mark below never sits behind the window controls.
          Same pattern as SidebarCollapsed. */}
      <div data-tauri-drag-region className="h-9 w-full shrink-0" />
      <div data-tauri-drag-region className="h-8 flex items-center justify-between pl-1">
        <div className="no-drag flex items-center gap-2 select-none text-sm text-[var(--color-text-muted)]">
          <HumlaMark size={16} />
          <span>Humla</span>
        </div>
        <button
          onClick={onCollapse}
          data-tauri-drag-region="false"
          className="no-drag p-1.5 rounded-md hover:bg-[var(--color-sidebar-active)] text-[var(--color-text-muted)] hover:text-[var(--color-text)]"
          aria-label="Collapse sidebar"
          title="⌘\"
        >
          <ChevronLeft size={16} strokeWidth={1.5} />
        </button>
      </div>

      <div className="no-drag flex items-center gap-2 pl-2 pr-2 py-2 rounded-md border border-[var(--color-line)] bg-[var(--color-surface)] focus-within:border-[var(--color-text-muted)] transition-colors">
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
            ? "bg-[var(--color-sidebar-active)] text-[var(--color-text)]"
            : "text-[var(--color-text-muted)] hover:bg-[var(--color-sidebar-active)] hover:text-[var(--color-text)]"
        )}
      >
        <HomeIcon size={15} strokeWidth={1.5} />
        <span>Home</span>
      </Link>

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
          className="no-drag flex items-center gap-2 px-2 py-2 rounded-md text-sm text-left text-[var(--color-text-muted)] hover:bg-[var(--color-sidebar-active)] hover:text-[var(--color-text)] transition-colors"
        >
          <FolderPlus size={15} strokeWidth={1.5} />
          <span>New folder</span>
        </button>
      )}

      <div className="flex-1 overflow-y-auto -mx-1 px-1 mt-2">
        {empty && !searching && (
          <div className="px-2 py-4 text-sm text-[var(--color-text-muted)]">No notes yet</div>
        )}
        {noResults && (
          <div className="px-2 py-4 text-sm text-[var(--color-text-muted)]">No matches</div>
        )}

        {searching ? (
          <div className="mb-3">
            {searchResults.map((n) => (
              <NoteRow
                key={n.id}
                note={n}
                active={location.pathname === `/note/${n.id}`}
                onDelete={deleteNote}
                folderName={n.folder_id ? folderById.get(n.folder_id)?.name : undefined}
              />
            ))}
          </div>
        ) : (
          <>
            {folders.length > 0 && (
              <div className="nd-label px-2 pt-1 pb-1.5">Folders</div>
            )}

            {folders.map((f) => (
              <FolderRow
                key={f.id}
                folder={f}
                count={folderCounts.get(f.id) ?? 0}
                active={location.pathname === `/folder/${f.id}`}
              />
            ))}
          </>
        )}
      </div>

      <Link
        to="/settings"
        className="no-drag flex items-center gap-2 px-2 py-2 rounded-md text-sm text-[var(--color-text-muted)] hover:bg-[var(--color-sidebar-active)] hover:text-[var(--color-text)] transition-colors"
        title="⌘,"
      >
        <SettingsIcon size={15} strokeWidth={1.5} />
        <span>Settings</span>
      </Link>
    </div>
  );
}

function FolderRow({
  folder,
  count,
  active,
}: {
  folder: Folder;
  count: number;
  active: boolean;
}) {
  const navigate = useNavigate();
  const location = useLocation();
  const upsertFolder = useNotesStore((s) => s.upsertFolder);
  const removeFolder = useNotesStore((s) => s.removeFolder);
  const [menuPos, setMenuPos] = useState<{ x: number; y: number } | null>(null);
  const [editing, setEditing] = useState(false);
  const [draftName, setDraftName] = useState(folder.name);

  function openMenu(e: React.MouseEvent) {
    e.preventDefault();
    e.stopPropagation();
    setMenuPos({ x: e.clientX, y: e.clientY });
  }

  function startRename() {
    setMenuPos(null);
    setDraftName(folder.name);
    setEditing(true);
  }

  async function commitRename() {
    const name = draftName.trim();
    if (!name || name === folder.name) {
      setEditing(false);
      return;
    }
    try {
      await ipc.renameFolder(folder.id, name);
      upsertFolder({ ...folder, name, updated_at: Date.now() });
    } finally {
      setEditing(false);
    }
  }

  async function deleteHere() {
    setMenuPos(null);
    // Notes fall back to root rather than being deleted — recoverable
    // so no confirm needed. If we're sitting on this folder's page,
    // navigate home so we don't end up on a dead route.
    await ipc.deleteFolder(folder.id);
    removeFolder(folder.id);
    if (location.pathname === `/folder/${folder.id}`) navigate("/");
  }

  if (editing) {
    return (
      <div className="no-drag flex items-center gap-2 px-2 py-2 mb-0.5 rounded-md border border-[var(--color-text-muted)] bg-[var(--color-surface)]">
        <FolderIcon size={15} strokeWidth={1.5} className="shrink-0 text-[var(--color-text-muted)]" />
        <input
          autoFocus
          value={draftName}
          onChange={(e) => setDraftName(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") commitRename();
            else if (e.key === "Escape") setEditing(false);
          }}
          onBlur={commitRename}
          className="flex-1 text-sm min-w-0"
        />
      </div>
    );
  }

  return (
    <>
      <Link
        to={`/folder/${folder.id}`}
        onContextMenu={openMenu}
        className={cn(
          "no-drag group flex items-center gap-2 px-2 py-2 mb-0.5 rounded-md text-sm transition-colors",
          active
            ? "bg-[var(--color-sidebar-active)] text-[var(--color-text)]"
            : "text-[var(--color-text-muted)] hover:bg-[var(--color-sidebar-active)] hover:text-[var(--color-text)]",
        )}
      >
        <FolderIcon size={15} strokeWidth={1.5} className="shrink-0" />
        <span className="flex-1 truncate">{folder.name}</span>
        {count > 0 && (
          <span
            className="text-[11px] text-[var(--color-text-disabled)] tabular-nums"
            style={{ fontFamily: "var(--font-mono)" }}
          >
            {count}
          </span>
        )}
      </Link>
      {menuPos && (
        <ContextMenu x={menuPos.x} y={menuPos.y} onClose={() => setMenuPos(null)}>
          <ContextMenuItem onClick={startRename}>Rename</ContextMenuItem>
          <ContextMenuItem onClick={deleteHere} danger>
            Delete
          </ContextMenuItem>
        </ContextMenu>
      )}
    </>
  );
}

function NoteRow({
  note,
  active,
  onDelete,
  folderName,
}: {
  note: Note;
  active: boolean;
  onDelete: (e: React.MouseEvent, id: string) => void;
  folderName?: string;
}) {
  return (
    <Link
      to={`/note/${note.id}`}
      className={cn(
        "no-drag group flex items-center gap-1 pl-2 pr-1 py-2 rounded-md text-sm transition-colors",
        active
          ? "bg-[var(--color-sidebar-active)] text-[var(--color-text)]"
          : "text-[var(--color-text-muted)] hover:bg-[var(--color-sidebar-active)] hover:text-[var(--color-text)]"
      )}
    >
      <span className="flex-1 min-w-0 flex flex-col">
        <span className="truncate">{note.title.trim() || "Untitled"}</span>
        {folderName && (
          <span className="truncate text-[10px] text-[var(--color-text-disabled)] uppercase tracking-[0.06em]" style={{ fontFamily: "var(--font-mono)" }}>
            {folderName}
          </span>
        )}
      </span>
      <button
        onClick={(e) => onDelete(e, note.id)}
        aria-label="Delete note"
        title="Delete"
        className="opacity-0 group-hover:opacity-100 focus:opacity-100 shrink-0 p-1 rounded text-[var(--color-text-muted)] hover:text-[var(--color-accent)] hover:bg-[var(--color-sidebar-active)] transition-colors"
      >
        <Trash2 size={14} strokeWidth={1.5} />
      </button>
    </Link>
  );
}
