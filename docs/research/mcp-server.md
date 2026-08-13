# Exposing Humla to Claude Code over MCP

Research, 2026-08-13. Question: what's the best way for Humla to speak MCP so
Claude Code (and any other MCP client) can read notes, search transcripts, and
possibly write back?

Verified against the code as it stands on `main` at v0.44.0, the Claude Code MCP
docs (code.claude.com/docs/en/mcp), and `rmcp` 3.1.2 (the official Rust SDK,
updated 2026-08-07, ~20M downloads).

## TL;DR recommendation

Ship it in two steps, and pick the tool contract *once* so step 2 doesn't break
step 1.

1. **`humla-mcp`, a second `[[bin]]` in the existing crate, stdio transport,
   read-only, opening `notes.sqlite` directly.** Reuses `db.rs` and the shape of
   `chat/tools.rs` verbatim. Works whether or not the app is running, needs no
   port, no token, no auth code — the file permissions on
   `~/Library/Application Support/no.humla.app/` *are* the authorization. A
   day's work, mostly wiring.
2. **When writes or actions are wanted, keep the same binary and make it a
   proxy**: the app listens on a **Unix domain socket** in its app-data dir; the
   stdio binary becomes a byte pump between Claude's stdin/stdout and that
   socket, falling back to the direct-SQLite read-only path when the app isn't
   running. `rmcp`'s `AsyncRwTransport::new_server(read, write)` serves any
   `AsyncRead`/`AsyncWrite` pair, so a `UnixStream` needs no custom protocol.

Do **not** open a localhost TCP port. See "Why not localhost HTTP" below.

## What the app already gives us

The hard part — retrieval over notes — is done. `chat/tools.rs` is already an
MCP-shaped tool surface in everything but the wire protocol:

