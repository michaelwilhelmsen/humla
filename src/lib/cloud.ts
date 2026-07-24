// Cloud / teams client layer.
//
// Talks to the `cloud_*` Tauri commands (commands/cloud.rs). All calls degrade
// gracefully when there's no Tauri runtime (e.g. a plain browser preview) or
// when cloud isn't configured — the store falls back to a "disconnected"
// status so the teams UI still renders (showing "Personal" / "Sign in").

import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import type { SyncStatus } from "./ipc";

/**
 * The hosted Humla Cloud endpoint — the one-click "Use Humla Cloud" button in
 * Account points here so downloaders don't have to know a URL. Set this to your
 * deployed server's address once Humla Cloud is live.
 */
export const HUMLA_CLOUD_URL = "https://sync.humla.team";

export type CloudRole = "owner" | "admin" | "member" | "viewer";
export type PlanStatus = "active" | "trialing" | "past_due" | "canceled" | "none";

export type CloudUser = { id: string; email: string; name: string; verified: boolean };
export type CloudWorkspace = {
  id: string;
  name: string;
  role: CloudRole;
  plan_status: PlanStatus;
  /** Billed seats (= workspace members). Absent on servers predating per-seat billing. */
  seats?: number | null;
};
export type CloudMember = { id: string; email: string; name: string; role: CloudRole };

/**
 * Workspace chat-key metadata (BYOK, issue #75). The key value is never exposed
 * — only this metadata. `keyHealth` ("ok" | "failing" | …) drives the
 * degradation warning; `setBy` is a user id resolved to a name in the UI.
 */
export type ChatKeyMeta = {
  configured: boolean;
  last4: string | null;
  setBy: string | null;
  setAt: string | null;
  keyHealth: string | null;
};

export type CloudStatus = {
  /** A server URL is configured. */
  configured: boolean;
  /** We hold (or could auto-acquire) a valid session. */
  logged_in: boolean;
  base_url: string;
  user: CloudUser | null;
  current_workspace: CloudWorkspace | null;
  workspaces: CloudWorkspace[];
  /** True when the server enforces billing (humla-cloud); false on self-host. */
  billing_enabled: boolean;
  /** Per-seat price in cents (Stripe unit_amount, e.g. 500 = $5.00). Absent on older servers. */
  seat_price_cents?: number | null;
  /** Lowercase ISO currency for seat_price_cents (e.g. "usd"). Absent when unknown. */
  seat_currency?: string | null;
};

export const cloudApi = {
  status: () => invoke<CloudStatus>("cloud_status"),
  configure: (baseUrl: string) => invoke<void>("cloud_configure", { baseUrl }),
  login: (email: string, password: string) =>
    invoke<CloudStatus>("cloud_login", { email, password }),
  signup: (email: string, password: string, name: string) =>
    invoke<CloudStatus>("cloud_signup", { email, password, name }),
  /** Resend the email-verification message to the signed-in user. */
  resendVerification: () => invoke<void>("cloud_resend_verification"),
  /** Email a password-reset link (signed-out; 204 even for unknown emails). */
  requestPasswordReset: (email: string) =>
    invoke<void>("cloud_request_password_reset", { email }),
  logout: () => invoke<void>("cloud_logout"),
  createWorkspace: (name: string) => invoke<CloudWorkspace>("cloud_create_workspace", { name }),
  selectWorkspace: (id: string) => invoke<void>("cloud_select_workspace", { id }),
  renameWorkspace: (workspaceId: string, name: string) =>
    invoke<void>("cloud_rename_workspace", { workspaceId, name }),
  deleteWorkspace: (workspaceId: string) =>
    invoke<void>("cloud_delete_workspace", { workspaceId }),
  leaveWorkspace: (workspaceId: string) =>
    invoke<void>("cloud_leave_workspace", { workspaceId }),
  transferWorkspace: (workspaceId: string, newOwnerId: string) =>
    invoke<void>("cloud_transfer_workspace", { workspaceId, newOwnerId }),
  workspaceMembers: (workspaceId: string) =>
    invoke<CloudMember[]>("cloud_workspace_members", { workspaceId }),
  pendingNoteIds: () => invoke<string[]>("cloud_pending_note_ids"),
  addMember: (workspaceId: string, email: string) =>
    invoke<void>("cloud_add_member", { workspaceId, email }),
  /** Invite by email. Returns "added" (had an account) or "invited" (pending). */
  inviteMember: (workspaceId: string, email: string, role: CloudRole = "member") =>
    invoke<string>("cloud_invite_member", { workspaceId, email, role }),
  removeMember: (workspaceId: string, userId: string) =>
    invoke<void>("cloud_remove_member", { workspaceId, userId }),
  setMemberRole: (workspaceId: string, userId: string, role: CloudRole) =>
    invoke<void>("cloud_set_member_role", { workspaceId, userId, role }),
  /** Start/resume Stripe Checkout for a workspace; returns a hosted URL to open. */
  billingCheckout: (workspaceId: string) =>
    invoke<string>("cloud_billing_checkout", { workspaceId }),
  /** Open the Stripe Customer Portal for a subscribed workspace; returns a URL. */
  billingPortal: (workspaceId: string) =>
    invoke<string>("cloud_billing_portal", { workspaceId }),
  /** Workspace chat-key metadata (member-readable, issue #75). Never the key. */
  chatKeyMeta: (workspaceId: string) => invoke<ChatKeyMeta>("chat_key_meta", { workspaceId }),
  /** Owner-only set/rotate — server test-on-saves and returns fresh metadata. */
  chatKeySet: (workspaceId: string, apiKey: string) =>
    invoke<ChatKeyMeta>("chat_key_set", { workspaceId, apiKey }),
  /** Owner-only remove — returns the unconfigured metadata. */
  chatKeyRemove: (workspaceId: string) => invoke<ChatKeyMeta>("chat_key_delete", { workspaceId }),
};

