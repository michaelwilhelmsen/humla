import { mockIPC } from "@tauri-apps/api/mocks";

type Handler = (args: unknown) => unknown;

// Backend stand-in for component tests. Covers every command the app fires
// at boot with an "empty but healthy" default so any component tree can
// mount; individual tests override per-command via `handlers`.
export function mockTauri(handlers: Record<string, Handler> = {}) {
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
      case "summary_prompts_list":
      case "local_whisper_models":
      case "note_audio_files":
      case "note_diagnostics_files":
      case "note_sessions":
      case "note_timeline":
        return [];
      case "recording_state":
        return "idle";
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
      case "diarize_status":
        return { downloaded: false, sizeBytes: 0, path: "" };
      case "app_data_dir":
        return "/tmp/humla-test";
      case "system_arch":
        return "aarch64";
      default:
        return null;
    }
  });
}
