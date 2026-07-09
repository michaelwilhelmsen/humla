import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type Note = {
  id: string;
  title: string;
  body: string;
  transcript: string;
  summary: string;
  audio_path: string | null;
  summary_preset: string;
  folder_id: string | null;
  // ISO 639-1 code or "auto". Empty string means "fall back to the global
  // language setting" — used by pre-feature notes and as the create-time
  // sentinel before the user makes an explicit choice.
  language: string;
  // Per-note override for summary provider. Empty string = use global
  // setting; same convention as `language`.
  summary_provider: string;
  // Optional speaker count hint passed to the offline diarizer. `null`
  // means auto-detect (the default); a positive integer pins the cluster
  // count via VBx's `withSpeakers(exactly:)`. Most reliable fix for
  // dominant-speaker conversations where auto collapses to 1 cluster.
  expected_speakers: number | null;
  created_at: number;
  updated_at: number;
  // Cloud sync: PocketBase user id of the note's creator. Empty for
  // local-only / pre-sync notes. Resolved to a name (via the workspace member
  // list) for "created by" attribution in shared workspaces.
  owner: string;
  // Cloud sync: which workspace this note belongs to. Empty = Personal /
  // local-only (private to this device). Non-empty = shared with that
  // workspace's members. Note lists are scoped to the active workspace.
  workspace_id: string;
  // Soft-delete timestamp (ms) when the note is in the Trash; null/absent = live.
  deleted_at?: number | null;
};

export type NoteRevision = {
  id: string;
  note_id: string;
  title: string;
  body: string;
  transcript: string;
  summary: string;
  created_at: number;
};

export type Folder = {
  id: string;
  name: string;
  created_at: number;
  updated_at: number;
};

// Note export (issue #18). One combined file; content selection + format +
// speaker-label toggle. Adding a future format is just a new union member
// here + a formatter branch in the backend — no surface rework.
export type ExportFormat = "markdown" | "txt";

export type ExportSpec = {
  // Destination chosen in the native save panel.
  path: string;
  format: ExportFormat;
  includeSummary: boolean;
  includeTranscript: boolean;
  includeNotes: boolean;
  // When false, the leading `Label: ` prefix is stripped from each
  // transcript line in the export.
  includeSpeakerLabels: boolean;
};

export type SettingsKey =
  | "language"
  | "default_summary_preset"
  | "diarize_model"
  | "community1_threshold"
  | "sortformer_silence_threshold"
  | "sortformer_pred_threshold"
  | "keep_audio"
  // Cloud/teams: upload a finished recording's audio to its workspace note
  // so teammates can play it back. Default on; only the string "false"
  // disables it on the upload path. Surfaced in the Account section.
  | "sync_audio"
  | "custom_vocabulary"
  | "summary_model"
  | "summary_provider"
  | "local_llm_base_url"
  | "local_llm_model"
  | "local_llm_think"
  | "theme"
  | "palette"
  | "developer_mode"
  | "silence_rms_threshold"
  // Onboarding wizard (v0.31): completion flag + resume cursor. Both are
  // plain settings rows so the grandfathering migration and the frontend
  // takeover guard read the same source of truth.
  | "onboarding_completed"
  | "onboarding_step";

export type TranscribeProvider = "openai" | "local" | "deepgram" | "groq";

// Mirror of the Rust `crate::stt::ProviderConfig` tagged union. The four
// variants match the four supported STT providers; `local` carries the
// extra preset + GPU fields it needs.
export type ProviderConfig =
  | { provider: "openai"; model: string; base_url?: string }
  | { provider: "local"; model_id: string; preset: string; use_gpu: boolean }
  | { provider: "deepgram"; model: string; base_url?: string }
  | { provider: "groq"; model: string };

