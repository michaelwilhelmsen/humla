import { create } from "zustand";
import { ipc, onRecordingDiagnostic, onRecordingError, onRecordingStatus, onSummary, onTranscript, type Note, type RecordingDiagnostic, type RecordingStatus } from "./ipc";

type NotesState = {
  notes: Note[];
  refresh: () => Promise<void>;
  upsertLocal: (note: Note) => void;
  appendTranscript: (id: string, text: string) => void;
  setSummary: (id: string, summary: string) => void;
  removeLocal: (id: string) => void;
};

export const useNotesStore = create<NotesState>((set) => ({
  notes: [],
  refresh: async () => {
    const notes = await ipc.listNotes();
    set({ notes });
  },
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
  setSummary: (id, summary) =>
    set((s) => ({ notes: s.notes.map((n) => (n.id === id ? { ...n, summary } : n)) })),
  removeLocal: (id) => set((s) => ({ notes: s.notes.filter((n) => n.id !== id) })),
}));

type RecordingState = {
  status: RecordingStatus;
  setStatus: (s: RecordingStatus) => void;
  errors: { id: number; noteId: string | null; message: string }[];
  pushError: (e: { noteId: string | null; message: string }) => void;
  dismissError: (id: number) => void;
  diag: RecordingDiagnostic | null;
  setDiag: (d: RecordingDiagnostic | null) => void;
};

let errorIdSeq = 0;

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
  diag: null,
  setDiag: (d) => set({ diag: d }),
}));

let listenersBound = false;
export function bindBackendListeners() {
  if (listenersBound) return;
  listenersBound = true;
  onTranscript(({ noteId, text }) => useNotesStore.getState().appendTranscript(noteId, text));
  onSummary(({ noteId, summary }) => useNotesStore.getState().setSummary(noteId, summary));
  onRecordingStatus((s) => {
    useRecordingStore.getState().setStatus(s);
    if (s.phase === "idle") useRecordingStore.getState().setDiag(null);
  });
  onRecordingError(({ noteId, message }) => useRecordingStore.getState().pushError({ noteId, message }));
  onRecordingDiagnostic((d) => useRecordingStore.getState().setDiag(d));
}
