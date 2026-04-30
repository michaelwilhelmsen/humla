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
  created_at: number;
  updated_at: number;
};

export type Folder = {
  id: string;
  name: string;
  created_at: number;
  updated_at: number;
};

export type SettingsKey =
  | "language"
  | "transcribe_provider"
  | "transcribe_model"
  | "whisper_preset"
  | "custom_vocabulary"
  | "summary_model"
  | "summary_prompt"
  | "summary_provider"
  | "summary_local_model"
  | "theme";

export type TranscribeProvider = "openai" | "local";

export type LocalWhisperStatus = {
  downloaded: boolean;
  sizeBytes: number | null;
  path: string | null;
};

export type LocalWhisperProgress = { received: number; total: number | null };

export type DiarizeModelStatus = {
  downloaded: boolean;
  sizeBytes: number | null;
  path: string | null;
};

export type DiarizeDownloadProgress = {
  fraction: number;
  phase: "listing" | "downloading" | "compiling";
};

export const ipc = {
  listNotes: () => invoke<Note[]>("notes_list"),
  getNote: (id: string) => invoke<Note>("notes_get", { id }),
  createNote: () => invoke<Note>("notes_create"),
  updateNote: (id: string, patch: Partial<Pick<Note, "title" | "body" | "transcript" | "summary" | "summary_preset" | "language" | "summary_provider">>) =>
    invoke<void>("notes_update", { id, patch }),
  deleteNote: (id: string) => invoke<void>("notes_delete", { id }),
  moveNote: (id: string, folderId: string | null) =>
    invoke<void>("notes_move", { id, folderId }),

  listFolders: () => invoke<Folder[]>("folders_list"),
  createFolder: (name: string) => invoke<Folder>("folders_create", { name }),
  renameFolder: (id: string, name: string) =>
    invoke<void>("folders_rename", { id, name }),
  deleteFolder: (id: string) => invoke<void>("folders_delete", { id }),

  getSetting: (key: SettingsKey) => invoke<string | null>("settings_get", { key }),
  setSetting: (key: SettingsKey, value: string) => invoke<void>("settings_set", { key, value }),

  getApiKey: () => invoke<string | null>("api_key_get"),
  setApiKey: (key: string) => invoke<void>("api_key_set", { key }),
  testApiKey: () => invoke<{ ok: boolean; status: number; error: string | null }>("api_key_test"),

  localWhisperStatus: () =>
    invoke<LocalWhisperStatus>("local_whisper_status"),
  localWhisperDownload: () => invoke<void>("local_whisper_download"),
  localWhisperDelete: () => invoke<void>("local_whisper_delete"),

  diarizeStatus: () => invoke<DiarizeModelStatus>("diarize_status"),
  diarizeDownload: () => invoke<void>("diarize_download"),
  diarizeDelete: () => invoke<void>("diarize_delete"),

  localLlmStatus: () => invoke<LocalLlmStatus>("local_llm_status"),
  localLlmDownload: (variant: string) =>
    invoke<void>("local_llm_download", { variant }),
  localLlmDelete: (variant: string) =>
    invoke<void>("local_llm_delete", { variant }),
  localLlmScan: () => invoke<DiscoveredLlm[]>("local_llm_scan"),
  localLlmSelectExisting: (path: string) =>
    invoke<void>("local_llm_select_existing", { path }),
  systemMemoryGb: () => invoke<number>("system_memory_gb"),

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
export type RecordingPhase = "idle" | "starting" | "recording" | "paused" | "stopping" | "diarizing" | "loading_model" | "polishing" | "summarizing";
export type SummaryProvider = "openai" | "local";

export type LocalLlmModelEntry = {
  variant: string;
  label: string;
  bytesHint: number;
  downloaded: boolean;
  sizeBytes: number | null;
  path: string | null;
};

export type LocalLlmStatus = {
  models: LocalLlmModelEntry[];
  managedDir: string;
};

export type DiscoveredLlm = {
  source: "lm-studio" | "ollama" | "huggingface";
  name: string;
  path: string;
  sizeBytes: number;
  architecture: string;
  quantization: string;
  compatible: boolean;
};

export type LocalLlmProgress = {
  variant: string;
  received: number;
  total: number | null;
};
export type RecordingStatus = { noteId: string | null; phase: RecordingPhase };
export type RecordingError = { noteId: string | null; message: string };
export type RecordingDiagnostic = {
  noteId: string;
  micFrames: number;
  sysFrames: number;
  chunks: number;
  micPeak: number;
  sysPeak: number;
};

export function onTranscript(cb: (e: TranscriptEvent) => void): Promise<UnlistenFn> {
  return listen<TranscriptEvent>("transcript_appended", (e) => cb(e.payload));
}
export function onTranscriptReplaced(cb: (e: TranscriptEvent) => void): Promise<UnlistenFn> {
  return listen<TranscriptEvent>("transcript_replaced", (e) => cb(e.payload));
}
export function onSummary(cb: (e: SummaryEvent) => void): Promise<UnlistenFn> {
  return listen<SummaryEvent>("summary_ready", (e) => cb(e.payload));
}
export function onRecordingStatus(cb: (e: RecordingStatus) => void): Promise<UnlistenFn> {
  return listen<RecordingStatus>("recording_status", (e) => cb(e.payload));
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
export function onDiarizeDownloadProgress(cb: (e: DiarizeDownloadProgress) => void): Promise<UnlistenFn> {
  return listen<DiarizeDownloadProgress>("diarize_download_progress", (e) => cb(e.payload));
}
export function onLocalLlmProgress(cb: (e: LocalLlmProgress) => void): Promise<UnlistenFn> {
  return listen<LocalLlmProgress>("local_llm_progress", (e) => cb(e.payload));
}
