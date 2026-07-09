import { Link, useNavigate, useLocation } from "react-router-dom";
import { useMemo, useState } from "react";
import {
  ChevronLeft,
  FileAudio,
  Files,
  Folder as FolderIcon,
  FolderPlus,
  Home as HomeIcon,
  Plus,
  Search,
  Settings as SettingsIcon,
  Trash2,
} from "lucide-react";
import { open } from "@tauri-apps/plugin-dialog";
import { useNotesStore } from "../lib/store";
import { ipc, type Folder, type Note } from "../lib/ipc";
import { cn } from "../lib/cn";
import { ContextMenu, ContextMenuItem } from "./ContextMenu";
import { WorkspaceSwitcher } from "./WorkspaceSwitcher";
import { SetupNag } from "./SetupNag";
import { ImportDialog } from "./ImportDialog";
import { useCloudStore } from "../lib/cloud";

// Humla mark sourced from humla-small.svg — single-path silhouette of
// the bee's head + antennae arc. Uses currentColor so it inherits text
// color from its parent (set on the brand row).
function HumlaMark({ size = 18 }: { size?: number }) {
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
  const upsertLocal = useNotesStore((s) => s.upsertLocal);
  const upsertFolder = useNotesStore((s) => s.upsertFolder);
  const [q, setQ] = useState("");
  const [creatingFolder, setCreatingFolder] = useState(false);
  const [newFolderName, setNewFolderName] = useState("");
  // Path of the file awaiting an import-config dialog. Non-null while the
  // dialog is open, between picking the file and confirming language/speakers.
  const [importPath, setImportPath] = useState<string | null>(null);

  // Import an existing audio file. Step 1: pick the file. We then open a config
  // dialog (language + speakers) rather than importing immediately — the
  // transcription runs once and can't be re-run per language, so the language
  // must be chosen before it starts, not corrected on the note afterward.
  async function importAudio() {
    let selected: string | string[] | null;
    try {
      selected = await open({
        multiple: false,
        filters: [
          {
            name: "Audio",
            // Whatever AVFoundation decodes — the sidecar handles the rest.
            extensions: ["m4a", "mp3", "wav", "aac", "caf", "aiff", "aif", "m4b", "mp4"],
          },
        ],
      });
    } catch {
      return; // dialog unavailable / cancelled
    }
    if (typeof selected !== "string") return; // cancelled or multi (shouldn't happen)
    setImportPath(selected);
  }

  // Step 2: config confirmed. Create the note with the chosen language +
  // speaker hint and kick off the pipeline; navigate so the transcript fills
  // in live. Throws propagate to the dialog, which surfaces the error inline.
  async function confirmImport(language: string, expectedSpeakers: number | null) {
    const path = importPath;
    if (!path) return;
    const note = await ipc.importAudio(path, language, expectedSpeakers);
    upsertLocal(note);
    setImportPath(null);
    navigate(`/note/${note.id}`);
  }

  async function deleteNote(e: React.MouseEvent, id: string) {
    e.preventDefault();
    e.stopPropagation();
    await ipc.deleteNote(id);
    removeLocal(id);
    if (location.pathname === `/note/${id}`) navigate("/all-notes");
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
      .sort((a, b) => b.created_at - a.created_at);
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
    <div className="h-full flex flex-col px-2.5 pb-2.5">
      {/* Traffic-light clearance + window drag handle — the macOS lights
          sit over this strip in the nav card's top-left. */}
      <div data-tauri-drag-region className="h-[34px] w-full shrink-0" />

      {/* Brand */}
      <div
        data-tauri-drag-region
        className="flex items-center justify-between pl-2 pr-0 pb-3"
      >
        <div className="no-drag flex items-center gap-2 select-none text-[var(--color-text)]">
          <HumlaMark size={18} />
          <span className="text-[15px] font-semibold tracking-[-0.01em]">Humla</span>
        </div>
        <button
          onClick={onCollapse}
          data-tauri-drag-region="false"
          className="no-drag p-1.5 rounded-[var(--radius)] hover:bg-[var(--color-pill-hover)] text-[var(--color-text-muted)] hover:text-[var(--color-text)] transition-colors"
          aria-label="Collapse sidebar"
          title="Collapse sidebar"
        >
          <ChevronLeft size={16} strokeWidth={1.5} />
        </button>
      </div>

      <WorkspaceSwitcher />

      <div className="no-drag mt-2.5 flex items-center gap-2 px-2.5 h-9 rounded-[var(--radius)] border border-[var(--color-line-visible)] bg-[var(--color-surface)] focus-within:border-[var(--color-text-muted)] transition-colors">
        <Search size={14} strokeWidth={1.5} className="text-[var(--color-text-muted)] shrink-0" />
        <input
          data-search-input
          value={q}
          onChange={(e) => setQ(e.target.value)}
          placeholder="Search notes"
          className="flex-1 text-sm min-w-0 bg-transparent"
        />
        <kbd className="shrink-0 px-1.5 py-0.5 text-[10px] text-[var(--color-text-disabled)] border border-[var(--color-line-visible)] rounded tabular-nums">
          ⌘K
        </kbd>
      </div>

      {/* Scrollable nav body: primary links → Folders section (or search
          results when searching). */}
      <div className="flex-1 overflow-y-auto -mx-1 px-1 mt-3.5">
        <div className="flex flex-col gap-px">
          <NavItem to="/" icon={HomeIcon} label="Home" active={location.pathname === "/"} />
          <NavItem
            to="/all-notes"
            icon={Files}
            label="All notes"
            active={location.pathname === "/all-notes"}
            count={notes.length}
          />
          <button
            type="button"
            onClick={importAudio}
            title="Import an existing audio file as a new note"
            className="no-drag flex items-center gap-2.5 px-2.5 py-2 rounded-[var(--radius)] text-[13.5px] transition-colors text-[var(--color-text-muted)] hover:bg-[var(--color-pill-hover)] hover:text-[var(--color-text)]"
          >
            <FileAudio size={16} strokeWidth={1.6} className="shrink-0 opacity-85" />
            <span className="flex-1 truncate text-left">Import audio…</span>
          </button>
        </div>

        {searching ? (
          <div className="mt-3">
            {noResults && (
              <div className="px-2 py-4 text-sm text-[var(--color-text-muted)]">No matches</div>
            )}
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
            <Divider />
            <div className="nd-label flex items-center justify-between px-2 pb-1.5">
              <span>Folders</span>
              <button
                onClick={() => setCreatingFolder(true)}
                aria-label="New folder"
                title="New folder"
                className="grid place-items-center w-5 h-5 rounded-full text-[var(--color-text-muted)] hover:text-[var(--color-text)] hover:bg-[var(--color-pill-hover)] transition-colors"
              >
                <Plus size={14} strokeWidth={2} />
              </button>
            </div>

            {creatingFolder && (
              <div className="flex items-center gap-2 px-2 py-2 mb-0.5 rounded-[var(--radius)] border border-[var(--color-text-muted)] bg-[var(--color-surface)]">
                <FolderPlus size={15} strokeWidth={1.5} className="shrink-0 text-[var(--color-text-muted)]" />
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
                  className="flex-1 text-sm min-w-0 bg-transparent"
                />
              </div>
            )}

            {empty && !creatingFolder ? (
              <div className="px-2 py-4 text-sm text-[var(--color-text-muted)]">No notes yet</div>
            ) : folders.length === 0 && !creatingFolder ? (
              <div className="px-2 py-3 text-xs text-[var(--color-text-disabled)]">No folders yet</div>
            ) : (
              folders.map((f) => (
                <FolderRow
                  key={f.id}
                  folder={f}
                  count={folderCounts.get(f.id) ?? 0}
                  active={location.pathname === `/folder/${f.id}`}
                />
              ))
            )}
          </>
        )}
      </div>

      {/* Pinned footer */}
      <Divider />
      <NavItem to="/trash" icon={Trash2} label="Trash" active={location.pathname === "/trash"} />
      {/* Setup nag — renders only while the recording pipeline isn't functional
          (shared predicate). Sits above Settings; self-hides when all set. */}
      <SetupNag />
      <NavItem
        to="/settings"
        icon={SettingsIcon}
        label="Settings"
        active={location.pathname.startsWith("/settings")}
        title="⌘,"
      />
      <AccountRow />

      {importPath && (
        <ImportDialog
          path={importPath}
          onCancel={() => setImportPath(null)}
          onConfirm={confirmImport}
        />
      )}
    </div>
  );
}

