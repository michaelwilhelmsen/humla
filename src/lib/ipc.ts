import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { SpeakerLabelStat } from "./speakerSuggest";
import { targetFolderId, targetNoteId, type ChatTarget } from "./chatTarget";

/** The `(noteId, folderId)` pair every chat command sends, derived in ONE place
 *  so no call site can pair a folder id with a note scope. `chatTarget.ts` imports
 *  only a type from here, so this direction of the cycle carries no runtime edge. */
function targetIds(target: ChatTarget): { noteId: string | null; folderId: string | null } {
  return { noteId: targetNoteId(target), folderId: targetFolderId(target) };
}

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
  // ISO 639-1 code the STT provider reported for the recording (issue #167),
  // decided by a length-weighted vote across chunks. Only set for notes
  // captured on `auto`; null on explicit-language notes, on providers that
  // don't report one, and on recordings too bilingual to call. Derived and
  // local-only — never synced (ADR 0002).
  detected_language: string | null;
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
  // Optional Client tag (issue #43). null = untagged. Independent of folder_id.
  client_id?: string | null;
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

// A Client (issue #43): who a Note is about. Same shape as Folder on the wire;
// workspace_id is backend-only and omitted here.
export type Client = {
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
  // AI Chat provider (issue #44). Independent of the summary/STT provider.
  // "openai" (cloud, shared OpenAI key) or "ollama" (local). The Ollama
  // endpoint reuses `local_llm_base_url`; `chat_model` holds the active
  // provider's model. Embedding model is auto-derived, not a setting.
  | "chat_provider"
  | "chat_model"
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

// What a "delete stored audio for existing notes" sweep would remove (#24).
// `notes` counts only notes that actually hold audio, so the confirm can be
// specific about an irreversible action.
export type StoredAudioStats = {
  notes: number;
  files: number;
  bytes: number;
  noteIds: string[];
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

  // Clients (issue #43) — mirror the folder methods. deleteClient un-tags the
  // client's notes (never deletes them); setNoteClient assigns/clears a note's
  // client (null = untag).
  listClients: () => invoke<Client[]>("clients_list"),
  createClient: (name: string) => invoke<Client>("clients_create", { name }),
  renameClient: (id: string, name: string) => invoke<void>("clients_rename", { id, name }),
  deleteClient: (id: string) => invoke<void>("clients_delete", { id }),
  setNoteClient: (id: string, clientId: string | null) =>
    invoke<void>("notes_set_client", { id, clientId }),

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
  // Honest keep_audio (#24): what a cleanup sweep would remove across every
  // note, and the sweep itself (returns the file count removed). Audio only —
  // transcripts, timelines and chunk timings survive.
  storedAudioStats: () => invoke<StoredAudioStats>("stored_audio_stats"),
  deleteStoredAudio: () => invoke<number>("delete_stored_audio"),
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
  // Per-session asset sync (#16): upload every take's assets after a recording
  // / re-diarize, or pull a shared note's sessions (rebuilds sessions.json +
  // fetches each take's playback + timeline). Returns true when sessions landed.
  uploadNoteSessions: (noteId: string) => invoke<void>("cloud_upload_note_sessions", { noteId }),
  downloadNoteSessions: (noteId: string) =>
    invoke<boolean>("cloud_download_note_sessions", { noteId }),
  // Opportunistic repair on note open: attach any per-session asset the server
  // is missing, re-sending nothing it already has. Covers the post-recording
  // upload never having run (app quit, network down) — without it, a stranded
  // timeline is stranded for good and teammates never see the speaker labels.
  repairNoteSessions: (noteId: string) =>
    invoke<void>("cloud_repair_note_sessions", { noteId }),
  // Who (if anyone) is currently recording a shared note. null = nobody, a
  // Personal note, or the cloud isn't configured. Drives the recording-lock
  // banner + disabled Record button so teammates don't record the same note.
  noteRecordingStatus: (noteId: string) =>
    invoke<RecordingLockStatus | null>("cloud_note_recording_status", { noteId }),
  // The rename picker's two suggestion halves (#116): every distinct speaker
  // label in the active workspace with usage counters, and the workspace member
  // names. Fetched once per strip mount; ranking and filtering happen in TS
  // (`lib/speakerSuggest.ts`). `speakerRoster` is best-effort by design and
  // serves a cached roster when offline — unlike `cloudApi.workspaceMembers`,
  // which member management needs to fail loudly.
  speakerLabelStats: () => invoke<SpeakerLabelStat[]>("speaker_label_stats"),
  speakerRoster: () => invoke<string[]>("cloud_speaker_roster"),
  // The name to prefill when renaming the literal `You:` label (#116 part 2):
  // `user_display_name` → workspace account name → macOS full name. The cloud
  // name is passed in because the store already holds it, so the prefill never
  // waits on the network.
  speakerDefaultName: (cloudName: string | null) =>
    invoke<string | null>("speaker_default_name", { cloudName }),
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
  // Import an existing audio file into a NEW note and run it through the full
  // transcription pipeline. Language + expected speaker count come from the
  // import config dialog so the note is seeded correctly before the (one-shot)
  // transcription runs. Returns the created note.
  importAudio: (path: string, language: string, expectedSpeakers: number | null) =>
    invoke<Note>("import_audio", { path, language, expectedSpeakers }),
  recordingPause: () => invoke<void>("recording_pause"),
  recordingResume: () => invoke<void>("recording_resume"),
  recordingState: () => invoke<"idle" | "recording">("recording_state"),
  summarizeNote: (noteId: string) => invoke<void>("summarize_note", { noteId }),

  // AI chat over a single Note (issue #46). `chatSend` runs one grounded,
  // streamed completion — the answer arrives via chat_* events, so the
  // resolved promise only carries the conversation id + a truncation flag.
  // `chatHistory` reloads a conversation and reports which one it resolved to.
  //
  // Chat sessions (issue #61): a Note can have multiple conversations. Each call
  // takes an optional `conversationId` — omit it (or pass null) to target the
  // active/most-recent session; opening the tab creates nothing. `chatHistory`
  // returns the resolved `conversationId` so the panel learns which session it's
  // on; `chatSend` returns it too (lazy-creating on the first turn).
  //
  // Chat is pinned to the loaded context (issue #58): the backend derives the
  // tenant from the active workspace (Personal when none), so no call takes a
  // tenant. Retrieval breadth ("note" | "folder" | "all") is persisted on the
  // conversation server-side — `chatSetBreadth` writes it, `chatGetBreadth` reads
  // it back to initialise the Scope chip. `chatSend` carries no scope: it reads
  // the persisted breadth, so the UI can never diverge from it.
  // Every chat command takes a `ChatTarget` and sends it as the `noteId` /
  // `folderId` pair the backend's `ChatTarget::from_ids` parses (#93, #110). The
  // union crosses the boundary rather than a bare nullable id because a null note
  // id no longer identifies a scope on its own: both a folder pane and the
  // library-wide pane have none. An ABSENT id is what selects a note-less scope
  // and an EMPTY string is rejected, so these must be null and never "".
  //
  // `ownerName` is a DISPLAY NAME for the pinned author (#103), used only in the
  // prompt's disclosure line — the pinned *id* lives on the conversation row and
  // is what actually filters. Sent from the roster the client already holds so
  // the turn costs no extra lookup; null when there's no pin or no name for it.
  //
  // `draft` carries a library-wide pane's breadth + authorship pin when no
  // conversation exists yet (#120). A drafting pane deliberately persists nothing
  // until its first turn, so these are the only way settings chosen beforehand
  // reach the row this turn creates — and they are read ONLY on creation, since an
  // existing conversation is already the source of truth for both. Omit them
  // whenever `conversationId` is non-null.
  chatSend: (
    target: ChatTarget,
    conversationId: string | null,
    message: string,
    ownerName: string | null = null,
    draft: { breadth: ChatScope | null; ownerFilter: string | null } | null = null,
  ) =>
    invoke<ChatSendResult>("chat_send", {
      ...targetIds(target),
      conversationId,
      message,
      ownerName,
      draftBreadth: draft?.breadth ?? null,
      draftOwnerFilter: draft?.ownerFilter ?? null,
    }),
  // Stop the turn streaming in a pane (issue #80). A no-op when nothing is in
  // flight, so a stray click can't error. Any text that already streamed is kept;
  // a stop before the first token leaves only the user's message.
  chatCancel: (target: ChatTarget) => invoke<void>("chat_cancel", targetIds(target)),
  chatHistory: (target: ChatTarget, conversationId: string | null = null) =>
    invoke<ChatHistory>("chat_history", { ...targetIds(target), conversationId }),
  // List / create chat sessions for a target (issue #61). Personal reads local
  // SQLite; a workspace reads/creates server-authoritative sessions.
  // `limit`/`offset` window the list, most-recent first (issue #95): `/chat`
  // lists conversations uncapped, so it pages them in as the sidebar scrolls.
  // Omitting `limit` returns everything, which is what the Note header's history
  // popover has always done.
  chatListConversations: (target: ChatTarget, page?: { limit: number; offset: number }) =>
    invoke<ConversationMeta[]>("chat_list_conversations", {
      ...targetIds(target),
      limit: page?.limit ?? null,
      offset: page?.offset ?? null,
    }),
  chatNewConversation: (target: ChatTarget) =>
    invoke<ConversationMeta>("chat_new_conversation", targetIds(target)),
  // Delete / rename a conversation (issue #109). `conversationId` is REQUIRED on
  // both — unlike the read commands, neither falls back to "the active one", so a
  // missing id can't destroy or relabel whatever happens to be newest.
  //
  // In a workspace both go to the server first and abort if it refuses (only the
  // thread's creator, or a workspace owner/admin, may do either). Delete is
  // idempotent, so a retry after a partial failure is safe; rename returns the
  // updated row so the caller doesn't have to re-list to see the new title.
  chatDeleteConversation: (target: ChatTarget, conversationId: string) =>
    invoke<void>("chat_delete_conversation", { ...targetIds(target), conversationId }),
  chatRenameConversation: (target: ChatTarget, conversationId: string, title: string) =>
    invoke<ConversationMeta>("chat_rename_conversation", { ...targetIds(target), conversationId, title }),
  // Persist / read the Scope chip's breadth on a conversation (issue #58/#61).
  // The single source of truth for retrieval breadth.
  chatSetBreadth: (target: ChatTarget, conversationId: string | null, breadth: ChatScope) =>
    invoke<void>("chat_set_breadth", { ...targetIds(target), conversationId, breadth }),
  chatGetBreadth: (target: ChatTarget, conversationId: string | null = null) =>
    invoke<ChatScope>("chat_get_breadth", { ...targetIds(target), conversationId }),
  // Persist / read the conversation's pinned authorship filter (#103) — a user
  // id, or null/"" for off. Workspace-only: the backend rejects a pin in
  // Personal, where every note is the user's own already.
  chatSetOwnerFilter: (target: ChatTarget, conversationId: string | null, owner: string | null) =>
    invoke<void>("chat_set_owner_filter", { ...targetIds(target), conversationId, owner }),
  chatGetOwnerFilter: (target: ChatTarget, conversationId: string | null = null) =>
    invoke<string>("chat_get_owner_filter", { ...targetIds(target), conversationId }),
  // Workspace turn allowance for the composer meter (issue #69). null in personal
  // context, and on any unavailable/error/unmetered outcome — a meter never
  // errors the pane, so the caller just hides the display when this is null.
  chatUsage: () => invoke<ChatUsage | null>("chat_usage"),
  /** How the workspace's retrieval index looks to search, or null when there's no
   *  information (Personal, an older server, or any failure — the caller keeps its
   *  local guess). See #102: only the server can tell a backfilling index apart
   *  from a genuinely empty library. */
  chatIndexState: () => invoke<ChatIndexState | null>("chat_index_state"),
  // Rebuild a Note's retrieval index — called on Note-view unmount so edits
  // that didn't trigger summarize/diarize still land in search.
  chatReindexNote: (noteId: string) => invoke<void>("chat_reindex_note", { noteId }),
  /** Rebuild the retrieval index for every live note; resolves with how many (#104).
   *  Slow and re-embeds, so it is user-triggered — see `chat_rebuild_index`. */
  chatRebuildIndex: () => invoke<number>("chat_rebuild_index"),
  /** How many notes a rebuild would repair — 0 when the index is current (#122). */
  chatStaleNoteCount: () => invoke<number>("chat_stale_note_count"),

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

// ── AI chat (issues #46, #47) ────────────────────────────────────────────
// A message's content is a typed parts array (opencode v2 shape): text parts
// plus, since #47, tool parts recording each executed retrieval call.
export type ChatCitation = { noteId: string; title: string; createdAt: number };
export type ChatPart =
  | { type: "text"; id: string; text: string }
  | {
      type: "tool";
      id: string;
      name: string;
      args?: string;
      result?: string;
      citations?: ChatCitation[];
      isError?: boolean;
    };
export type ChatMessageDto = {
  id: string;
  role: "user" | "assistant";
  seq: number;
  parts: ChatPart[];
  createdAt: number;
};
export type ChatSendResult = { conversationId: string; truncated: boolean };
// `chatHistory` result (issue #61): the messages plus the conversation they were
// resolved to. `conversationId` is null when the Note has no session yet.
export type ChatHistory = { conversationId: string | null; messages: ChatMessageDto[] };
// One chat session in the list (issue #61). `title` is resolved server/back-end
// side (stored title, else a date fallback), so it always renders.
export type ConversationMeta = {
  id: string;
  title: string;
  breadth: ChatScope;
  /** The pinned authorship filter's user id, or "" for off (#103). A user id
   *  rather than a flag because a workspace's conversation list is shared: a
   *  boolean would mean different notes to different readers of one thread. */
  ownerFilter: string;
  updatedAt: number;
  messageCount: number;
};
// Retrieval breadth chosen in the Scope popover (issue #47), persisted per
// conversation on the backend (issue #58).
export type ChatScope = "note" | "folder" | "all";
// Workspace turn allowance for the composer meter (issue #69). Only ever present
// for a metered workspace; personal/unmetered/unavailable resolve to null.
/** The server's view of a workspace's retrieval index (#102). "empty" covers both
 *  never-indexed and mid-backfill; "quarantined" is the indexer's deactivation
 *  grace window. */
export type ChatIndexState = "ready" | "empty" | "quarantined";

export type ChatUsage = { used: number; cap: number; periodEnd: number };

// Streaming events (the #46 wire contract + #47 tool/citation events).
export type ChatTextDeltaEvent = {
  conversationId: string;
  messageId: string;
  blockId: string;
  delta: string;
};
export type ChatDoneEvent = { conversationId: string; messageId: string };
export type ChatErrorEvent = {
  conversationId: string;
  message: string;
  // Machine reason code (issue #76) so the pane can render role-aware BYOK
  // error copy. Empty/absent for the personal loop and unknown errors.
  reason?: string;
};
export type ChatToolActivityEvent = {
  conversationId: string;
  messageId: string;
  name: string;
  isError: boolean;
};
export type ChatCitationsEvent = {
  conversationId: string;
  messageId: string;
  citations: ChatCitation[];
};

export function onChatTextDelta(cb: (e: ChatTextDeltaEvent) => void): Promise<UnlistenFn> {
  return listen<ChatTextDeltaEvent>("chat_text_delta", (e) => cb(e.payload));
}
export function onChatToolActivity(cb: (e: ChatToolActivityEvent) => void): Promise<UnlistenFn> {
  return listen<ChatToolActivityEvent>("chat_tool_activity", (e) => cb(e.payload));
}
export function onChatCitations(cb: (e: ChatCitationsEvent) => void): Promise<UnlistenFn> {
  return listen<ChatCitationsEvent>("chat_citations", (e) => cb(e.payload));
}
// Part of the wire contract and emitted by the backend after the assistant
// row is finalised. The Note's ChatPanel drives completion off the chat_send
// promise (it reloads history when the call resolves), so it doesn't subscribe
// here — but the listener is part of the client's event surface for other
// consumers (e.g. a future multi-pane view) and to mirror the cloud contract.
export function onChatDone(cb: (e: ChatDoneEvent) => void): Promise<UnlistenFn> {
  return listen<ChatDoneEvent>("chat_done", (e) => cb(e.payload));
}
export function onChatError(cb: (e: ChatErrorEvent) => void): Promise<UnlistenFn> {
  return listen<ChatErrorEvent>("chat_error", (e) => cb(e.payload));
}

export type TranscriptEvent = { noteId: string; text: string };
export type SummaryEvent = { noteId: string; summary: string };
export type StreamDeltaEvent = { noteId: string; delta: string };
export type RecordingPhase = "idle" | "starting" | "recording" | "paused" | "stopping" | "diarizing" | "importing";
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
