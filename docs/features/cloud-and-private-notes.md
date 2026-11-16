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
