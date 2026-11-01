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
  /** Every member must have two-factor authentication to open it. */
  require_2fa?: boolean;
};

/** A workspace named and nothing more: all a member is told about one that
 *  requires two-factor authentication they don't have yet. */
export type WorkspaceRef = { id: string; name: string };

/** The signed-in user's two-factor authentication. `available` is false
 *  on a server without it, and the app then shows none of it. */
export type TwoFactorStatus = {
  available: boolean;
  enabled: boolean;
  recovery_codes_left: number;
  /** Every workspace the user is in that requires it. */
  required_by: WorkspaceRef[];
};

export const TWO_FACTOR_UNAVAILABLE: TwoFactorStatus = {
  available: false,
  enabled: false,
  recovery_codes_left: 0,
  required_by: [],
};

/** Where a password sign-in landed: signed in, or waiting for the code from
 *  the user's authenticator app (`cloudApi.verifyTwoFactor`). */
export type SignInStep =
  | { step: "signed_in"; status: CloudStatus }
  | { step: "two_factor" };

/** The second factor a two-factor route takes. */
export type SecondFactor = { code: string } | { recoveryCode: string };

/** What an authenticator app is set up with. `qr.path` draws its dark modules
 *  one unit apart; the quiet zone is the page's to add. */
export type TwoFactorSetup = {
  secret: string;
  otpauth_uri: string;
  qr: { size: number; path: string } | null;
};

/** How the two-factor commands fail: `reason` is the server's code to branch
 *  on (empty when there is none), `message` what the user reads. */
export type CloudError = { reason: string; message: string };

/** Read any command's rejection as a CloudError. Most commands reject with a
 *  plain string; the two-factor ones with `{ reason, message }`. */
export function cloudError(e: unknown): CloudError {
  if (e && typeof e === "object" && "message" in e) {
    const { reason, message } = e as { reason?: unknown; message?: unknown };
    return { reason: typeof reason === "string" ? reason : "", message: String(message) };
  }
  return { reason: "", message: String(e) };
}

