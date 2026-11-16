# Chat over your notes

An agentic retrieval loop (`chat/`), shown as the Chat tab of the Note right panel, at the library-wide `/chat` route, and per folder at `/folder/:id/chat` (#110).

## Loop and tools

- `chat/mod.rs`: `run_chat`, `SYSTEM_PROMPT` + `system_prompt_with_date`, step ceilings `MAX_STEPS_NOTE = 6` / `MAX_STEPS_BROAD = 12` (picked by `max_steps_for`), `build_grounding` + `GROUNDING_CHAR_BUDGET = 24_000`, `assemble_prompt`, `validate_breadth`, typed message `Part`s, `derive_title`.
- `adapter.rs`: `ChatAdapter::step`, one streamed step offering tools. `providers.rs`: OpenAI, local and Anthropic adapters.
- `tools.rs`: `search_notes` (`db::hybrid_search_chunks`, FTS5 + semantic; reports how many notes matched as well as how many were returned), `get_note`, `list_notes` (per-note summary line and Client; relative window `within_days` / `until_days`), `ToolScope` breadth clamp.
- `search_notes` / `get_note` results carry citations; `list_notes` does not (a listing is an index, not a source). The answer cites notes as chips that navigate to them.
- Retrieved content is framed as reference data, never instructions.
- `cloud.rs`: workspace turns POST `/api/chat` and re-emit SSE onto the same `chat_*` events. **Workspace retrieval runs server-side**, so `tools.rs` is Personal-only and changes must be mirrored in humla-cloud; tool schemas are pinned pairwise in both repos.

## Filters on the conversation row

They bind every turn whatever the model asks:
- **Breadth** (`note` | `folder` | `all`).
- **Authorship pin** (`owner_filter`, a user id; workspace-only, the "Created by me" toggle).

## Scope and drafts

- **A conversation's scope is `(tenant, scope, scope_id)`**: `note` (note id), `folder` (folder id) or `global` (a fixed sentinel). The populations never leak into each other, locally or server-side.
- Breadth is a live filter within a thread. A note-less target is **pinned** to its own reach (`all` for `/chat`, `folder` for a folder chat): `pinned_breadth` (Rust) / `targetPinsScope` (TS).
- Deleting a folder hard-deletes its conversations; its notes only reparent, so the Sidebar confirms only when threads would be lost.
- `ChatTarget::from_ids` takes note id and folder id as alternatives: both at once is an error, and an empty id is an error.
- **Opening a pane drafts or resumes** (`resumes_on_open` in Rust, `targetResumesOnOpen` in TS; change both). `/chat` drafts: an unsaved conversation, nothing persisted until the first turn, and "+" is a local reset. A note's Chat tab resumes that note's most recent thread.
- A drafting pane's breadth and pin live in the pane and ride on `chat_send` (`DraftSettings`).
- Conversation lists hide zero-message rows for drafting targets only: in SQL for Personal, off the server's `message_count` for a workspace.
- A conversation can be renamed or hard-deleted (with a confirm; there is no Trash for chat) from the sidebar row menu, `/chat`'s ⋯, or the note pane's history popover.

## Providers and embeddings

- Chat has its own provider settings (Settings → Chat). The local provider is stored as `"ollama"` but is not Ollama-only (#179): `:11434` uses `/api/chat`, everything else OpenAI-compatible, the same choice `summarize_with_base` makes.
- The embedder has its own address and model (`embed_base_url` / `embed_model`; blank = the chat server and `embeddinggemma`), because the chat server often can't embed (`mlx_lm.server` has no `/v1/embeddings` route). Settings probes it with a real one-word embedding, not a model listing.
- `chunk_embeddings` is keyed `(text_hash, model)` and search filters on the model, so naming a different embedder makes existing vectors unreachable. `chat_unembedded_note_count` counts the notes affected; `chat_embed_missing` runs the backfill.
- `localChatHint` is the one readiness ladder behind both the Settings tab and the Note pane's setup prompt.
- Anthropic and Copilot have no embeddings: keyword-only retrieval.
