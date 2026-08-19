import { useEffect } from "react";
import { useLocation, useNavigate } from "react-router-dom";
import { ipc, onToggleRecord } from "./ipc";
import { resolveHotkeyAction } from "./hotkey";
import { useNotesStore, useRecordingStore } from "./store";
import { useCloudStore } from "./cloud";
import { useZoomStore } from "./zoom";

// Is the note on this route one this user may record into? The Record button is
// hidden for viewers on a shared note, so every keyboard path has to be gated
// the same way.
function routeNoteIsReadOnly(noteId: string): boolean {
  const note = useNotesStore.getState().notes.find((n) => n.id === noteId);
  if (!note?.workspace_id) return false;
  const role = useCloudStore
    .getState()
    .status.workspaces.find((w) => w.id === note.workspace_id)?.role;
  return role === "viewer";
}

function routeNoteId(pathname: string): string | null {
  return pathname.match(/^\/note\/([^/]+)$/)?.[1] ?? null;
}

export function useGlobalShortcuts() {
  const navigate = useNavigate();
  const location = useLocation();

  useEffect(() => {
    async function onKey(e: KeyboardEvent) {
      const meta = e.metaKey || e.ctrlKey;
      if (!meta) return;

      if (e.key === "n" && !e.shiftKey) {
        e.preventDefault();
        const note = await ipc.createNote();
        useNotesStore.getState().upsertLocal(note);
        navigate(`/note/${note.id}`);
      } else if (e.key === ",") {
        e.preventDefault();
        // No-op while the settings dialog is already open — a second push
        // would stack /settings history entries and break "Esc closes".
        if (!location.pathname.startsWith("/settings")) navigate("/settings");
      } else if (e.key === "r") {
        e.preventDefault();
        const { status } = useRecordingStore.getState();
        if (status.phase === "recording") {
          await ipc.recordingPause();
        } else if (status.phase === "paused") {
          await ipc.recordingResume();
        } else if (status.phase === "idle" && status.noteId === null) {
          const id = routeNoteId(window.location.pathname);
          if (id && !routeNoteIsReadOnly(id)) await ipc.recordingStart(id);
        }
      } else if (e.key === "k") {
        e.preventDefault();
        const el = document.querySelector<HTMLInputElement>("[data-search-input]");
        el?.focus();
      } else if (e.key === "=" || e.key === "+") {
        e.preventDefault();
        await useZoomStore.getState().zoomIn();
      } else if (e.key === "-") {
        e.preventDefault();
        await useZoomStore.getState().zoomOut();
      } else if (e.key === "0") {
        e.preventDefault();
        await useZoomStore.getState().zoomReset();
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [navigate, location.pathname]);

  // The global record hotkey (#21), when it fires with the window on screen.
  // The backend hands it over rather than deciding, because what it should do
  // depends on the route — see `resolveHotkeyAction`.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    onToggleRecord(async () => {
      const { status } = useRecordingStore.getState();
      const routeId = routeNoteId(window.location.pathname);
      const action = resolveHotkeyAction({
        phase: status.phase,
        activeNoteId: status.noteId,
        routeNoteId: routeId,
        routeReadOnly: routeId ? routeNoteIsReadOnly(routeId) : false,
      });
      try {
        if (action.kind === "stop") {
          await ipc.recordingStop();
        } else if (action.kind === "start") {
          await ipc.recordingStart(action.noteId);
        } else if (action.kind === "startNew") {
          // Nothing open to record into, so make somewhere to put it — and go
          // there, since the window is visible and the user is watching.
          const note = await ipc.createNote();
          useNotesStore.getState().upsertLocal(note);
          navigate(`/note/${note.id}`);
          await ipc.recordingStart(note.id);
        }
      } catch (e) {
        useRecordingStore.getState().pushError({ noteId: null, message: String(e) });
      }
    }).then((u) => {
      // Tauri's listen() is async and can resolve after a StrictMode remount
      // has torn this effect down (see feedback_tauri_listen_strictmode).
      if (cancelled) u();
      else unlisten = u;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [navigate]);
}