// Hairline divider between nav groups. 0.5px reads as a crisp warm line on
// the card surface in both themes.
function Divider() {
  return <div className="h-px bg-[var(--color-line)] mx-1.5 my-3" />;
}

// A primary / footer nav row: leading icon, label, optional trailing count.
// Active = surface lift (sits above the card); hover = a faint tint.
function NavItem({
  to,
  icon: Icon,
  label,
  active,
  count,
  title,
}: {
  to: string;
  icon: typeof HomeIcon;
  label: string;
  active: boolean;
  count?: number;
  title?: string;
}) {
  return (
    <Link
      to={to}
      title={title}
      className={cn(
        "no-drag flex items-center gap-2.5 px-2.5 py-2 rounded-[var(--radius)] text-[13.5px] transition-colors",
        active
          ? "bg-[var(--color-sidebar-active)] text-[var(--color-text)] font-medium shadow-[0_1px_2px_rgba(0,0,0,0.05)]"
          : "text-[var(--color-text-muted)] hover:bg-[var(--color-pill-hover)] hover:text-[var(--color-text)]",
      )}
    >
      <Icon size={16} strokeWidth={1.6} className="shrink-0 opacity-85" />
      <span className="flex-1 truncate">{label}</span>
      {count !== undefined && count > 0 && (
        <span className="text-[11px] text-[var(--color-text-disabled)] tabular-nums">{count}</span>
      )}
    </Link>
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
    if (location.pathname === `/folder/${folder.id}`) navigate("/all-notes");
  }

  if (editing) {
    return (
      <div className="no-drag flex items-center gap-2 px-2.5 py-2 mb-0.5 rounded-[var(--radius)] border border-[var(--color-text-muted)] bg-[var(--color-surface)]">
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
          className="flex-1 text-sm min-w-0 bg-transparent"
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
          "no-drag group flex items-center gap-2.5 px-2.5 py-2 mb-0.5 rounded-[var(--radius)] text-[13.5px] transition-colors",
          active
            ? "bg-[var(--color-sidebar-active)] text-[var(--color-text)] font-medium shadow-[0_1px_2px_rgba(0,0,0,0.05)]"
            : "text-[var(--color-text-muted)] hover:bg-[var(--color-pill-hover)] hover:text-[var(--color-text)]",
        )}
      >
        <FolderIcon size={16} strokeWidth={1.6} className="shrink-0 opacity-85" />
        <span className="flex-1 truncate">{folder.name}</span>
        {count > 0 && (
          <span className="text-[11px] text-[var(--color-text-disabled)] tabular-nums">
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
        "no-drag group flex items-center gap-1 pl-2.5 pr-1 py-2 rounded-[var(--radius)] text-[13.5px] transition-colors",
        active
          ? "bg-[var(--color-sidebar-active)] text-[var(--color-text)]"
          : "text-[var(--color-text-muted)] hover:bg-[var(--color-pill-hover)] hover:text-[var(--color-text)]"
      )}
    >
      <span className="flex-1 min-w-0 flex flex-col">
        <span className="truncate">{note.title.trim() || "Untitled"}</span>
        {folderName && (
          <span className="truncate text-[10px] text-[var(--color-text-disabled)]">
            {folderName}
          </span>
        )}
      </span>
      <button
        onClick={(e) => onDelete(e, note.id)}
        aria-label="Delete note"
        title="Delete"
        className="opacity-0 group-hover:opacity-100 focus:opacity-100 shrink-0 p-1 rounded text-[var(--color-text-muted)] hover:text-[var(--color-danger)] hover:bg-[var(--color-pill-hover)] transition-colors"
      >
        <Trash2 size={14} strokeWidth={1.5} />
      </button>
    </Link>
  );
}

// Signed-in profile presence pinned at the bottom of the nav. Hidden when
// signed out (the workspace switcher already offers a sign-in affordance).
function AccountRow() {
  const status = useCloudStore((s) => s.status);
  if (!status.logged_in || !status.user) return null;
  const u = status.user;
  return (
    <Link
      to="/settings?tab=account"
      className="no-drag flex items-center gap-2.5 mt-1 px-2 py-2 rounded-[var(--radius)] hover:bg-[var(--color-pill-hover)] transition-colors"
      title="Account"
    >
      <span className="shrink-0 grid place-items-center w-6 h-6 rounded-full bg-[var(--color-surface-raised)] text-[11px] font-semibold text-[var(--color-text)]">
        {(u.name || u.email).slice(0, 1).toUpperCase()}
      </span>
      <span className="flex-1 min-w-0 flex flex-col leading-tight">
        <span className="truncate text-[13px] text-[var(--color-text)]">{u.name || u.email}</span>
        {u.name && (
          <span className="truncate text-[11px] text-[var(--color-text-disabled)]">{u.email}</span>
        )}
      </span>
    </Link>
  );
}
