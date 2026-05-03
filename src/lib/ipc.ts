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
  | "local_whisper_model"
  | "local_whisper_use_gpu"
  | "final_pass"
  | "default_summary_preset"
  | "diarize_model"
  | "community1_threshold"
  | "sortformer_silence_threshold"
  | "sortformer_pred_threshold"
  | "keep_audio"
  | "custom_vocabulary"
  | "summary_model"
  | "summary_prompt"
  | "summary_provider"
  | "local_llm_base_url"
  | "local_llm_model"
  | "local_llm_think"
  | "theme";

export type TranscribeProvider = "openai" | "local";

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
  // "primary" — selectable as the active transcription model.
  // "addon" — auto-applied for recordings whose language matches
  // `addonLanguage`; never the active primary.
  kind: "primary" | "addon";
  addonLanguage: string | null;
  downloaded: boolean;
  sizeBytes: number | null;
  path: string | null;
};

export type LocalWhisperProgress = {
  modelId: string;
  received: number;
  total: number | null;
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

  listFolders: () => invoke<Folder[]>("folders_list"),
  createFolder: (name: string) => invoke<Folder>("folders_create", { name }),
  renameFolder: (id: string, name: string) =>
    invoke<void>("folders_rename", { id, name }),
  deleteFolder: (id: string) => invoke<void>("folders_delete", { id }),

  getSetting: (key: SettingsKey) => invoke<string | null>("settings_get", { key }),
  setSetting: (key: SettingsKey, value: string) => invoke<void>("settings_set", { key, value }),
  appDataDir: () => invoke<string>("app_data_dir"),
  noteDiagnosticsDir: (noteId: string) =>
    invoke<string>("note_diagnostics_dir", { noteId }),
  noteAudioDir: (noteId: string) =>
    invoke<string>("note_audio_dir", { noteId }),
  noteAudioFiles: (noteId: string) =>
    invoke<string[]>("note_audio_files", { noteId }),
  noteDiagnosticsFiles: (noteId: string) =>
    invoke<string[]>("note_diagnostics_files", { noteId }),

  summaryPromptsList: () => invoke<SummaryPrompt[]>("summary_prompts_list"),
  summaryPromptsCreate: (name: string, content: string) =>
    invoke<SummaryPrompt>("summary_prompts_create", { name, content }),
  summaryPromptsUpdate: (id: string, name: string, content: string) =>
    invoke<SummaryPrompt>("summary_prompts_update", { id, name, content }),
  summaryPromptsDelete: (id: string) =>
    invoke<void>("summary_prompts_delete", { id }),

  getApiKey: () => invoke<string | null>("api_key_get"),
  setApiKey: (key: string) => invoke<void>("api_key_set", { key }),
  testApiKey: () => invoke<{ ok: boolean; status: number; error: string | null }>("api_key_test"),

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
  polishNote: (noteId: string) => invoke<void>("polish_note", { noteId }),

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
export type RecordingPhase = "idle" | "starting" | "recording" | "paused" | "stopping" | "diarizing" | "retranscribing" | "polishing" | "summarizing";
export type SummaryProvider = "openai" | "local";
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
export function onSummaryThinkingDelta(cb: (e: StreamDeltaEvent) => void): Promise<UnlistenFn> {
  return listen<StreamDeltaEvent>("summary_thinking_delta", (e) => cb(e.payload));
}
export function onSummaryContentDelta(cb: (e: StreamDeltaEvent) => void): Promise<UnlistenFn> {
  return listen<StreamDeltaEvent>("summary_content_delta", (e) => cb(e.payload));
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
