import { create } from "zustand";
import { ipc, onRecordingDiagnostic, onRecordingError, onRecordingStatus, onSummary, onTranscript, onTranscriptReplaced, type Folder, type Note, type RecordingDiagnostic, type RecordingStatus } from "./ipc";

type NotesState = {
  notes: Note[];
  folders: Folder[];
  refresh: () => Promise<void>;
  refreshFolders: () => Promise<void>;
  upsertLocal: (note: Note) => void;
  upsertFolder: (folder: Folder) => void;
  removeFolder: (id: string) => void;
  appendTranscript: (id: string, text: string) => void;
  replaceTranscript: (id: string, text: string) => void;
  setSummary: (id: string, summary: string) => void;
  removeLocal: (id: string) => void;
};

export const useNotesStore = create<NotesState>((set) => ({
  notes: [],
  folders: [],
  refresh: async () => {
    const [notes, folders] = await Promise.all([ipc.listNotes(), ipc.listFolders()]);
    set({ notes, folders });
  },
  refreshFolders: async () => {
    const folders = await ipc.listFolders();
    set({ folders });
  },
  upsertFolder: (folder) =>
    set((s) => {
      const idx = s.folders.findIndex((f) => f.id === folder.id);
      if (idx === -1) return { folders: [...s.folders, folder] };
      const next = s.folders.slice();
      next[idx] = folder;
      return { folders: next };
    }),
  removeFolder: (id) =>
    set((s) => ({
      folders: s.folders.filter((f) => f.id !== id),
      // Notes in the deleted folder fall back to root.
      notes: s.notes.map((n) => (n.folder_id === id ? { ...n, folder_id: null } : n)),
    })),
  upsertLocal: (note) =>
    set((s) => {
      const idx = s.notes.findIndex((n) => n.id === note.id);
      if (idx === -1) return { notes: [note, ...s.notes] };
      const next = s.notes.slice();
      next[idx] = note;
      return { notes: next };
    }),
  appendTranscript: (id, text) =>
    set((s) => ({
      notes: s.notes.map((n) =>
        n.id === id ? { ...n, transcript: (n.transcript ? n.transcript + " " : "") + text } : n
      ),
    })),
  replaceTranscript: (id, text) =>
    set((s) => ({
      notes: s.notes.map((n) => (n.id === id ? { ...n, transcript: text } : n)),
    })),
  setSummary: (id, summary) =>
    set((s) => ({ notes: s.notes.map((n) => (n.id === id ? { ...n, summary } : n)) })),
  removeLocal: (id) => set((s) => ({ notes: s.notes.filter((n) => n.id !== id) })),
}));

export type Flash = { id: number; message: string };

type RecordingState = {
  status: RecordingStatus;
  setStatus: (s: RecordingStatus) => void;
  errors: { id: number; noteId: string | null; message: string }[];
  pushError: (e: { noteId: string | null; message: string }) => void;
  dismissError: (id: number) => void;
  flashes: Flash[];
  pushFlash: (message: string) => void;
  dismissFlash: (id: number) => void;
  diag: RecordingDiagnostic | null;
  setDiag: (d: RecordingDiagnostic | null) => void;
};

let errorIdSeq = 0;
let flashIdSeq = 0;

export const useRecordingStore = create<RecordingState>((set, get) => ({
  status: { noteId: null, phase: "idle" },
  setStatus: (status) => set({ status }),
  errors: [],
  pushError: (e) => {
    // Dedupe: if the most recent error has the same message and noteId,
    // drop this one. Sidecars sometimes emit dozens of identical write
    // errors in a tight loop — surfacing each as its own toast is noise.
    const recent = get().errors[get().errors.length - 1];
    if (recent && recent.message === e.message && recent.noteId === e.noteId) {
      return;
    }
    const id = ++errorIdSeq;
    set((s) => ({ errors: [...s.errors, { id, ...e }] }));
    window.setTimeout(() => set((s) => ({ errors: s.errors.filter((x) => x.id !== id) })), 8000);
  },
  dismissError: (id) => set((s) => ({ errors: s.errors.filter((x) => x.id !== id) })),
  flashes: [],
  pushFlash: (message) => {
    const id = ++flashIdSeq;
    set((s) => ({ flashes: [...s.flashes, { id, message }] }));
    // Auto-dismiss faster than errors — flashes are positive
    // confirmations, no action needed from the user.
    window.setTimeout(
      () => set((s) => ({ flashes: s.flashes.filter((x) => x.id !== id) })),
      2500,
    );
  },
  dismissFlash: (id) => set((s) => ({ flashes: s.flashes.filter((x) => x.id !== id) })),
  diag: null,
  setDiag: (d) => set({ diag: d }),
}));

let listenersBound = false;
export function bindBackendListeners() {
  if (listenersBound) return;
  listenersBound = true;
  onTranscript(({ noteId, text }) => useNotesStore.getState().appendTranscript(noteId, text));
  onTranscriptReplaced(({ noteId, text }) => useNotesStore.getState().replaceTranscript(noteId, text));
  onSummary(({ noteId, summary }) => useNotesStore.getState().setSummary(noteId, summary));
  onRecordingStatus((s) => {
    useRecordingStore.getState().setStatus(s);
    if (s.phase === "idle") useRecordingStore.getState().setDiag(null);
  });
  onRecordingError(({ noteId, message }) => useRecordingStore.getState().pushError({ noteId, message }));
  onRecordingDiagnostic((d) => useRecordingStore.getState().setDiag(d));
}
