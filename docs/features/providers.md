# Summary and chat providers

`providers.rs` holds the one provider list; see `CLAUDE.md` for the registry rules. This file covers the transports.

## OpenAI and OpenAI-compatible local servers (`openai.rs`)

- `summarize_with_base` serves cloud OpenAI and any OpenAI-compatible local server.
- **The cloud path streams** (`summarize_cloud_stream` → SSE via `consume_chat_sse`). A non-streaming reasoning-model call is silent for minutes and a VPN/proxy reaps the idle connection; retries can't fix that.
- `send_chat_with_retries` / `post_chat` retry transient `Kind::Request` send errors twice (500 ms / 1.5 s), covering a pooled HTTP/2 connection the edge has half-closed.
- Reasoning models (gpt-5 and later, o-series) reject `temperature`; `is_reasoning_model()` decides when to omit it.
- `client()` / `local_client()`: 120 s vs 600 s timeouts. `list_models()` (keyed via `list_models_with_key`) is narrowed by `chat_capable_models`. `trim_runaway_repetition()` is the last guard against Qwen content loops.

### Local LLM (Ollama and others)

- Ollama is detected by port `:11434` and routed to `ollama_native_chat` (streaming `/api/chat`), not the `/v1` shim, so `think` is a real toggle. Other local servers stay OpenAI-compatible. The same choice is made for chat (#179); TS mirrors it as `isOllamaUrl`, which only decides what advice to print. The stored provider id is still `"ollama"`.
- `num_ctx` is sized from prompt length + `num_predict`, rounded up to a power of two, bounded `[8192, 65536]`. A flat 65K KV cache OOMed smaller Macs.
- `keep_alive: 0` frees the model after each summary.
- Sampling is model-family-aware (`sampling_profile(model, think)`): Qwen gets `presence_penalty=1.5`, `frequency_penalty=0.5`, `repeat_penalty=1.0` (breaks Qwen 3.5's thinking loop); every other family gets `presence/frequency=0`, `repeat_penalty=1.1`. Only `ollama_chat_stream` (summary) carries this; `ollama_chat_step` (chat tools) uses `temperature=0`.

## Anthropic (`anthropic.rs`, #183)

- Separate module: top-level `system`, tool specs keyed `input_schema`, tool results in a **user** turn, typed SSE content blocks (`AnthropicSseParser`).
- `messages_stream` is the one request; `summarize` and `chat/providers.rs`'s `AnthropicChatAdapter` call it. Send retries go through `openai::send_with_retries`.
- **Never send `thinking`, `temperature`, `top_p` or `top_k`.** Current models reject an explicit `thinking` block.
- No embeddings endpoint: `resolve_embed` returns `None` and chat retrieval is keyword-only.
- Summary model setting: `anthropic_model`; chat reuses `chat_model`.

## GitHub Copilot (#227)

Bills the user's Copilot seat instead of an API key.

- **Sign-in**: Humla's own OAuth app (device flow, `copilot::auth`, client id in `CLIENT_ID`, registered under the Screenpartner-AS GitHub org). GitHub token, refresh token and login are kept in the Keychain (`copilot_credentials`). Tokens last 8 h and refresh without a secret; a refresh GitHub rejects signs the user out. `fresh_token`'s rule is tested against an `OAuthServer` seam.
- **Every call goes through the Copilot CLI** driven by `github-copilot-sdk` (`runtime` feature only; `.cargo/config.toml` sets `COPILOT_SKIP_CLI_DOWNLOAD`, or its build script downloads a 180 MB CLI). Calling the Copilot API directly is not an option: GitHub refuses the `copilot_internal` token exchange to apps it hasn't allow-listed, and the bare token only sees gpt-3.5/4o-mini/4o.
- **Each call is its own SDK session** with Humla's system prompt in `replace` mode and `available_tools` limited to Humla's tools; otherwise the CLI's agent prompt costs ~29k input tokens a call.
- Chat tools are declared without handlers, so a step ends at the `assistant.message` carrying `toolRequests` and `chat::run_chat` drives the loop.
- **One session serves a whole chat turn** (`ChatTurnSession`, held by the per-turn `CopilotChatAdapter`): later steps hand tool results back with `handle_pending_tool_call`, which Copilot reports as `initiator: "agent"` (a continuation, which premium-request billing distinguishes from a user prompt). `continuation` decides whether a step can continue; the forced wrap-up starts a fresh session with the history as one transcript (`render_prompt`).
- Each model call logs one `[copilot] model= initiator= input= output= cost=` line, never content.
- **Finding the CLI** (`copilot::cli`): `COPILOT_CLI_PATH`, Humla's own download, the usual roots, then one login-shell call for its PATH and `npm prefix -g`. npm's `copilot` is a Node loader; Humla runs the platform package's native binary beside it, so `node` is never needed. If none is found, Settings downloads the SDK's version (`cli::VERSION`) from npm, checks a pinned sha512 and GitHub's code signature, and keeps it under Application Support.
- The CLI runs with `--no-auto-update` (a newer CLI may fail the SDK's protocol check), `--disable-builtin-mcps` and `--disable-client-telemetry`.
- A CLI that fails to start keeps its error on `Runtime`; `copilot_status` returns it as `cliError`, and Settings offers the supported download when the failing CLI is another version.
- Models: `copilot_model` (summary, title) and `copilot_chat_model` (chat), default `auto`. Not the shared `chat_model`: Copilot's ids overlap Anthropic's and OpenAI's with different meanings.
- No embeddings: chat retrieval is keyword-only.
- Modules: `auth.rs`, `cli.rs`, `session.rs` (`Runtime`, restarted when binary or token changes; `complete`; chat's `step`), `mod.rs` (Keychain credentials, `Runtime::ready_client`). `commands/copilot.rs` emits `copilot_sign_in` and `copilot_cli_download`.
- Live tests (spend real requests): `cargo test --lib copilot_live -- --ignored` (with `HUMLA_COPILOT_TOKEN`) and `copilot_download_live`.

## Custom OpenAI-compatible endpoint (#173)

A `custom` provider for summaries, chat and transcription.

- One base URL (`custom_base_url`) **used exactly as typed**: Humla appends only the route, so errors name the full URL (`status_error`).
- Optional key in the Keychain (`custom_api_key`); none means no Authorization header.
- A model per surface: `custom_model`, `custom_chat_model`, and the `transcribe_config` entry's `model`.
- **Nothing is guessed about the server**: chat and summary send `model`, `messages`, `stream` (and chat's `tools`) only — no temperature, token limit, reasoning field, Ollama detection or `/v1/responses` fallback (`openai::Endpoint::Custom`, `summarize_custom`, `custom_chat_step`).
- Transcription (`stt/custom.rs`) asks for `verbose_json` word timings; on a 400 it asks that chunk again in plain `json` and remembers the `(URL, model)` for the launch, whose chunks then have no per-word highlighting.
- `local` keeps its stays-on-your-Mac promise; a hosted endpoint never goes through it.
- Chat on it is keyword-only. `custom_list_models` only helps fill the field; Settings' Test (`custom_endpoint::check`) reports the listing and a one-line completion separately. Personal-only: workspace chat runs server-side.
