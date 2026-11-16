# MCP server, webhook, record links and Save audio

User guides live at humla.team/docs/integrations (written in the `humla-landing` repo). None of these features returns, sends or references audio except Save audio, which copies out what `keep_audio` already let the device store; `keep_audio` stays the single gate on audio (#24).

## MCP server (#172)

A second binary (`src/bin/humla-mcp.rs`) built from this crate and shipped at `Humla.app/Contents/MacOS/humla-mcp`. It opens `notes.sqlite` directly, so it works whether or not the app runs; filesystem permissions are the authorization.

- Six read-only tools: `search_notes`, `get_note`, `get_transcript`, `list_notes`, `list_folders`, `list_clients`. Off until enabled (`mcp_enabled`, Settings → General → Integrations, which also offers a config snippet per client; `mcp_server_path` gives the binary's path).
- `mcp/tools.rs` is the seam: `execute(conn, workspace, name, args, now_ms)`. `server.rs` adapts it to `rmcp`'s `ServerHandler` and holds no logic.
- **The workspace is resolved, never accepted as an argument**, so Personal and workspace notes can't reach each other.
- A parallel module to `chat/tools.rs`, not a reuse: chat's specs are pinned against humla-cloud and terse for small models. The three overlapping tools' names and argument names are pinned by a test, which lists every deliberate difference.
- Keyword-only search (`query_vec: None`): no API key, no Keychain prompt, no network. It spans typed body, summary and transcript and tags each excerpt with its source.
- Language is data: `lang:` on every hit and row, `language` as a filter. A tool description ships identically to every user, so it may name no language (`LANGUAGE_NOTE`). An empty search instead names the languages the scope holds (`db::languages_in_scope`, counted, most common first); a listing does so only under a `language` filter.
- Date windows: relative (`within_days` / `until_days`) or absolute (`since` / `until`, `YYYY-MM-DD`, inclusive). `resolve_filter` owns the rule: a malformed date errors rather than widening, two forms of one edge conflict, and an empty result echoes the resolved window (`window_echo`).
- The by-id header carries what a listing row carries (client, folder, who spoke). `get_note` takes `note_ids` (up to ten): a batch divides one note's budget, refuses `include_transcript`, and names a missing id instead of failing.

## Send to webhook (#213)

⋯ → **Send to webhook…** POSTs one note as JSON to a URL the user chose. Manual every time: no automatic send on stop or summary. It lives in the ⋯ menu beside Export…, not the toolbar.

- `SendToWebhookModal` shows the receiver's **host only**, never path or query (they often carry a token).
- Opt-in twice: `webhook_enabled` (SQLite setting, so the menu can ask without the Keychain) and the endpoint in the Keychain (`webhook_config`, via `crate::keychain`), saved by its own button in Settings → General → Integrations; the form isn't loaded until the switch is on.
- **Header values are write-only**: `webhook_get_endpoint` returns header names; a header saved with no value keeps the stored one (`merge`).
- The backend sends from the database, so the view awaits `flushEdits` first (`sendNoteToWebhook`, `src/lib/webhook.ts`).
- The payload (`build_payload`) is a contract: an explicit allowlist under `schema_version: 1`, documented at humla.team/docs/integrations/webhooks (`content/docs/` in `humla-landing`) and pinned by a test listing its keys. Fields may be added; existing ones keep their meaning. Folder and client go as `{ id, name }`; `language` via `summary::note_language`; `recording` covers takes with index > 0 only, with totals `null` unless every take's time is known.
- One request, no retries, no redirects, 30 s. No error carries the URL or the receiver's response.

## Record from a link (#91)

`humla://record/start`, `humla://record/start?name=…`, `humla://record/stop` (`deep_link.rs`).

- A start link always records into a new note through #21's headless path and never calls `show_main_window`.
- A link does only what the tray could: `deep_link::decide` (exhaustive over `Phase`) is pinned to `tray_menu_state` by a test.
- A start **claims** the tray's Start under the phase lock (`menubar::claim_start`, also used by the tray and hotkey), because `recording_start` marks the slot only after slow checks.
- No confirmation in Humla. Nothing a link does is silent: a start posts a notification naming its note, a stop marks the recording for the "saved" notification (`announce_when_saved`), and a link that does nothing says why (including a start while the last take is still saving, #182).
- **Recording links wait for the frontend** (`deep_link::Held`): the frontend calls `deep_links_ready` once its `recording_status` listener is attached; a 10 s fallback releases them. Setup also reads the plugin's current link, since the launch link can arrive before the listener exists.
- `parse` accepts every spelling `parseDeepLink` accepts. `title_from_name` makes a one-line title of at most 200 chars; `name` is a user-owned title that #90 leaves alone unless it has a generated title's shape.

## Save audio (#224)

⋯ → **Save audio…** writes one file per note to wherever the user picks.

- It joins every take's playback audio (`playback.m4a`, or `playback.wav` on pre-#226 takes, decoded to WAV first) in take order with `wav::concat_pcm16_mono_16k`. That copies samples without decoding, refuses anything not 16 kHz mono 16-bit, and builds in a temp dir before copying over the destination. Index 0 ("Earlier transcript") is not a take.
- M4A (AAC via `/usr/bin/afconvert`) is the default; WAV the alternative.
- Its own sheet, `SaveAudioModal`, lists each take with its length, **Missing** for one whose audio is gone or would be refused by the join, and a total (`note_audio_export_info`, one `TakeAudio` per take). A note with no audio still gets the sheet with the reason, not a greyed menu item.
- Disabled while a capture or replay on the note may still be writing a take.
