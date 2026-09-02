import { mockIPC } from "@tauri-apps/api/mocks";

type Handler = (args: unknown) => unknown;

// Backend stand-in for component tests. Covers every command the app fires
// at boot with an "empty but healthy" default so any component tree can
// mount; individual tests override per-command via `handlers`.
// `events: true` turns on the mock's own listen/emit bookkeeping, so a test can
// `emit()` a backend event and have the app's real `listen()` handler run. Off
// by default: without it `plugin:event|listen` is answered by the stub below,
// which is all most tests need and avoids the extra machinery.
export function mockTauri(
  handlers: Record<string, Handler> = {},
  options: { events?: boolean } = {},
) {
  mockIPC(async (cmd, args) => {
    if (cmd in handlers) return handlers[cmd](args);

    // Tauri plugin internals (event listeners, app metadata).
    if (cmd === "plugin:event|listen") return 1;
    if (cmd === "plugin:event|unlisten") return undefined;
    if (cmd === "plugin:app|version") return "0.0.0-test";
    if (cmd.startsWith("plugin:updater|")) throw new Error("no updater in tests");
    if (cmd.startsWith("plugin:")) return undefined;

    switch (cmd) {
      case "settings_get": {
        const key = (args as { key?: string } | undefined)?.key;
        return key === "onboarding_completed" ? "true" : null;
      }
      case "notes_list":
      case "notes_list_trash":
      case "folders_list":
      case "clients_list":
      case "summary_prompts_list":
      case "local_whisper_models":
      case "note_audio_files":
      case "note_diagnostics_files":
      case "note_sessions":
      case "note_timeline":
      case "speaker_label_stats":
      case "cloud_speaker_roster":
        return [];
      case "recording_state":
        return "idle";
      case "record_hotkey_get":
        return "Command+Control+KeyR";
      case "note_timeline_repair":
        return { repaired: false, coversTranscript: true };
      case "stored_audio_stats":
        return { notes: 0, files: 0, bytes: 0, noteIds: [] };
      case "chat_get_breadth":
        return "note";
      case "chat_get_owner_filter":
        return "";
      case "permissions_status":
        return { microphone: "granted", screen: "granted" };
      case "cloud_status":
        return {
          configured: false,
          logged_in: false,
          base_url: "",
          user: null,
          current_workspace: null,
          workspaces: [],
          billing_enabled: false,
        };
      case "provider_key_get":
        return null;
      // No embedding server in a test environment — the honest default is the
      // failure a real machine without one gives (#179).
      case "local_llm_embed_probe":
        throw new Error("Couldn't reach http://localhost:11434/v1/embeddings");
      case "diarize_status":
        return { downloaded: false, sizeBytes: 0, path: "" };
      case "app_data_dir":
        return "/tmp/humla-test";
      case "system_arch":
        return "aarch64";
      default:
        return null;
    }
  }, options.events ? { shouldMockEvents: true } : undefined);
}
