import { create } from "zustand";
import { ipc, onRecordingDiagnostic, onRecordingError, onRecordingStatus, onSummary, onSummaryStatus, onTranscript, onTranscriptReplaced, onNotesChanged, onSyncStatus, onSyncConflict, onLocalWhisperProgress, type Folder, type Note, type RecordingDiagnostic, type RecordingStatus } from "./ipc";
import { useCloudStore } from "./cloud";

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
  // Per-note "summary in flight" flags. Lives separately from `status` so
  // summarising note B can't blank the recording state of note A — those
  // are independent lifecycles. Keyed by noteId; absent or false means no
  // summary running for that note.
  summarizing: Record<string, boolean>;
  setSummarizing: (noteId: string, active: boolean) => void;
  errors: { id: number; noteId: string | null; message: string; kind?: "recording" | "summary" }[];
  pushError: (e: { noteId: string | null; message: string; kind?: "recording" | "summary" }) => void;
  dismissError: (id: number) => void;
  flashes: Flash[];
  pushFlash: (message: string) => void;
  dismissFlash: (id: number) => void;
  diag: RecordingDiagnostic | null;
  setDiag: (d: RecordingDiagnostic | null) => void;
  // Live audio-level (peak) for the meter in the recording bar. Fed off the
  // sidecar heartbeat (every ~2s) and decayed toward zero between beats by the
  // bar itself so it reads as a meter, not a stutter. 0..~1.
  micLevel: number;
  sysLevel: number;
  setLevels: (mic: number, sys: number) => void;
  // No-audio safety net. Tracks, per recording, whether real mic audio has ever
  // arrived and how long the recording has been actively capturing (pauses
  // excluded), so the bar can warn after ~10s of silence. Reset when a
  // recording starts; `micHeard` latches true the moment audio arrives.
  micHeard: boolean;
  // Wall-clock ms when the current *active* (non-paused) capture segment began,
  // or null while paused/idle. Total active time = activeAccumMs + (now - activeSince).
  activeSince: number | null;
  activeAccumMs: number;
  // Called from the status listener on every recording phase transition to keep
  // the audio-warning bookkeeping in sync (reset on start, pause the clock on
  // pause, resume it on resume, clear on stop).
  syncAudioWatch: (phase: RecordingStatus["phase"], noteId: string | null) => void;
};

let errorIdSeq = 0;
let flashIdSeq = 0;

export const useRecordingStore = create<RecordingState>((set, get) => ({
  status: { noteId: null, phase: "idle" },
  setStatus: (status) => set({ status }),
  summarizing: {},
  setSummarizing: (noteId, active) =>
    set((s) => {
      const next = { ...s.summarizing };
      if (active) next[noteId] = true;
      else delete next[noteId];
      return { summarizing: next };
    }),
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
  micLevel: 0,
  sysLevel: 0,
  setLevels: (mic, sys) => set({ micLevel: mic, sysLevel: sys }),
  micHeard: false,
  activeSince: null,
  activeAccumMs: 0,
  syncAudioWatch: (phase, noteId) =>
    set((s) => {
      const now = Date.now();
      switch (phase) {
        case "starting":
        case "recording": {
          // A brand-new recording (different note, or coming from a
          // non-capturing phase) resets the whole safety-net state. A
          // recording→recording repeat (heartbeat-driven re-renders don't call
          // this, but a resume does via "recording") must NOT reset — only
          // (re)start the active clock.
          const isNewRecording =
            s.status.noteId !== noteId || (s.status.phase !== "recording" && s.status.phase !== "paused");
          if (isNewRecording) {
            return { micHeard: false, activeAccumMs: 0, activeSince: now, micLevel: 0, sysLevel: 0 };
          }
          // Resume: bank nothing extra, just restart the active clock if it was
          // stopped (paused). If already ticking, leave it.
          return s.activeSince === null ? { activeSince: now } : {};
        }
        case "paused": {
          // Freeze the active clock: bank the elapsed segment, stop counting.
          if (s.activeSince === null) return {};
          return { activeAccumMs: s.activeAccumMs + (now - s.activeSince), activeSince: null };
        }
        default: {
          // stopping / diarizing / idle — recording is over. Clear the meter and
          // the watch so nothing lingers into the next session.
          return { micHeard: false, activeSince: null, activeAccumMs: 0, micLevel: 0, sysLevel: 0 };
        }
      }
    }),
}));

// Global model-download slice (WP-E). The wizard's Transcription step tracks
// download progress only while mounted; the sidebar nag chip and the "You're
// all set" recap need it AFTER the user skips out mid-download. This tiny
// store is fed by a single `local_whisper_progress` listener registered in
// bindBackendListeners() so the chip can show "Downloading — NN%" and clear
// itself (re-evaluating pipelineReady) the instant the download completes.
type WhisperDownload = { modelId: string; received: number; total: number | null };
type DownloadState = {
  // The in-flight whisper model download, or null when nothing is downloading.
  active: WhisperDownload | null;
  setProgress: (d: WhisperDownload) => void;
  clear: () => void;
};
export const useDownloadStore = create<DownloadState>((set) => ({
  active: null,
  setProgress: (d) => set({ active: d }),
  clear: () => set({ active: null }),
}));