// Mirror of the Rust `crate::stt::TranscribeConfig`. Wraps a default
// ProviderConfig plus a map of per-language overrides keyed by ISO 639-1
// code (matching Note.language and the global `language` setting).
// Resolution at recording time: per_language[lang] ?? default. The "auto"
// pseudo-language always resolves to default.
export type TranscribeConfig = {
  default: ProviderConfig;
  per_language: Record<string, ProviderConfig>;
};

export type SummaryPrompt = {
  id: string;
  name: string;
  content: string;
  // Snake-case from Rust serde — keep as-is to avoid an extra map step.
  // The UI rarely needs these timestamps; they're here so we can sort
  // or display "edited X ago" later if useful.
  created_at: number;
  updated_at: number;
};

export type LocalWhisperModelStatus = {
  id: string;
  label: string;
  description: string;
  filename: string;
  sizeBytesHint: number;
  // "multilingual" — selectable as the default transcription model.
  // "language_specific" — usable only as the model behind a per-language
  // override in `transcribeConfig.per_language`. Never the default.
  kind: "multilingual" | "language_specific";
  specificLanguage: string | null;
  downloaded: boolean;
  sizeBytes: number | null;
  path: string | null;
};

export type LocalWhisperProgress = {
  modelId: string;
  received: number;
  total: number | null;
};

export type LocalWhisperDownloadError = {
  modelId: string;
  message: string;
};

export type DiarizeModelStatus = {
  downloaded: boolean;
  sizeBytes: number | null;
  path: string | null;
};

export type DiarizeDownloadProgress = {
  fraction: number;
  phase: "listing" | "downloading" | "compiling";
  // Which engine this progress belongs to. Both community1 and
  // sortformer share the diarize_download_progress event channel; the
  // frontend filters by this field.
  engine: "community1" | "sortformer";
};

export type DiarizeEngine = "community1" | "sortformer";

// Word-level timing in stream-absolute milliseconds. Drives the
// playback view's word-by-word highlight when present. Empty for
// chunks transcribed via OpenAI's streaming API (no word data
// exposed) or older recordings made before timestamps were enabled.
export type TimelineWord = {
  text: string;
  start_ms: number;
  end_ms: number;
};

// One entry per VAD-bounded chunk. `start_ms` / `end_ms` are the
// chunk's bounds in the merged playback timeline; mic and sys chunks
// can overlap, so the player highlights every chunk whose interval
// brackets the current playhead. `words` (when populated) lets the
// player highlight each word as audio passes through it.
export type TimelineEntry = {
  start_ms: number;
  end_ms: number;
  label: string;
  text: string;
  words: TimelineWord[];
  // Which recording session (#16) this entry belongs to, and its 0-based
  // index within that session's own timeline. `noteTimeline` concatenates
  // every session's timeline into one merged document so the reader never
  // hides text; `start_ms` / `end_ms` / word times stay session-*local*, so
  // the player only karaoke-matches the active session's entries against the
  // one playback.wav it has loaded.
  sessionId: string;
  sessionIndex: number;
  chunkIdx: number;
};

// One recording session (a single recording_start→stop cycle) for a note.
// Drives the playback carousel + the styled reader's session dividers.
export type NoteSession = {
  id: string;
  index: number;
  startedAt: string;
  durationMs: number;
  streams: string[];
  hasPlayback: boolean;
};

