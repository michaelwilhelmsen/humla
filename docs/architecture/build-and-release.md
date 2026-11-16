# Build, signing and releases

The command table and release checklist are in `CLAUDE.md`. This file has the detail.

## Mock harness (`pnpm mock`)

`mock.html` + `src/mockBoot.tsx` render a real component against mocked Tauri IPC in a plain browser, so layout and token bugs jsdom can't see are visible. Dev-only; Vite builds `index.html` only.

- `?case=<name>` picks a scenario; `?palette=<id>&theme=light|dark` picks a design. `?case=themes` is a specimen sheet of every token-driven surface.
- `?case=toolbar-*` + `scripts/measure-toolbar.js` (paste into the console) sweep 300–900px across both sidebar gutters and report overflow (`scrollWidth > clientWidth`) and tightest slack. `NoteToolbar`'s thresholds and `BODY_MIN` come from it.
- `?case=recbar-*` + `scripts/measure-recording-bar.js` do the same for the recording bar (#177), sweeping 380–900px with the widest content (90-minute capture, paused, hour-plus timer).
- `?case=notes` / `?case=notes-folder` render the grid views against `src/test/noteLibrary.ts`.
- **A scenario wrapper must mirror the real container** (e.g. the onboarding canvas in `Onboarding.tsx`); a plain `<div>` produces layout bugs that aren't real.

## Other build notes

- `pnpm lint` is a correctness net (no formatter). `react-hooks/rules-of-hooks` catches a hook below an early return, which type-checks and passes unit tests but crashes the view. `exhaustive-deps` is a warning because several omissions are deliberate and commented. Type-aware rules are off; `pnpm build` runs `tsc -b`.
- `./scripts/build-mcp.sh` has no source-hash skip (cargo does that) and writes a zero-byte placeholder first, because `humla-mcp` is a declared `externalBin` and tauri-build fails without the file.
- `cargo run --release -p echo-probe -- …` (from `src-tauri/`): the measurement CLI of the crate whose `delay` and `aec` modules the echo pass runs (#196). Per-window echo lag (GCC-PHAT; level, polarity, drift, steps), split into start offset and output path with a take's `capture-<session_id>.json`; the lag from a mixed `playback.wav`; offline echo cancellation. `session <recordings/<note>/<session>>` does all of it for one take, `selftest` checks against known answers. Refuses to write audio under `no.humla.app/`.

## Dev builds are code-signed

`src-tauri/.cargo/config.toml` points cargo's `runner` at `scripts/dev-sign.sh`, so every `cargo run` (which `tauri dev` uses) signs the debug binary with the release's Developer ID identity **and** `identifier` (`no.humla.app`). Ad-hoc signed binaries get a per-build `cdhash` designated requirement, so the Keychain's "Always Allow" lasted one launch; an identity-based DR is identical across rebuilds and shared with the installed app. ~0.5 s per launch. Fails soft with no identity (CI, fresh clone). Only the binary named `humla` is signed (the runner also fires for `cargo test`).

## Signing and notarisation

- Identity: **Developer ID Application: MICHAEL MEHLUM WILHELMSEN (NBUP88JQ35)** (`bundle.macOS.signingIdentity` in `src-tauri/tauri.conf.json`). Both sidecars get the same identity + hardened runtime.
- Notarytool credentials in `.env.notarise` (gitignored) at the repo root:

  ```
  export APPLE_API_KEY=<10-char Key ID>
  export APPLE_API_ISSUER=<Issuer UUID>
  export APPLE_API_KEY_PATH=/Users/michaelwilhelmsen/.private_keys/AuthKey_<Key ID>.p8
  ```

  `scripts/build-dmg.sh` sources it; Tauri's bundler then runs `xcrun notarytool submit --wait` and the stapler. Without it the build is signed but not notarised (first launch needs right-click → Open).
- Verify: `spctl --assess -vv /Applications/Humla.app` → `accepted, source=Notarized Developer ID`.
- Failure logs:

  ```
  xcrun notarytool log <submission-id> \
    --key $APPLE_API_KEY_PATH \
    --key-id $APPLE_API_KEY \
    --issuer $APPLE_API_ISSUER \
    | jq
  ```

  Common causes: nested binary missing hardened runtime, missing entitlement, wrong identifier on a Framework, executable bit lost during copy.

## Updater signing key

A separate Ed25519 keypair signs the update payload.

- Private key: `~/.private_keys/humla-updater.key` (passwordless, ~700 perms). Losing it means existing installs accept no further updates. The build script reads its path from `.env.notarise` (`TAURI_SIGNING_PRIVATE_KEY`).
- Public key: `plugins.updater.pubkey` in `src-tauri/tauri.conf.json`. Never change it once shipped.

## What `pnpm release` does

1. Refuses a dirty tree or a version not above the latest GitHub release, and blocks when the version has no complete entry in `src/content/releases.ts`.
2. Prunes `src-tauri/target/` with `cargo sweep --installed` (skipped with a note if `cargo-sweep` is missing — `cargo install cargo-sweep` — and under `SKIP_BUILD=1`).
3. Builds the DMG (`pnpm dmg`), signs, notarises, staples, produces a `.sig` with the updater key.
4. Resolves artifacts **by version and payload, never by newest-on-disk**: the DMG by name (`Humla_<version>_*.dmg`, exactly one match), the updater tarball by the `CFBundleShortVersionString` inside it. A failed DMG step leaves the previous release's tarball in place; picking it would publish an update that moves every install backwards with a valid signature. `SKIP_BUILD=1` reuses artifacts on disk and is where this would happen. Then writes `latest.json`.
5. Tags `v<version>`, pushes the tag, creates the GitHub release, uploads `.dmg` + `.sig` + `latest.json`.

Installs poll the updater endpoint at startup.