export const DISCONNECTED: CloudStatus = {
  configured: false,
  logged_in: false,
  base_url: "",
  user: null,
  current_workspace: null,
  workspaces: [],
  billing_enabled: false,
  seat_price_cents: null,
  seat_currency: null,
};

type CloudState = {
  status: CloudStatus;
  /** First status fetch has completed (so the UI can avoid a flash). */
  ready: boolean;
  /** Members of the active workspace, keyed by user id — for owner attribution. */
  members: Record<string, CloudMember>;
  /** Live sync state from the worker; null when not syncing (Personal/signed out). */
  syncStatus: SyncStatus | null;
  /** Note ids with an unpushed change queued — for a per-note "syncing…" hint. */
  pendingNoteIds: Set<string>;
  refresh: () => Promise<void>;
  setStatus: (s: CloudStatus) => void;
  setSyncStatus: (s: SyncStatus | null) => void;
  refreshPending: () => Promise<void>;
};

export const useCloudStore = create<CloudState>((set) => ({
  status: DISCONNECTED,
  ready: false,
  members: {},
  syncStatus: null,
  pendingNoteIds: new Set(),
  setStatus: (status) => set({ status, ready: true }),
  setSyncStatus: (syncStatus) => set({ syncStatus }),
  refreshPending: async () => {
    try {
      set({ pendingNoteIds: new Set(await cloudApi.pendingNoteIds()) });
    } catch {
      set({ pendingNoteIds: new Set() });
    }
  },
  refresh: async () => {
    try {
      const status = await cloudApi.status();
      set({ status, ready: true });
      // Load the active workspace's members so notes can resolve their owner id
      // to a display name. Cleared in Personal / signed-out.
      const wsId = status.current_workspace?.id;
      if (wsId) {
        try {
          const list = await cloudApi.workspaceMembers(wsId);
          set({ members: Object.fromEntries(list.map((m) => [m.id, m])) });
        } catch {
          set({ members: {} });
        }
      } else {
        // Personal / signed out → no members, no sync indicator.
        set({ members: {}, syncStatus: null });
      }
    } catch {
      // No Tauri runtime / command unavailable → treat as disconnected.
      set({ status: DISCONNECTED, ready: true, members: {}, syncStatus: null });
    }
  },
}));

/**
 * Resolve a note's `owner` id to a display name for "created by" attribution,
 * using the active workspace's member list. Returns null when the note is
 * yours, has no owner, or the owner can't be resolved (e.g. a former member) —
 * callers should render nothing in those cases.
 */
export function useOwnerName(ownerId: string | undefined | null): string | null {
  const me = useCloudStore((s) => s.status.user?.id);
  const member = useCloudStore((s) => (ownerId ? s.members[ownerId] : undefined));
  const rosterLoaded = useCloudStore((s) => Object.keys(s.members).length > 0);
  if (!ownerId || ownerId === me) return null;
  if (member) return member.name || member.email;
  // Owner isn't in the current roster. If the roster is loaded, they've left the
  // workspace — label it rather than rendering nothing (which would read as
  // "yours"). If the roster isn't loaded yet, don't guess.
  return rosterLoaded ? "Former member" : null;
}

export function roleLabel(role: CloudRole): string {
  return role.charAt(0).toUpperCase() + role.slice(1);
}

/**
 * Format a per-seat price (in cents, Stripe `unit_amount`) as a currency string.
 * Drops the fraction when the amount is a whole unit: 500 → "$5", 550 → "$5.50".
 * Falls back to USD when the currency is unknown.
 */
export function formatSeatPrice(cents: number, currency?: string | null): string {
  const whole = cents % 100 === 0;
  return new Intl.NumberFormat("en-US", {
    style: "currency",
    currency: (currency || "usd").toUpperCase(),
    minimumFractionDigits: whole ? 0 : 2,
    maximumFractionDigits: whole ? 0 : 2,
  }).format(cents / 100);
}

/** Token-driven colour for a role pill, reusing the speaker-pill palette. */
export function roleColorVar(role: CloudRole): string {
  switch (role) {
    case "owner":
      return "var(--color-accent-text)";
    case "admin":
      return "var(--color-warning-text)";
    default:
      return "var(--color-text-muted)";
  }
}
