import { ipc, type Note } from "./ipc";
import { useNotesStore } from "./store";

export async function createNoteForRoute(pathname: string): Promise<Note> {
  const folderId = pathname.match(/^\/folder\/([^/]+)/)?.[1] ?? null;
  const note = await ipc.createNote();
  if (folderId && useNotesStore.getState().folders.some((f) => f.id === folderId)) {
    await ipc.moveNote(note.id, folderId);
    const moved = { ...note, folder_id: folderId };
    useNotesStore.getState().upsertLocal(moved);
    return moved;
  }
  useNotesStore.getState().upsertLocal(note);
  return note;
}