| Existing | Backing | Notes |
|---|---|---|
| `search_notes` | `db::hybrid_search_chunks` (FTS5 + vector, RRF-fused) | Already degrades to keyword-only when `query_vec: None` (#48) |
| `get_note` | `db::get_note` | 6 KB text budget |
| `list_notes` | `db::list_notes_filtered` | 40 rows, 180-char summaries |

Plus the filter vocabulary (`folder_id`, `client_id`, `speaker`,
`within_days`/`until_days`, `mine_only`), the `workspace` tenant argument
threaded through every query, and `Citation { note_id, title, created_at }` —
which maps cleanly onto MCP resource links.

Two properties of that file carry straight over and should be preserved:

- **Tool errors are content, never panics.** MCP clients recover from an error
  result; they don't recover from a dead server.
- **Retrieved note content is framed as reference data, never as instructions.**
  This matters *more* over MCP than in Humla's own chat: a transcript contains
  other people's speech, and an agent with Bash and Edit in the same session is
  a far juicier prompt-injection target than a summarizer. Keep the framing, and
  keep the reminder that a note's text is data.

## Do we need embeddings?

Mostly no. An earlier draft of this doc treated the missing semantic leg as
option A's headline cost; that was wrong, and worth writing down because it
changes how much the Keychain problem matters (barely).

**Why Humla's own chat needs vectors and an MCP client doesn't.** `chat/mod.rs`
retrieves under a step ceiling — `MAX_STEPS_NOTE = 6`, `MAX_STEPS_BROAD = 12` —
so the first query or two has to land, and a small local model is not a
reformulating machine. Claude Code has none of those constraints: it can query
five times, spell the term three ways, `list_notes` and skim, then `get_note` the
plausible ones. Repeated bm25 plus reading beats one good vector query. The
corpus helps too — a personal library is hundreds to low thousands of notes, and
`list_notes` (40 rows, 180-char summaries, date window, folder/client/speaker
filters) makes browsing a real strategy rather than a fallback.

Keyword-only is also **positively** good here: no API key, no per-query cost, no
Keychain prompt, works offline, and deterministic — which is most of what makes
the phase-1 binary a day's work.

**The one genuine hole is cross-language, and it is a general one.** FTS5 is
lexical, so a query in one language against a transcript in another returns
*zero* hits — not worse ranking, nothing. Humla transcribes ~99 languages and a
given user's library may be monolingual in any of them, or mixed; #48's probe
covered four languages plus a cross-lingual case for exactly this reason. So the
severity of this gap is a property of the individual library, not of any
particular language.

That rules out the cheap fix of naming languages in a tool description — there is
no language to name. The right mitigation is to **make language visible as data**:
`notes.language` exists, and `notes.detected_language` (#167) carries the code the
STT provider actually reported for auto-recorded notes. Return it on search hits
and `list_notes` rows, and accept it as a filter. Then the tool description can
state the mechanism once, language-agnostically — the index is lexical, so query
terms should match the language a note is written in — and the agent discovers
which languages its library actually contains by looking, instead of being told a
guess. Conceptual queries with no shared term, and misrecognized proper nouns,
are the same shape of gap and get no help from this.

This is also the strongest argument *for* eventually wiring the semantic leg: both
embedders in play are multilingual, and the more mixed a user's library, the more
the vector path earns its keep. It just isn't a phase-1 blocker.

**And if semantic is wanted, the key isn't the blocker.** `embed.rs` derives the
embedder from the chat provider: OpenAI → `text-embedding-3-small` (needs the
Keychain key), Ollama → `embeddinggemma` with `api_key: None` against a local
`/v1/embeddings`. So on an Ollama-configured library a standalone binary can
embed the query with no key and no prompt. The real constraint is subtler:
`hybrid_search_chunks` filters stored vectors by model id, so the query must be
embedded under **the same model the library was indexed with**. Library on
`embeddinggemma` → free semantic search from any process. Library on
`text-embedding-3-small` → keyword-only unless the app proxies (option C). Either
way the degradation is already built: `query_vec: None` is a supported path, not
an error case.

## The three architectures

### A. Standalone stdio binary reading SQLite directly

```
claude mcp add --scope user humla -- /Applications/Humla.app/Contents/MacOS/humla-mcp
```

**For.** Works with the app closed (the common case — Humla is not a
menu-bar-resident daemon). No listening socket at all, so no auth to design and
nothing for another local process or a webpage to reach. Trivially shipped: an
extra `[[bin]]` in `src-tauri/Cargo.toml` reusing `db.rs`, or a copy under
`~/.local/bin`. SQLite is in WAL mode, so a second reader is safe alongside the
app's writers.

**Against — three real limits, all of them acceptable if the server is
read-only:**

1. **Keyword-only search — which is the right trade, not a wound.** See
   "Do we need embeddings?" below. The short version: an agentic client
   substitutes queries for vector recall, and if semantic *is* wanted the local
   Ollama embedder needs no key at all.
2. **Writes wouldn't reach the cloud, and wouldn't refresh the open app.** Note
   writes normally ping `SyncObserver::note_upserted`, and the frontend learns
   about changes through Tauri events. An external writer does neither: the note
   changes on disk, the UI keeps showing the old copy until reload, and the
   change never uploads. Mitigation exists but is a trap — `sync_outbox` is a
   plain table (`INSERT INTO sync_outbox (entity, entity_id, op, workspace,
   attempts, enqueued_at)`, coalescing on `(entity, entity_id, workspace)`), so
   an outside process *could* enqueue its own sync op. That duplicates a
   coalescing rule and a workspace-resolution rule that live in
   `crates/cloud-sync`, in a second place, for the sake of a write path that
   still leaves the UI stale. Read-only, or proxy (option C).
3. **`db::open` sets no `busy_timeout`.** A concurrent writer today is only ever
   the app itself, holding one `parking_lot` mutex, so nothing has ever needed
   it. A second process changes that: an MCP write (or a long read) can collide
   with the recording pipeline's `append_transcript` and return `SQLITE_BUSY`
   immediately. Set `PRAGMA busy_timeout` in the MCP binary — and arguably in
   `db::open` too, since it costs nothing.

### B. HTTP MCP server embedded in the running Tauri app

`rmcp`'s `StreamableHttpService` (tower/axum) bound to `127.0.0.1:<port>`,
bearer token in a header, added with
`claude mcp add --transport http humla http://127.0.0.1:PORT/mcp -H "Authorization: Bearer …"`.

**For.** Everything option A can't do: writes go through the real command layer
so the sync observer fires and the UI updates live; the Keychain is already
unlocked in-process so semantic search works; and *actions* become possible —
`summarize_note`, `rediarize_note`, even `recording_start`.

**Against.** Only works while the app runs (`claude mcp list` shows a red
`✘ Failed to connect` the rest of the time). And it puts a listening TCP socket
carrying every meeting transcript on the machine: reachable by any local
process regardless of user intent, and by web content via DNS-rebinding-style
attacks unless `Origin`/`Host` are validated as well as the token. Then there's
port allocation to solve, and the token to get into `.mcp.json` (or a
`headersHelper`). This is a lot of security surface bought for the sake of not
writing a 60-line pump.

### C. Unix-socket server in the app + stdio shim (the upgrade path)

The app serves MCP over `AsyncRwTransport::new_server` on a `UnixStream`
accepted from, say,
`~/Library/Application Support/no.humla.app/mcp.sock`. `humla-mcp` connects to
that socket and copies bytes in both directions between it and stdio.

**Why this is the right end state.** It has every advantage of B — command
layer, sync observer, live UI, Keychain, actions — with none of B's exposure:
filesystem permissions authorize the socket (mode 0600 in a per-user directory),
there is no port, and browsers cannot reach it. The client config stays the
plain stdio one-liner from option A, with no token in any file. And the shim can
degrade: socket absent → open SQLite read-only and serve the option-A tool set,
optionally after `open -a Humla` to wake the app.

Cost is one extra hop and two places that speak MCP. Both are small; the tool
implementations live in the app in this design, and the shim has no schema
knowledge at all.

## Tool surface to design once

Names and args should match `chat/tools.rs` where they overlap — same words for
the same concepts, so the mirroring rule that already binds `tools.rs` to
`humla-cloud/chat-service/src/tools.ts` extends to a third file rather than
forking a fourth vocabulary.

**Read (phase 1, all three architectures):**

- `search_notes(query, folder_id?, client_id?, speaker?, language?, within_days?, until_days?, limit?)`
  — hits carry the note's language (`language`, falling back to
  `detected_language`), for the reason in "Do we need embeddings?"
- `get_note(note_id)` — body + summary; `include_transcript` off by default,
  since a full transcript is large and the caller should ask
- `get_transcript(note_id)` — labelled transcript; consider `session` /
  time-range narrowing given the timeline work in #169/#170
- `list_notes(...)` — same filters, and each row carries its language too;
  `list_folders()`, `list_clients()`

**Write / act (phase 2, requires B or C):**

- `create_note(title, body, folder_id?)`
- `append_to_note(note_id, markdown)` — the honest shape for an agent
  contributing to a note; a whole-body overwrite invites clobbering the user's
  own typing
- `set_note_folder`, `set_note_client`
- `summarize_note(note_id)` — start the app's own summary pass

Deliberately **not** exposed: audio files, and anything that deletes. `keep_audio`
is documented as the single absolute gate on audio (#24) and an MCP tool must
not become an exception above it. Deletion has a Trash in the UI and no undo
over a wire protocol.

**Resources.** Claude Code supports MCP resources as `@` mentions
(`@humla:humla://note/<id>`), fuzzy-searchable in the autocomplete, fetched and
attached automatically. Exposing notes as `humla://note/<id>` resources is a
better fit than a tool for "put this meeting in context" and costs almost
nothing once `get_note` exists.

**Server instructions matter more than they used to.** Claude Code defers MCP
tool schemas by default and loads only names plus the server `instructions`
field at session start, then searches for tools on demand. So `ServerInfo`'s
`instructions` is what decides whether Humla's tools get found at all — write it
as "when to reach for this", not as a tool list, and keep it under 2 KB
(descriptions and instructions are both truncated there).

## Team workspaces

A workspace changes two things: the notes are server-authoritative, and
retrieval for workspace chat **already runs server-side** (`/api/chat`, with
`humla-cloud/chat-service/src/tools.ts` doing what `chat/tools.rs` does
locally). So there are two viable teams paths, and they are not exclusive.

### Path 1 — the local stdio server, made workspace-aware

This works with **no server-side work at all**, and it's worth understanding why:

- Workspace notes live in the same local `notes.sqlite`, tagged
  `notes.workspace_id`. Sync pulls the notes the caller is entitled to read, so
  the entitlement question is already answered by the time MCP sees a row.
- `db::note_ids_needing_reindex` and `db::live_note_ids` are **workspace-agnostic**
  — they select every live note. So the startup backfill chunks and FTS-indexes
  teammates' synced notes exactly like personal ones.
- The read side *is* workspace-scoped: `KEYWORD_FROM_WHERE` filters
  `n.workspace_id = ?2`. Resolve the active workspace the way `ChatContext::load`
  does (`cloud::active_workspace`, empty string = Personal) and pass it as the
  workspace argument — a handful of lines, and the local MCP server is correct
  for teams.

Three caveats, all inherent to reading a replica:

1. **Freshness lags twice.** A note is visible when sync has pulled it, and
   *searchable* only once something reindexed it — the startup backfill, a Note
   view unmount, a summarize/diarize checkpoint, or a manual rebuild. Nothing in
   `crates/cloud-sync` calls `reindex_note`, so a teammate's note that arrived
   this morning is fetchable by id but may be missing from `search_notes` until
   the app next launches. If the local path is the one that ships, having the
   pull worker ping `reindex_note_content` is the fix, and it's small.
2. **Keyword only, and here the reason is structural.** Workspace embeddings
   live server-side, so local vectors would mean re-embedding the whole team's
   corpus under a local index — on the user's own key if their chat provider is
   OpenAI. This is the one place where "just use Ollama's keyless embedder"
   doesn't rescue it cheaply, and the one thing path 2 straightforwardly buys.
3. **One workspace at a time** — the active one, mirroring the app. An MCP client
   might reasonably want to search across workspaces; that's a server-side
   answer, not a local one.

### Path 2 — a remote MCP endpoint on humla-cloud

`claude mcp add --transport http humla https://sync.humla.team/mcp`, backed by
the retrieval that already exists in chat-service.

**What it buys.** Complete and immediately fresh coverage (every teammate's note
the moment it lands, with the server's own embeddings, so semantic search comes
back), no dependency on a Mac being awake, and cross-workspace reach if wanted.
It's also the only path that works for a teammate who doesn't run Humla locally.

**Auth is the design work.** PocketBase auth tokens are short-lived JWTs — the
desktop copes with a process-lifetime `SESSION` cache and one-shot 401 re-auth,
which is not something a static `.mcp.json` header can do. Three options,
cheapest first:

- **Personal access tokens** (recommended): a `workspace_tokens` collection, a
  token that carries the *creating user's* identity so existing PocketBase rules
  and the owner/admin/member/viewer roles keep applying, an expiry, and mint /
  revoke in the teams UI. Pasted once as
  `--header "Authorization: Bearer humla_pat_…"`. No refresh problem, revocable,
  auditable.
- **Full OAuth 2.1**: Claude Code discovers RFC 9728 protected-resource metadata
  at `/.well-known/oauth-protected-resource`, falling back to RFC 8414. Best UX
  (browser sign-in, no pasted secret) and the most work.
- **`headersHelper`**: a command Claude Code runs at connect time to print a
  fresh header, which could shell out to the local install's stored credentials.
  Clever, but it couples the remote path to a local Humla — which defeats the
  main reason to have the remote path.

Never a workspace-wide service token. The whole permission model is
per-user-scoped rules; a token without a user behind it discards it.

**Two things to decide deliberately, not by default:**

- **It bypasses the chat meter, correctly but invisibly.** No inference happens
  server-side — Claude Code brings its own model — so an MCP turn consumes no
  metered chat turn and no workspace BYOK key. Only the query embedding costs
  anything. That's the right billing answer, and it also means the entire
  workspace corpus gains an egress path that `/api/chat/usage` cannot see. It
  wants its own counter, a rate limit, and an **owner-level switch** (MCP on/off
  per workspace, list active tokens, revoke).
- **The posture shift is real.** A workspace already means notes on a server, so
  this isn't new in kind — but "any MCP client holding a PAT can read the whole
  team's meeting transcripts from anywhere" is new in degree, for a product that
  sells local-first. Owner visibility and revocation are the honest minimum.

### How they fit together

Ship path 1 first — make the phase-1 stdio binary workspace-aware from the
start, since it costs almost nothing and means teams get *something* the day the
personal version lands. Add path 2 if teams actually ask for coverage a replica
can't give.

Keep the tool names and arguments identical across both, so the client config is
the only difference. That extends the seam the code already runs on:
`chat/tools.rs` mirrors `chat-service/src/tools.ts`, pinned pairwise. An MCP
surface should be a **third mirror of that same vocabulary, not a fourth one** —
and the corollary from the header comment in `tools.rs` applies to it too: a
schema change is a change in every mirror, verified pairwise, or the paths
silently diverge the way `client_id` did before #105.

## Privacy and gating

An MCP server hands an outside agent the contents of meetings — including other
people's speech, in a product whose pitch is that the data stays yours. That
argues for:

- **Off by default**, enabled explicitly in Settings (`developer_mode` is a
  precedent for a hidden-until-asked toggle, though this deserves its own).
- **A scope on the grant**, not a binary: read-only vs read-write, and
  optionally one folder rather than the library. `ToolScope` in `chat/tools.rs`
  is the existing model for a breadth clamp enforced below the tool args.
- **Workspace honesty.** Every `db` query takes a `workspace` argument. The MCP
  server must resolve the same active workspace the app would, so a Personal
  session can't read workspace notes and vice versa.
- **No query logging.** Same rule as chat: don't retain what was searched.

## Sources

- Claude Code MCP reference — https://code.claude.com/docs/en/mcp
  (transports and `claude mcp add` forms, scopes, `@` resource mentions, tool
  search and the 2 KB instructions budget)
- `rmcp` 3.1.2 — https://docs.rs/rmcp (`#[tool_router]` / `#[tool]` macros,
  `serve_server`, `AsyncRwTransport::new_server`, `StreamableHttpService`)
- In-tree: `src-tauri/src/chat/tools.rs`, `src-tauri/src/db.rs`,
  `src-tauri/src/sync.rs`, `src-tauri/crates/cloud-sync/src/lib.rs`
