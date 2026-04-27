import { useEffect } from "react";
import { useNavigate } from "react-router-dom";
import { ipc } from "./ipc";
import { useNotesStore, useRecordingStore } from "./store";

export function useGlobalShortcuts() {
  const navigate = useNavigate();

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
        navigate("/settings");
      } else if (e.key === "r") {
        e.preventDefault();
        const { status } = useRecordingStore.getState();
        if (status.phase === "recording") {
          await ipc.recordingPause();
        } else if (status.phase === "paused") {
          await ipc.recordingResume();
        } else if (status.phase === "idle" && status.noteId === null) {
          const path = window.location.pathname;
          const match = path.match(/^\/note\/([^/]+)$/);
          if (match) await ipc.recordingStart(match[1]);
        }
      } else if (e.key === "k") {
        e.preventDefault();
        const el = document.querySelector<HTMLInputElement>("[data-search-input]");
        el?.focus();
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [navigate]);
}