/** "Acme", "Acme and Beta", "Acme, Beta and Gamma". */
export function joinNames(names: string[]): string {
  if (names.length < 2) return names[0] ?? "";
  return `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;
}

function factorArgs(factor: SecondFactor | null) {
  return {
    code: factor && "code" in factor ? factor.code : null,
    recoveryCode: factor && "recoveryCode" in factor ? factor.recoveryCode : null,
  };
}
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
  /** Managed chat add-on config, when the server advertises it (issue #75).
   *  null/absent → self-host or not configured; the add-on pitch is dropped. */
  chat_addon?: ChatAddon | null;
  /** The signed-in user's two-factor authentication; unavailable when signed out. */
  two_factor?: TwoFactorStatus;
  /** Workspaces the user is in but can't open until they set up two-factor
   *  authentication: the server leaves them out of `workspaces`. */
  locked_workspaces?: WorkspaceRef[];
  /** The selected workspace when it is one of `locked_workspaces` — then
   *  `current_workspace` is null although a workspace is selected. */
  locked_current_workspace?: WorkspaceRef | null;
  /** Signed out without signing out: the email of the account whose session
   *  ended, for the sign-in form to ask them back. */
  signed_out_email?: string | null;
};

/** Managed chat add-on advertised in the server's billing config (issue #75). */
export type ChatAddon = {
  available: boolean;
  price_id?: string | null;
  price_cents?: number | null;
  currency?: string | null;
};

/**
 * Which in-app surface sent an owner to Stripe Checkout. Rides onto the
 * subscription metadata (`metadata[source]`), so a trial start is attributable —
 * Stripe on its own only records the session.
 *
 * A union, not a bare string, on purpose: the server ALLOWLISTS the value
 * against /^[a-z0-9_]{1,40}$/ and silently drops anything else rather than
 * failing the checkout, so a typo would cost the datapoint without any error to
 * notice. Every member here must match that pattern.
 */
export type CheckoutSource =
  | "onboarding"
  | "settings_organization"
  | "new_workspace"
  | "note_banner"
  /** The switcher's ambient "Add team" pill, and the export sheet's one-line
   *  hint. Both are aimed at someone who has never gone looking, and both share
   *  a sheet with a deliberate entrance — worth telling apart, because the whole
   *  question a hint raises is whether it produces trials or just noise. */
  | "team_hint_switcher"
  | "team_hint_export"
  /** A `humla://new-workspace?source=…` link from an email or the website. */
  | "email"
  | "web";

export const cloudApi = {
  status: () => invoke<CloudStatus>("cloud_status"),
  configure: (baseUrl: string) => invoke<void>("cloud_configure", { baseUrl }),
  login: (email: string, password: string) =>
    invoke<SignInStep>("cloud_login", { email, password }),
  /** Finish a sign-in that answered with `two_factor`. */
  verifyTwoFactor: (factor: SecondFactor) =>
    invoke<CloudStatus>("cloud_verify_two_factor", factorArgs(factor)),
  /** Start linking an authenticator app. With two-factor authentication on
   *  already this replaces the app, and takes its code or a recovery code. */
  twoFactorSetup: (password: string, factor: SecondFactor | null = null) =>
    invoke<TwoFactorSetup>("cloud_two_factor_setup", { password, ...factorArgs(factor) }),
  /** Confirm the app with its first code. Returns the ten recovery codes, shown once. */
  twoFactorEnable: (code: string) => invoke<string[]>("cloud_two_factor_enable", { code }),
  /** Replace the recovery codes; the old ones stop working. */
  twoFactorRecoveryCodes: (password: string, factor: SecondFactor) =>
    invoke<string[]>("cloud_two_factor_recovery_codes", { password, ...factorArgs(factor) }),
  twoFactorDisable: (password: string, factor: SecondFactor) =>
    invoke<void>("cloud_two_factor_disable", { password, ...factorArgs(factor) }),
  /** Which members have two-factor authentication, by user id. Owner and admins only. */
  twoFactorMembers: (workspaceId: string) =>
    invoke<Record<string, boolean>>("cloud_two_factor_members", { workspaceId }),
  setWorkspaceRequire2fa: (workspaceId: string, require: boolean) =>
    invoke<void>("cloud_set_workspace_require_2fa", { workspaceId, require }),
  /** Write the recovery codes to a .txt file the user picked. */
  saveRecoveryCodes: (path: string, codes: string[]) =>
    invoke<void>("cloud_save_recovery_codes", { path, codes }),
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
  /** Start/resume Stripe Checkout for a workspace; returns a hosted URL to open.
   *  `source` names the surface that sent the owner here — see CheckoutSource.
   *  Required at this seam even though the route treats it as optional: it is
   *  the only place the value can be lost, and losing it is invisible. */
  billingCheckout: (workspaceId: string, source: CheckoutSource) =>
    invoke<string>("cloud_billing_checkout", { workspaceId, source }),
  /** Open the Stripe Customer Portal for a subscribed workspace; returns a URL. */
  billingPortal: (workspaceId: string) =>
    invoke<string>("cloud_billing_portal", { workspaceId }),
  /** Workspace chat-key metadata (member-readable, issue #75). Never the key. */
  chatKeyMeta: (workspaceId: string) => invoke<ChatKeyMeta>("chat_key_meta", { workspaceId }),
  /** Owner-only set/rotate — server test-on-saves and returns fresh metadata. */
  chatKeySet: (workspaceId: string, apiKey: string) =>
    invoke<ChatKeyMeta>("chat_key_set", { workspaceId, apiKey }),
  /** Owner-only set/rotate using the personal OpenAI key from the Keychain —
   *  the key is read in Rust and never enters the webview (issue #75). */
  chatKeySetFromKeychain: (workspaceId: string) =>
    invoke<ChatKeyMeta>("chat_key_set_from_keychain", { workspaceId }),
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
  chat_addon: null,
  two_factor: TWO_FACTOR_UNAVAILABLE,
  locked_workspaces: [],
  locked_current_workspace: null,
  signed_out_email: null,
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

/**
 * Resolve a workspace member id to their display name — plainly, with none of
 * `useOwnerName`'s attribution logic. That one collapses "it's you" and "can't
 * resolve them" to the same `null`, which is right for a "created by" byline
 * (both render nothing) and wrong everywhere the two must be told apart: the
 * authorship chip labels a self-pin "Created by me" and an unresolvable one
 * neutrally, and the chat turn sends the name for the model's disclosure line
 * whether or not the pinned person is the caller (#103).
 */
export function useMemberName(id: string | undefined | null): string | null {
  const member = useCloudStore((s) => (id ? s.members[id] : undefined));
  const myName = useCloudStore((s) => (id && s.status.user?.id === id ? s.status.user : null));
  if (!id) return null;
  if (member) return member.name || member.email || null;
  // The roster excludes the caller on some servers, so fall back to their own
  // account before giving up — a self-pin must not read as a stranger.
  return myName ? myName.name || myName.email || null : null;
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
