// Cloud / teams client layer.
//
// Talks to the `cloud_*` Tauri commands (commands/cloud.rs). All calls degrade
// gracefully when there's no Tauri runtime (e.g. a plain browser preview) or
// when cloud isn't configured — the store falls back to a "disconnected"
// status so the teams UI still renders (showing "Personal" / "Sign in").

import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";

export type CloudRole = "owner" | "admin" | "member";

export type CloudUser = { id: string; email: string; name: string };
export type CloudWorkspace = { id: string; name: string; role: CloudRole };
export type CloudMember = { id: string; email: string; name: string; role: CloudRole };

export type CloudStatus = {
  /** A server URL is configured. */
  configured: boolean;
  /** We hold (or could auto-acquire) a valid session. */
  logged_in: boolean;
  base_url: string;
  user: CloudUser | null;
  current_workspace: CloudWorkspace | null;
  workspaces: CloudWorkspace[];
};

export const cloudApi = {
  status: () => invoke<CloudStatus>("cloud_status"),
  configure: (baseUrl: string) => invoke<void>("cloud_configure", { baseUrl }),
  login: (email: string, password: string) =>
    invoke<CloudStatus>("cloud_login", { email, password }),
  logout: () => invoke<void>("cloud_logout"),
  createWorkspace: (name: string) => invoke<CloudWorkspace>("cloud_create_workspace", { name }),
  selectWorkspace: (id: string) => invoke<void>("cloud_select_workspace", { id }),
  renameWorkspace: (workspaceId: string, name: string) =>
    invoke<void>("cloud_rename_workspace", { workspaceId, name }),
  deleteWorkspace: (workspaceId: string) =>
    invoke<void>("cloud_delete_workspace", { workspaceId }),
  workspaceMembers: (workspaceId: string) =>
    invoke<CloudMember[]>("cloud_workspace_members", { workspaceId }),
  addMember: (workspaceId: string, email: string) =>
    invoke<void>("cloud_add_member", { workspaceId, email }),
  removeMember: (workspaceId: string, userId: string) =>
    invoke<void>("cloud_remove_member", { workspaceId, userId }),
  setMemberRole: (workspaceId: string, userId: string, role: CloudRole) =>
    invoke<void>("cloud_set_member_role", { workspaceId, userId, role }),
};

export const DISCONNECTED: CloudStatus = {
  configured: false,
  logged_in: false,
  base_url: "",
  user: null,
  current_workspace: null,
  workspaces: [],
};

type CloudState = {
  status: CloudStatus;
  /** First status fetch has completed (so the UI can avoid a flash). */
  ready: boolean;
  refresh: () => Promise<void>;
  setStatus: (s: CloudStatus) => void;
};

export const useCloudStore = create<CloudState>((set) => ({
  status: DISCONNECTED,
  ready: false,
  setStatus: (status) => set({ status, ready: true }),
  refresh: async () => {
    try {
      const status = await cloudApi.status();
      set({ status, ready: true });
    } catch {
      // No Tauri runtime / command unavailable → treat as disconnected.
      set({ status: DISCONNECTED, ready: true });
    }
  },
}));

export function roleLabel(role: CloudRole): string {
  return role.charAt(0).toUpperCase() + role.slice(1);
}

/** Token-driven colour for a role pill, reusing the speaker-pill palette. */
export function roleColorVar(role: CloudRole): string {
  switch (role) {
    case "owner":
      return "var(--color-accent)";
    case "admin":
      return "var(--color-warning)";
    default:
      return "var(--color-text-muted)";
  }
}