export const ipc = {
  listNotes: () => invoke<Note[]>("notes_list"),
  getNote: (id: string) => invoke<Note>("notes_get", { id }),
  createNote: () => invoke<Note>("notes_create"),
  updateNote: (
    id: string,
    patch: Partial<Pick<Note, "title" | "body" | "transcript" | "summary" | "summary_preset" | "language" | "summary_provider" | "expected_speakers">>,
  ) => invoke<void>("notes_update", { id, patch }),
  deleteNote: (id: string) => invoke<void>("notes_delete", { id }),
  moveNote: (id: string, folderId: string | null) =>
    invoke<void>("notes_move", { id, folderId }),
  // Reassign a note to a workspace ("" = Personal/local-only).
  setNoteWorkspace: (id: string, workspaceId: string) =>
    invoke<void>("notes_set_workspace", { id, workspaceId }),
  // Trash (soft-delete) — list / restore / permanently delete.
  listTrashedNotes: () => invoke<Note[]>("notes_list_trash"),
  restoreNote: (id: string) => invoke<Note>("notes_restore", { id }),
  purgeNote: (id: string) => invoke<void>("notes_purge", { id }),
  // Version history (local) — list saved revisions / restore one.
  noteRevisions: (id: string) => invoke<NoteRevision[]>("notes_revisions", { id }),
  restoreNoteRevision: (id: string, revisionId: string) =>
    invoke<Note>("notes_restore_revision", { id, revisionId }),

  listFolders: () => invoke<Folder[]>("folders_list"),
  createFolder: (name: string) => invoke<Folder>("folders_create", { name }),
  renameFolder: (id: string, name: string) =>
    invoke<void>("folders_rename", { id, name }),
  deleteFolder: (id: string) => invoke<void>("folders_delete", { id }),

  getSetting: (key: SettingsKey) => invoke<string | null>("settings_get", { key }),
  setSetting: (key: SettingsKey, value: string) => invoke<void>("settings_set", { key, value }),
  appDataDir: () => invoke<string>("app_data_dir"),
  // CPU architecture of the running process ("aarch64", "x86_64", …).
  // Onboarding uses it to steer Intel Macs toward cloud transcription.
  systemArch: () => invoke<string>("system_arch"),
  noteDiagnosticsDir: (noteId: string) =>
    invoke<string>("note_diagnostics_dir", { noteId }),
  noteAudioDir: (noteId: string) =>
    invoke<string>("note_audio_dir", { noteId }),
  noteAudioFiles: (noteId: string) =>
    invoke<string[]>("note_audio_files", { noteId }),
  noteDiagnosticsFiles: (noteId: string) =>
    invoke<string[]>("note_diagnostics_files", { noteId }),
  notePlaybackPath: (noteId: string) =>
    invoke<string | null>("note_playback_path", { noteId }),
  // Recording sessions (#16): list a note's takes, and resolve a specific
  // take's playback.wav for the session-switched player.
  noteSessions: (noteId: string) =>
    invoke<NoteSession[]>("note_sessions", { noteId }),
  noteSessionPlaybackPath: (noteId: string, sessionId: string) =>
    invoke<string | null>("note_session_playback_path", { noteId, sessionId }),
  // Cloud audio sync: upload a finished recording to its workspace, or pull a
  // shared note's audio down for local playback.
  uploadNoteAudio: (noteId: string) => invoke<void>("cloud_upload_note_audio", { noteId }),
  downloadNoteAudio: (noteId: string) => invoke<boolean>("cloud_download_note_audio", { noteId }),
  // Who (if anyone) is currently recording a shared note. null = nobody, a
  // Personal note, or the cloud isn't configured. Drives the recording-lock
  // banner + disabled Record button so teammates don't record the same note.
  noteRecordingStatus: (noteId: string) =>
    invoke<RecordingLockStatus | null>("cloud_note_recording_status", { noteId }),
  noteTimeline: (noteId: string) =>
    invoke<TimelineEntry[]>("note_timeline", { noteId }),
  noteTimelineRename: (noteId: string, oldLabel: string, newLabel: string) =>
    invoke<void>("note_timeline_rename", { noteId, oldLabel, newLabel }),
  noteTimelineSetChunkLabel: (
    noteId: string,
    sessionId: string,
    chunkIdx: number,
    newLabel: string,
  ) =>
    invoke<void>("note_timeline_set_chunk_label", {
      noteId,
      sessionId,
      chunkIdx,
      newLabel,
    }),
  noteTimelineDeleteChunk: (noteId: string, sessionId: string, chunkIdx: number) =>
    invoke<void>("note_timeline_delete_chunk", { noteId, sessionId, chunkIdx }),
  openInFinder: (path: string) => invoke<void>("open_in_finder", { path }),
  // Write a note's selected content to the chosen path as one combined file.
  exportNote: (noteId: string, spec: ExportSpec) =>
    invoke<void>("export_note", { noteId, spec }),
  rediarizeNote: (noteId: string) => invoke<void>("rediarize_note", { noteId }),

  summaryPromptsList: () => invoke<SummaryPrompt[]>("summary_prompts_list"),
  summaryPromptsCreate: (name: string, content: string) =>
    invoke<SummaryPrompt>("summary_prompts_create", { name, content }),
  summaryPromptsUpdate: (id: string, name: string, content: string) =>
    invoke<SummaryPrompt>("summary_prompts_update", { id, name, content }),
  summaryPromptsDelete: (id: string) =>
    invoke<void>("summary_prompts_delete", { id }),

  getProviderKey: (provider: TranscribeProvider) =>
    invoke<string | null>("provider_key_get", { provider }),
  setProviderKey: (provider: TranscribeProvider, key: string) =>
    invoke<void>("provider_key_set", { provider, key }),
  testProviderKey: (provider: TranscribeProvider) =>
    invoke<{ ok: boolean; status: number; error: string | null }>("provider_key_test", {
      provider,
    }),
  getTranscribeConfig: () => invoke<TranscribeConfig>("get_transcribe_config"),
  setTranscribeConfig: (config: TranscribeConfig) =>
    invoke<void>("set_transcribe_config", { config }),

  localWhisperModels: () =>
    invoke<LocalWhisperModelStatus[]>("local_whisper_models"),
  localWhisperDownload: (modelId: string) =>
    invoke<void>("local_whisper_download", { modelId }),
  localWhisperDelete: (modelId: string) =>
    invoke<void>("local_whisper_delete", { modelId }),

  diarizeStatus: (engine?: DiarizeEngine) =>
    invoke<DiarizeModelStatus>("diarize_status", { engine }),
  diarizeDownload: (engine?: DiarizeEngine) =>
    invoke<void>("diarize_download", { engine }),
  diarizeDelete: (engine?: DiarizeEngine) =>
    invoke<void>("diarize_delete", { engine }),

  localLlmListModels: (baseUrl: string) =>
    invoke<string[]>("local_llm_list_models", { baseUrl }),

  recordingStart: (noteId: string) => invoke<void>("recording_start", { noteId }),
  recordingStop: () => invoke<void>("recording_stop"),
  recordingPause: () => invoke<void>("recording_pause"),
  recordingResume: () => invoke<void>("recording_resume"),
  recordingState: () => invoke<"idle" | "recording">("recording_state"),
  summarizeNote: (noteId: string) => invoke<void>("summarize_note", { noteId }),

  permissionsStatus: () => invoke<PermissionsStatus>("permissions_status"),
  permissionsRequest: (kind: PermissionKind) => invoke<PermissionsStatus>("permissions_request", { kind }),
  permissionsOpenSettings: (kind: PermissionKind) => invoke<void>("permissions_open_settings", { kind }),
};

