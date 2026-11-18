# Cloud session, two-factor sign-in and private notes

Server contracts: `humla-cloud/docs/two-factor.md` and humla-cloud#47.

## Session and two-factor authentication (#221)

- `commands/cloud_auth.rs` owns the session. The token is kept in the Keychain (`cloud_credentials`, beside the email) and renewed with `auth-refresh`; `renew` is the rule, tested against a fake `AuthServer`.
- **The password is a fallback only, dropped the first time the server answers `two_factor_required`.** Each password sign-in of a 2FA account leaves a server challenge no background task can answer. An account with 2FA on stores no password (enable drops it, disable keeps it again); a session that ends asks the user back (`signed_out_email`) instead of retrying.
- **The sync worker never signs in.** `cloud_sync::Config` carries a `Session` the app lends it (`AppSession` in `cloud_worker.rs`); a refused token goes back through `rejected`, and the worker reports `signed_out` / `two_factor_required`, not `error`.
- Sign-in is two-step on every surface through one hook and one form (`useCloudSignIn`, `TwoFactorCodeForm`): `cloud_login` answers `{ step: "two_factor" }` and keeps the challenge in Rust, `cloud_verify_two_factor` finishes it, `challenge_expired` sends the form back to the password.
- 2FA commands reject with `{ reason, message }` (`CloudError`, read in TS by `cloudError`), worded by `two_factor_error`.
- A workspace that requires 2FA is hidden from a member without it. `cloud_status` reports `locked_workspaces` (and `locked_current_workspace`); the switcher shows it locked, leading to Settings → Account.
- **A change made while locked is never dropped** (`requires_two_factor` in the worker): a refused push is the rule, not the payload, and a deletion that finds nothing to tombstone found a hidden record.
- A server without the 2FA routes may answer with its web page, which reads as no requirement.
- Setup's QR code is drawn in Rust (`qr_code`, `qrcodegen`) as an SVG path, so the page injects no markup. Recovery codes are saved through `cloud_save_recovery_codes`, which refuses anything that isn't one.

## Private notes in a workspace (#191, #232)

- `notes.private`, changed from a lock chip in the note meta bar (and shown as a lock on the card). Rendered only for a note that has a workspace.
- Not derived: it syncs and bumps `updated_at`, because it records a decision.
- Default is shared, and sticky per workspace: the last visibility chosen there becomes the next note's default (`note_private_default:<workspace_id>`). The menu-bar recorder reads the same rule.
- **Only the author may change it**; otherwise any member could withdraw someone else's note.
- **Sharing with chosen members** (#232): `notes.shared_with` (user ids; empty = author only; ignored on a shared note), set from the Share popover (`NoteAccessPicker`, labels in `src/lib/noteAccess.ts`) through `notes_set_access`, author only. The note is withdrawn only when someone loses access (`access_narrowed`). A member it is shared with sees who has access, without the controls.
- A private note shared with anyone takes the recording lock again; one only its author reads takes none.

### Revocation

Tightening the server rule stops future reads only; a record leaving your view produces no pull event, so a teammate keeps their local copy unless told.

- `note_revocations` (member-readable: id and timestamp, no content) is the only signal. `note_withdrawn` on the `SyncObserver` carries the transition.
- Since #232 a revocation doesn't mean gone for everyone, so a pulled revocation is **queued** (`pending_revocations:<workspace>` in `sync_state`) and settled by asking the server whether it still lists the note for this member. Only an empty listing deletes; a failed request or a 2FA-locked workspace keeps it queued.
- Applying one is the only pull path that destroys, guarded three ways: same workspace, non-empty `owner`, and an owner that is not us. It takes the derived chunks with it (as `db::purge_note` does, ADR-0002) and every file via `purge_note_assets`; the crate hands the id back on its `purged` callback.

### The chat index

humla-cloud's indexer reads with a service token across every workspace, which no collection rule constrains. So `private` and `owner` ride on every chunk and `IndexStore.viewerSql` is composed by every retrieval path (search, match count, both speaker listings). The viewer is optional in the type and **fail-closed**: omitted, it matches only public chunks.

## Managed configuration (organisation layer, C6)

An MDM configuration profile for the domain `no.humla.app` pins policies on a Mac with no sign-in, and no sign-out releases them (spec: `humla-cloud/docs/organisation-layer.md` §4 option C). `org_policy::managed()` reads it through CFPreferences and `effective()` merges it strictest-wins with the cached server config, as one more `Layer` whose source is **"This Mac's configuration"** (`MANAGED_SOURCE`, mirrored in `src/lib/orgPolicy.ts`).

Flat keys in the payload, named after the server's fields:

| Key | Type | Effect |
|---|---|---|
| `local_only` | boolean | Policy on/off |
| `telemetry_off` | boolean | Policy on/off; stops first-run telemetry during onboarding |
| `audio_forbidden` | boolean | Policy on/off |
| `policy_reason` | string | Shown with the managed rules |
| `server_url` | string | The cloud server; the user can't change or disconnect it |

- **Only forced values count** (`CFPreferencesAppValueIsForced`): a user's own `defaults write no.humla.app …` is ignored.
- **Fail closed**: a policy key forced to anything but a boolean (`<string>false</string>`, an integer) reads as **on**. A `server_url` that isn't an http(s) URL is ignored with a log line.
- `retention_days` is **not read** from a profile. Retention covers organisation workspaces only, which a profile can't name, and `retention_in_force` reads only the server cache.
- Read once on first use and again on every `cloud_status` (sign-in and window focus) and every MCP call (`mcp::refusal`), after `CFPreferencesAppSynchronize`; which also moves settings to their enforced values (`apply_policy_to_settings`) and emits `org_policy_changed` when the layer changed. At launch, before the sync observer exists, `apply_managed_at_launch` does the same.
- `server_url` is pinned by writing it into `cloud_base_url`, so every reader stays unchanged; when that changes the setting, the whole stored account (email, password, token) is dropped (`org::pin_managed_server` → `cloud_auth::forget_account`), since a password left behind would sign in to the new server. `cloud_configure` refuses any other URL, and `cloud_status.server_managed` hides Change server and Disconnect.
- A profile that forces `audio_forbidden` to `true` (not a mistyped value) owes the same one-time sweep and notice as an organisation, recorded in `org_audio_swept` as `mac-configuration` (`MANAGED_SWEEP_ID`).
- Tests never read the Mac's profiles (under `cfg(test)` the cache and `read_forced` are per thread, set by `force_for_test`); the parsing is `parse_managed` over a map of forced values. Linux builds read nothing.

A sample profile: [`no.humla.app.mobileconfig`](no.humla.app.mobileconfig).