// Peak above which we consider the mic to be genuinely hearing something (not
// noise-floor / silence). Matches the existing active-dot threshold in the bar.
const MIC_AUDIBLE_PEAK = 0.001;

let listenersBound = false;
// The note id of the in-flight recording, captured so we can upload its audio
// once it finishes (the idle status itself doesn't carry the note id).
let recordingNoteId: string | null = null;
export function bindBackendListeners() {
  if (listenersBound) return;
  listenersBound = true;
  // Re-check cloud status when the window regains focus — picks up subscription
  // changes after the user completes Stripe checkout/portal in their browser.
  window.addEventListener("focus", () => {
    void useCloudStore.getState().refresh();
  });
  onTranscript(({ noteId, text }) => useNotesStore.getState().appendTranscript(noteId, text));
  onTranscriptReplaced(({ noteId, text }) => useNotesStore.getState().replaceTranscript(noteId, text));
  onSummary(({ noteId, summary }) => useNotesStore.getState().setSummary(noteId, summary));
  onRecordingStatus((s) => {
    // Update the audio-warning bookkeeping BEFORE setStatus — syncAudioWatch
    // compares against the *previous* status to tell a new recording apart from
    // a resume.
    useRecordingStore.getState().syncAudioWatch(s.phase, s.noteId);
    useRecordingStore.getState().setStatus(s);
    if (s.phase === "idle") {
      useRecordingStore.getState().setDiag(null);
      // A recording just finished — if it was a shared (workspace) note, upload
      // its audio for teammates. The command waits for the post-stop pipeline to
      // write playback.wav, so fire-and-forget here.
      if (recordingNoteId) {
        const id = recordingNoteId;
        recordingNoteId = null;
        const note = useNotesStore.getState().notes.find((n) => n.id === id);
        if (note?.workspace_id) void ipc.uploadNoteAudio(id);
      }
    } else if (s.noteId) {
      recordingNoteId = s.noteId; // an active recording — remember which note
    }
  });
  onSummaryStatus(({ noteId, active }) => {
    useRecordingStore.getState().setSummarizing(noteId, active);
  });
  onRecordingError(({ noteId, message }) => useRecordingStore.getState().pushError({ noteId, message }));
  onRecordingDiagnostic((d) => {
    const st = useRecordingStore.getState();
    st.setDiag(d);
    // Feed the level meter (the bar decays these toward 0 between heartbeats).
    st.setLevels(d.micPeak, d.sysPeak);
    // Latch "mic heard" the instant real audio arrives — clears any pending or
    // shown no-audio warning for the rest of this recording.
    if (!st.micHeard && d.micPeak > MIC_AUDIBLE_PEAK) {
      useRecordingStore.setState({ micHeard: true });
    }
  });
  // Cloud sync applied remote changes → refetch notes + folders.
  onNotesChanged(() => useNotesStore.getState().refresh());
  // Live sync state → sidebar indicator. Also refresh the per-note pending set,
  // since a syncing→idle transition brackets the outbox drain.
  onSyncStatus((s) => {
    useCloudStore.getState().setSyncStatus(s);
    void useCloudStore.getState().refreshPending();
  });
  // A sync conflict preserved local edits as a copy → tell the user where it went.
  onSyncConflict((title) =>
    useRecordingStore.getState().pushError({
      noteId: null,
      message: `"${title}" changed on the server — your unsynced edits were saved as a "(conflict copy)" note.`,
    }),
  );
  // Global whisper-download tracking (WP-E). Feeds the sidebar nag chip + the
  // "You're all set" recap so a model download that finishes AFTER the user
  // leaves the wizard still clears the nag. On the final event (received >=
  // total) we clear the slice, which re-evaluates pipelineReady everywhere.
  onLocalWhisperProgress(({ modelId, received, total }) => {
    if (total !== null && received >= total) {
      useDownloadStore.getState().clear();
    } else {
      useDownloadStore.getState().setProgress({ modelId, received, total });
    }
  });
  // "Added to a workspace" notification. Watch the cloud status for workspaces
  // that newly appear and that you didn't create (role !== owner) — i.e. an
  // admin added you — and flash it, since otherwise it's silent.
  let knownWorkspaceIds: Set<string> | null = null;
  let knownUserId: string | null = null;
  useCloudStore.subscribe((s) => {
    if (!s.ready) return;
    const uid = s.status.user?.id ?? null;
    const ids = new Set(s.status.workspaces.map((w) => w.id));
    // Re-baseline (no flash) on the first snapshot AND whenever the signed-in
    // user changes (login / logout / account switch) — otherwise the next
    // session's workspaces would all spuriously flash "Added to…".
    if (knownWorkspaceIds === null || uid !== knownUserId) {
      knownUserId = uid;
      knownWorkspaceIds = ids;
      return;
    }
    for (const w of s.status.workspaces) {
      if (!knownWorkspaceIds.has(w.id) && w.role !== "owner") {
        useRecordingStore.getState().pushFlash(`Added to "${w.name}"`);
      }
    }
    knownWorkspaceIds = ids;
  });
}