export type PermissionKind = "microphone" | "screen";
export type PermissionStatus =
  | "granted"
  | "denied"
  | "restricted"
  | "not_determined"
  | "unknown";
export type PermissionsStatus = {
  microphone: PermissionStatus;
  screen: PermissionStatus;
};

export type TranscriptEvent = { noteId: string; text: string };
export type SummaryEvent = { noteId: string; summary: string };
export type StreamDeltaEvent = { noteId: string; delta: string };
export type RecordingPhase = "idle" | "starting" | "recording" | "paused" | "stopping" | "diarizing";
export type SummaryProvider = "openai" | "local";
export type RecordingStatus = { noteId: string | null; phase: RecordingPhase };
export type RecordingError = { noteId: string | null; message: string };
export type SummaryStatus = { noteId: string; active: boolean };
export type RecordingDiagnostic = {
  noteId: string;
  micFrames: number;
  sysFrames: number;
  chunks: number;
  micPeak: number;
  sysPeak: number;
};
// The teammate currently holding a shared note's recording lock.
export type RecordingLockStatus = { holderId: string; holderName: string };

export function onTranscript(cb: (e: TranscriptEvent) => void): Promise<UnlistenFn> {
  return listen<TranscriptEvent>("transcript_appended", (e) => cb(e.payload));
}
export function onTranscriptReplaced(cb: (e: TranscriptEvent) => void): Promise<UnlistenFn> {
  return listen<TranscriptEvent>("transcript_replaced", (e) => cb(e.payload));
}
export function onSummary(cb: (e: SummaryEvent) => void): Promise<UnlistenFn> {
  return listen<SummaryEvent>("summary_ready", (e) => cb(e.payload));
}
export function onSummaryThinkingDelta(cb: (e: StreamDeltaEvent) => void): Promise<UnlistenFn> {
  return listen<StreamDeltaEvent>("summary_thinking_delta", (e) => cb(e.payload));
}
export function onSummaryContentDelta(cb: (e: StreamDeltaEvent) => void): Promise<UnlistenFn> {
  return listen<StreamDeltaEvent>("summary_content_delta", (e) => cb(e.payload));
}
export function onRecordingStatus(cb: (e: RecordingStatus) => void): Promise<UnlistenFn> {
  return listen<RecordingStatus>("recording_status", (e) => cb(e.payload));
}
export function onSummaryStatus(cb: (e: SummaryStatus) => void): Promise<UnlistenFn> {
  return listen<SummaryStatus>("summary_status", (e) => cb(e.payload));
}
export function onRecordingError(cb: (e: RecordingError) => void): Promise<UnlistenFn> {
  return listen<RecordingError>("recording_error", (e) => cb(e.payload));
}
export function onRecordingDiagnostic(cb: (e: RecordingDiagnostic) => void): Promise<UnlistenFn> {
  return listen<RecordingDiagnostic>("recording_diagnostic", (e) => cb(e.payload));
}
export function onLocalWhisperProgress(cb: (e: LocalWhisperProgress) => void): Promise<UnlistenFn> {
  return listen<LocalWhisperProgress>("local_whisper_progress", (e) => cb(e.payload));
}
// Fired when a model download fails on the backend. Progress events alone
// can't express failure, and the invoke promise that started the download may
// belong to a component that has since unmounted — this event is how any
// still-mounted UI learns the download is dead instead of showing a
// forever-progress bar.
export function onLocalWhisperDownloadError(
  cb: (e: LocalWhisperDownloadError) => void,
): Promise<UnlistenFn> {
  return listen<LocalWhisperDownloadError>("local_whisper_download_error", (e) => cb(e.payload));
}
export function onDiarizeDownloadProgress(cb: (e: DiarizeDownloadProgress) => void): Promise<UnlistenFn> {
  return listen<DiarizeDownloadProgress>("diarize_download_progress", (e) => cb(e.payload));
}
// Emitted by the cloud sync worker after it applies pulled remote changes to
// the local store, so the UI refetches. Fires only when sync is active (cloud
// feature + signed in + a workspace selected); inert otherwise.
export function onNotesChanged(cb: () => void): Promise<UnlistenFn> {
  return listen("notes_changed", () => cb());
}

// Coarse sync state from the cloud worker, for the sidebar indicator.
export type SyncStatus = "syncing" | "idle" | "error";
export function onSyncStatus(cb: (s: SyncStatus) => void): Promise<UnlistenFn> {
  return listen<SyncStatus>("sync_status", (e) => cb(e.payload));
}

// A pull preserved local edits as a "(conflict copy)" instead of overwriting
// them. Payload is the note title. The UI toasts it so the user can find the copy.
export function onSyncConflict(cb: (title: string) => void): Promise<UnlistenFn> {
  return listen<string>("sync_conflict", (e) => cb(e.payload));
}
