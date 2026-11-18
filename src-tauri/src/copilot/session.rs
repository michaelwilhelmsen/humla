//! One completion through the Copilot CLI: the summary, the title, and chat.
//! Every session gets Humla's system prompt in place of the CLI's agent prompt
//! and no built-in tool, so the model is sent the prompt and nothing else.
//! Chat's tools are declared without handlers: a step ends at the assistant
//! message that requests them, `chat::run_chat` runs them as for every
//! provider, and the next step hands the results back to the same session.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use copilot_sdk::handler::ApproveAllHandler;
use copilot_sdk::rpc::HandlePendingToolCallRequest;
use copilot_sdk::subscription::EventSubscription;
use copilot_sdk::session_events::{AssistantMessageData, AssistantMessageToolRequest};
use copilot_sdk::rpc::ModelPolicyState;
use copilot_sdk::types::{CreateSessionOptions, SystemMessageConfig};
use copilot_sdk::types::{RequestId, SessionEvent};
use copilot_sdk::session::Session;
use copilot_sdk::{
    CliProgram, Client, ClientMode, ClientOptions, RuntimeConnection, SpawnedProcessOptions, Tool,
};

use crate::chat::adapter::{CancelFlag, ChatStep, ChatTurn, ToolCall, ToolSpec};
use crate::openai::StreamChunk;

/// What every model call through Copilot may take before it is given up on.
const CALL_TIMEOUT: Duration = Duration::from_secs(600);

/// The CLI started but speaks a protocol version the SDK doesn't.
#[derive(Debug)]
pub struct WrongProtocol(pub String);

impl std::fmt::Display for WrongProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for WrongProtocol {}

pub fn is_version_mismatch(e: &copilot_sdk::Error) -> bool {
    matches!(
        e.kind(),
        copilot_sdk::ErrorKind::Protocol(copilot_sdk::ProtocolErrorKind::VersionMismatch { .. })
    )
}

/// The running CLI, kept between calls: starting one takes a second or two.
/// A new one starts when the binary or the token changes; the old one lives on
/// until the calls still holding it finish.
#[derive(Default)]
pub struct Runtime {
    running: tokio::sync::Mutex<Option<Running>>,
    pub(super) cli_path: tokio::sync::Mutex<Option<PathBuf>>,
    /// Held across a token refresh: GitHub rotates the refresh token, so a
    /// second refresh with the same one is refused and would sign the user out.
    pub(super) refreshing: tokio::sync::Mutex<()>,
    /// Why the CLI last failed to start, until it next starts.
    start_error: parking_lot::Mutex<Option<String>>,
}

struct Running {
    cli: PathBuf,
    token: String,
    client: Client,
}

impl Runtime {
    pub async fn client(&self, cli: &Path, token: &str) -> anyhow::Result<Client> {
        let mut running = self.running.lock().await;
        if let Some(r) = running.as_ref() {
            if r.cli == cli && r.token == token && r.client.ping(None).await.is_ok() {
                return Ok(r.client.clone());
            }
        }
        // Spawned over stdio: 2.0 otherwise loads the runtime into Humla's own
        // process, and Humla only ever drives the CLI as a child.
        let process = SpawnedProcessOptions::new()
            .with_program(CliProgram::Path(cli.to_path_buf()))
            .with_working_directory(std::env::temp_dir())
            .with_args([
                // The SDK refuses a CLI outside its protocol range, so the CLI
                // must not swap itself for a newer download.
                "--no-auto-update",
                "--disable-builtin-mcps",
                "--disable-client-telemetry",
            ]);
        let opts = ClientOptions::new(ClientMode::CodingAgent)
            .with_runtime(RuntimeConnection::SpawnedStdio(process))
            .with_github(|g| g.with_token(token).with_use_logged_in_user(false))
            .with_observability(|o| o.with_disable_client_telemetry(true));
        let client = match Client::start(opts).await {
            Ok(client) => client,
            Err(e) => {
                let message = format!("Couldn't start the Copilot CLI: {e}");
                *self.start_error.lock() = Some(message.clone());
                let advice = format!(
                    "{message}. If it is an older or newer version than {}, download the one \
                     Humla supports in Settings.",
                    super::cli::VERSION
                );
                if is_version_mismatch(&e) {
                    return Err(WrongProtocol(advice).into());
                }
                bail!(advice);
            }
        };
        *self.start_error.lock() = None;
        *running = Some(Running { cli: cli.to_path_buf(), token: token.to_string(), client: client.clone() });
        Ok(client)
    }

    pub fn start_error(&self) -> Option<String> {
        self.start_error.lock().clone()
    }

    pub async fn stop(&self) {
        if let Some(r) = self.running.lock().await.take() {
            let _ = r.client.stop().await;
        }
    }
}

/// A chat-capable model the account may use, for the Settings picker.
#[derive(serde::Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub vendor: Option<String>,
    /// Premium requests per call, relative to the base rate; 0 is included.
    pub multiplier: Option<f64>,
}

pub async fn list_models(client: &Client) -> anyhow::Result<Vec<ModelInfo>> {
    let models = client.list_models().await.map_err(|e| anyhow!("{e}"))?;
    Ok(models
        .into_iter()
        .filter(|m| m.policy.as_ref().is_none_or(|p| p.state == ModelPolicyState::Enabled))
        .map(|m| ModelInfo {
            multiplier: m.billing.as_ref().and_then(|b| b.multiplier),
            id: m.id,
            name: m.name,
            vendor: m.vendor,
        })
        .collect())
}

/// The model that costs the least per call, for work the user didn't ask
/// for: the automatic title. `auto` is never it, since it may route anywhere.
pub fn cheapest(models: &[ModelInfo]) -> Option<&str> {
    models
        .iter()
        .filter(|m| m.id != "auto")
        .filter_map(|m| m.multiplier.map(|x| (x, m.id.as_str())))
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, id)| id)
}

/// The single prompt a step sends: the system turns become the system prompt,
/// and everything after them is written out as one transcript, since a
/// session carries no history Humla didn't put in this message.
pub fn render_prompt(turns: &[ChatTurn]) -> (String, String) {
    let system = turns
        .iter()
        .filter(|t| t.role == "system")
        .map(|t| t.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    let rest: Vec<&ChatTurn> = turns.iter().filter(|t| t.role != "system").collect();
    if let [only] = rest.as_slice() {
        if only.role == "user" {
            return (system, only.text.clone());
        }
    }
    let mut out = String::from(
        "The conversation so far, oldest first. Continue it: answer the last user \
         message, or call a tool.\n",
    );
    for turn in rest {
        match turn.role.as_str() {
            "tool" => out.push_str(&format!(
                "\n<tool_result call=\"{}\">\n{}\n</tool_result>\n",
                turn.tool_call_id.as_deref().unwrap_or_default(),
                turn.text
            )),
            role => {
                out.push_str(&format!("\n<{role}>\n"));
                if !turn.text.is_empty() {
                    out.push_str(&turn.text);
                    out.push('\n');
                }
                for call in &turn.tool_calls {
                    out.push_str(&format!(
                        "[called {} {} as {}]\n",
                        call.name, call.arguments, call.id
                    ));
                }
                out.push_str(&format!("</{role}>\n"));
            }
        }
    }
    (system, out)
}

/// The step an assistant message amounts to.
pub fn step_from_message(content: &str, tool_requests: &[AssistantMessageToolRequest]) -> ChatStep {
    ChatStep {
        text: content.to_string(),
        tool_calls: tool_requests
            .iter()
            .map(|r| ToolCall {
                id: r.tool_call_id.clone(),
                name: r.name.clone(),
                arguments: r
                    .arguments
                    .as_ref()
                    .map(|a| a.to_string())
                    .unwrap_or_else(|| "{}".into()),
            })
            .collect(),
    }
}

/// What one session streams, as it arrives.
enum Delta<'a> {
    Content(&'a str),
    Thinking(&'a str),
}

/// The tool results to answer inside a running turn's session, when `turns`
/// only adds the assistant's tool calls and their results to the `sent` turns
/// it already holds. `None` means the session can't continue this history —
/// the forced wrap-up adds a system turn — and a new one is needed.
pub fn continuation(turns: &[ChatTurn], sent: usize) -> Option<Vec<(String, String)>> {
    let added = turns.get(sent..)?;
    let (first, rest) = added.split_first()?;
    if first.role != "assistant" || first.tool_calls.is_empty() {
        return None;
    }
    let results: Option<Vec<_>> = rest
        .iter()
        .map(|t| if t.role == "tool" { Some((t.tool_call_id.clone()?, t.text.clone())) } else { None })
        .collect();
    let results = results?;
    let answered: Vec<&str> = results.iter().map(|(id, _)| id.as_str()).collect();
    let all_answered = first.tool_calls.iter().all(|c| answered.contains(&c.id.as_str()));
    (all_answered && !results.is_empty()).then_some(results)
}

/// A session and what has been read from it so far.
struct Live {
    client: Client,
    session: Session,
    events: EventSubscription,
    /// The tool calls the model asked for, by call id, with the request id the
    /// CLI awaits an answer under once it has announced one.
    pending: HashMap<String, Option<RequestId>>,
    /// How many of the turn's chat turns the session has seen.
    sent: usize,
}

impl Live {
    async fn open(
        client: &Client,
        model: &str,
        system: &str,
        tools: Vec<Tool>,
    ) -> anyhow::Result<Self> {
        // Custom tools ask permission before the CLI hands them back; no
        // built-in tool is available to ask.
        let names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
        let mut cfg = CreateSessionOptions::default()
            .with_model(model)
            .with_system_message(SystemMessageConfig::new().with_mode("replace").with_content(system))
            .with_permissions(|p| p.with_on_request(Arc::new(ApproveAllHandler)))
            .with_events(|e| e.with_streaming(true))
            .with_tools(|t| t.with_available_tools(names));
        if !tools.is_empty() {
            cfg = cfg.with_tools(|t| t.with_definitions(tools));
        }
        let session = client
            .create_session(cfg)
            .await
            .map_err(|e| anyhow!("Copilot couldn't start a session: {e}"))?;
        let events = session.subscribe();
        Ok(Live { client: client.clone(), session, events, pending: HashMap::new(), sent: 0 })
    }

    async fn send(&mut self, prompt: &str) -> anyhow::Result<()> {
        self.session.send(prompt).await.map_err(|e| anyhow!("Copilot refused the message: {e}"))?;
        Ok(())
    }

    async fn next_event(&mut self) -> anyhow::Result<SessionEvent> {
        self.events.recv().await.map_err(|e| anyhow!("Copilot stopped answering: {e:?}"))
    }

    /// Note a tool request's id if `ev` announces one.
    fn note_request(&mut self, ev: &SessionEvent) {
        if ev.event_type == "external_tool.requested" {
            let call = ev.data.get("toolCallId").and_then(|v| v.as_str());
            let request = serde_json::from_value(ev.data["requestId"].clone()).ok();
            if let (Some(call), Some(request)) = (call, request) {
                self.pending.insert(call.to_string(), Some(request));
            }
        }
    }

    /// Answer each tool call, waiting for the CLI to announce any it hasn't yet.
    async fn answer(&mut self, results: Vec<(String, String)>) -> anyhow::Result<()> {
        for (call_id, text) in results {
            let request_id = loop {
                match self.pending.get(&call_id) {
                    Some(Some(id)) => break id.clone(),
                    Some(None) => {
                        let ev = self.next_event().await?;
                        self.note_request(&ev);
                    }
                    None => bail!("Copilot never asked for tool call {call_id}"),
                }
            };
            self.pending.remove(&call_id);
            self.session
                .rpc()
                .tools()
                .handle_pending_tool_call(HandlePendingToolCallRequest {
                    request_id,
                    result: Some(serde_json::Value::String(text)),
                    error: None,
                })
                .await
                .map_err(|e| anyhow!("Copilot didn't take the tool result: {e}"))?;
        }
        Ok(())
    }

    /// Read to the next assistant message that requests tools, or to the end
    /// of the answer. `on_delta` returns `false` to stop early.
    async fn read_step(
        &mut self,
        on_delta: &mut (dyn FnMut(Delta<'_>) -> bool + Send),
    ) -> anyhow::Result<ChatStep> {
        let mut text = String::new();
        loop {
            let ev = self.next_event().await?;
            self.note_request(&ev);
            let data = &ev.data;
            match ev.event_type.as_str() {
                "assistant.message_delta" | "assistant.reasoning_delta" => {
                    let delta = data.get("deltaContent").and_then(|v| v.as_str()).unwrap_or_default();
                    let keep_going = if ev.event_type == "assistant.message_delta" {
                        text.push_str(delta);
                        on_delta(Delta::Content(delta))
                    } else {
                        on_delta(Delta::Thinking(delta))
                    };
                    if !keep_going {
                        return Ok(ChatStep { text, tool_calls: Vec::new() });
                    }
                }
                "assistant.message" => {
                    let msg: AssistantMessageData =
                        ev.typed_data().context("Copilot sent a message Humla can't read")?;
                    let requests = msg.tool_requests.unwrap_or_default();
                    if !requests.is_empty() {
                        for r in &requests {
                            self.pending.entry(r.tool_call_id.clone()).or_insert(None);
                        }
                        return Ok(step_from_message(&msg.content, &requests));
                    }
                    text = msg.content;
                }
                "session.error" => {
                    let message = data.get("message").and_then(|v| v.as_str()).unwrap_or("unknown error");
                    bail!("Copilot: {message}");
                }
                "session.idle" => return Ok(ChatStep { text, tool_calls: Vec::new() }),
                "assistant.usage" => eprintln!(
                    "[copilot] model={} initiator={} input={} output={} cost={}",
                    data["model"], data["initiator"], data["inputTokens"], data["outputTokens"], data["cost"]
                ),
                _ => {}
            }
        }
    }

    async fn close(self) {
        let id = self.session.id().clone();
        let _ = self.session.abort().await;
        let _ = self.session.disconnect().await;
        let _ = self.client.delete_session(&id).await;
    }
}

async fn within_timeout<T>(work: impl std::future::Future<Output = anyhow::Result<T>>) -> anyhow::Result<T> {
    tokio::time::timeout(CALL_TIMEOUT, work)
        .await
        .unwrap_or_else(|_| Err(anyhow!("Copilot took too long to answer.")))
}

/// A summary or a title.
pub async fn complete(
    client: &Client,
    model: &str,
    system: &str,
    user: &str,
    mut on_chunk: impl FnMut(StreamChunk) + Send,
) -> anyhow::Result<String> {
    let mut live = Live::open(client, model, system, Vec::new()).await?;
    let result = within_timeout(async {
        live.send(user).await?;
        live.read_step(&mut |d| {
            match d {
                Delta::Content(s) => on_chunk(StreamChunk::Content(s)),
                Delta::Thinking(s) => on_chunk(StreamChunk::Thinking(s)),
            }
            true
        })
        .await
    })
    .await;
    live.close().await;
    let text = result?.text;
    if text.trim().is_empty() {
        bail!("Copilot returned an empty answer.");
    }
    Ok(text)
}

/// One chat turn on Copilot: every step after the first answers the previous
/// step's tool calls inside the same session, so Copilot counts the follow-up
/// calls as the agent's rather than as new prompts from the user.
#[derive(Default)]
pub struct ChatTurnSession {
    live: tokio::sync::Mutex<Option<Live>>,
}

impl ChatTurnSession {
    pub async fn step(
        &self,
        client: &Client,
        model: &str,
        turns: &[ChatTurn],
        tools: &[ToolSpec],
        cancel: &CancelFlag,
        on_text: &mut (dyn FnMut(&str) + Send),
    ) -> anyhow::Result<ChatStep> {
        let mut slot = self.live.lock().await;
        let results = slot.as_ref().and_then(|l| continuation(turns, l.sent));
        let outcome = within_timeout(async {
            let mut live = match (slot.take(), results) {
                (Some(mut live), Some(results)) => {
                    live.answer(results).await?;
                    live
                }
                (old, _) => {
                    if let Some(old) = old {
                        old.close().await;
                    }
                    let (system, prompt) = render_prompt(turns);
                    let specs = tools
                        .iter()
                        .map(|t| Tool::new(t.name).with_description(t.description).with_parameters(t.parameters.clone()))
                        .collect();
                    let mut live = Live::open(client, model, &system, specs).await?;
                    live.send(&prompt).await?;
                    live
                }
            };
            live.sent = turns.len();
            let step = live
                .read_step(&mut |d| {
                    if let Delta::Content(s) = d {
                        on_text(s);
                    }
                    !cancel.is_cancelled()
                })
                .await;
            Ok((live, step))
        })
        .await;
        match outcome {
            Ok((live, Ok(step))) if !step.tool_calls.is_empty() && !cancel.is_cancelled() => {
                *slot = Some(live);
                Ok(step)
            }
            Ok((live, step)) => {
                live.close().await;
                step
            }
            Err(e) => Err(e),
        }
    }
}

impl Drop for ChatTurnSession {
    fn drop(&mut self) {
        if let Some(live) = self.live.get_mut().take() {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(live.close());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn model(id: &str, multiplier: Option<f64>) -> ModelInfo {
        ModelInfo { id: id.into(), name: id.into(), vendor: None, multiplier }
    }

    #[test]
    fn only_a_protocol_mismatch_counts_as_the_wrong_protocol() {
        use copilot_sdk::{Error, ErrorKind, ProtocolErrorKind};
        let mismatch = Error::from(ErrorKind::Protocol(ProtocolErrorKind::VersionMismatch { server: 3, min: 4, max: 4 }));
        let timeout = Error::from(ErrorKind::Protocol(ProtocolErrorKind::CliStartupTimeout));
        assert!(is_version_mismatch(&mismatch));
        assert!(!is_version_mismatch(&timeout));
    }

    #[test]
    fn the_cheapest_model_is_the_lowest_multiplier_and_never_auto() {
        let models = [
            model("auto", Some(0.0)),
            model("claude-opus-5.5", Some(10.0)),
            model("gpt-5-mini", Some(0.0)),
            model("claude-haiku-5.5", Some(0.33)),
            model("grok-4.7", None),
        ];
        assert_eq!(cheapest(&models), Some("gpt-5-mini"));
        assert_eq!(cheapest(&[model("auto", Some(0.0)), model("x", None)]), None);
    }

    fn asked(ids: &[&str]) -> ChatTurn {
        let calls = ids
            .iter()
            .map(|id| ToolCall { id: id.to_string(), name: "search_notes".into(), arguments: "{}".into() })
            .collect();
        ChatTurn::assistant_tool_calls("", calls)
    }

    fn opening() -> Vec<ChatTurn> {
        vec![ChatTurn::new("system", "S"), ChatTurn::new("user", "What did Acme decide?")]
    }

    #[test]
    fn answered_tool_calls_continue_the_session() {
        let mut turns = opening();
        turns.push(asked(&["c1", "c2"]));
        turns.push(ChatTurn::tool_result("c1", "kept the budget"));
        turns.push(ChatTurn::tool_result("c2", "moved Q1 review"));
        assert_eq!(
            continuation(&turns, 2),
            Some(vec![("c1".into(), "kept the budget".into()), ("c2".into(), "moved Q1 review".into())])
        );
    }

    #[test]
    fn the_forced_wrap_up_starts_a_new_session() {
        let mut turns = opening();
        turns.push(asked(&["c1"]));
        turns.push(ChatTurn::tool_result("c1", "kept the budget"));
        turns.push(ChatTurn::new("system", "Answer now."));
        assert_eq!(continuation(&turns, 2), None);
    }

    #[test]
    fn a_call_left_unanswered_or_a_history_it_never_saw_starts_over() {
        let mut turns = opening();
        turns.push(asked(&["c1", "c2"]));
        turns.push(ChatTurn::tool_result("c1", "kept the budget"));
        assert_eq!(continuation(&turns, 2), None, "c2 has no result");
        assert_eq!(continuation(&opening(), 2), None, "nothing new");
        assert_eq!(continuation(&opening(), 5), None, "fewer turns than were sent");
    }

    #[test]
    fn a_first_turn_goes_as_it_is() {
        let (system, prompt) = render_prompt(&[
            ChatTurn::new("system", "You answer from notes."),
            ChatTurn::new("system", "Today is 27 Oct."),
            ChatTurn::new("user", "What did Acme decide?"),
        ]);
        assert_eq!(system, "You answer from notes.\n\nToday is 27 Oct.");
        assert_eq!(prompt, "What did Acme decide?");
    }

    #[test]
    fn a_tool_trail_is_written_out_in_order() {
        let call = ToolCall { id: "c1".into(), name: "search_notes".into(), arguments: r#"{"query":"Acme"}"#.into() };
        let (system, prompt) = render_prompt(&[
            ChatTurn::new("system", "S"),
            ChatTurn::new("user", "What did Acme decide?"),
            ChatTurn::assistant_tool_calls("", vec![call]),
            ChatTurn::tool_result("c1", "Acme kept the budget."),
        ]);
        assert_eq!(system, "S");
        assert_eq!(
            prompt,
            "The conversation so far, oldest first. Continue it: answer the last user message, \
             or call a tool.\n\
             \n<user>\nWhat did Acme decide?\n</user>\n\
             \n<assistant>\n[called search_notes {\"query\":\"Acme\"} as c1]\n</assistant>\n\
             \n<tool_result call=\"c1\">\nAcme kept the budget.\n</tool_result>\n"
        );
    }

    #[test]
    fn a_grounded_follow_up_keeps_every_turn() {
        let (_, prompt) = render_prompt(&[
            ChatTurn::new("system", "S"),
            ChatTurn::new("user", "Note: the Acme meeting."),
            ChatTurn::new("user", "Summarise it"),
            ChatTurn::new("assistant", "They kept the budget."),
            ChatTurn::new("user", "And the date?"),
        ]);
        assert!(prompt.contains("<user>\nNote: the Acme meeting.\n</user>"));
        assert!(prompt.contains("<assistant>\nThey kept the budget.\n</assistant>"));
        assert!(prompt.trim_end().ends_with("<user>\nAnd the date?\n</user>"));
    }

    /// Against the real CLI and account; spends Copilot requests. Run with
    /// `HUMLA_COPILOT_TOKEN=$(gh auth token) cargo test --lib copilot_live -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn copilot_live() {
        let token = std::env::var("HUMLA_COPILOT_TOKEN").expect("HUMLA_COPILOT_TOKEN");
        let cli = super::super::cli::locate(&std::env::temp_dir()).await.expect("a Copilot CLI");
        let rt = Runtime::default();
        let client = rt.client(&cli, &token).await.unwrap();

        let models = list_models(&client).await.unwrap();
        assert!(models.iter().any(|m| m.id == "auto"), "{models:?}");

        let mut streamed = String::new();
        let text = complete(&client, "auto", "Answer with one word.", "Say hello in Norwegian.", |c| {
            if let StreamChunk::Content(s) = c {
                streamed.push_str(s);
            }
        })
        .await
        .unwrap();
        assert!(!text.trim().is_empty());
        assert_eq!(streamed, text);

        let tool = ToolSpec {
            name: "search_notes",
            description: "Search the user's meeting notes by keyword.",
            parameters: json!({ "type": "object", "properties": { "query": { "type": "string" } }, "required": ["query"] }),
        };
        let turns = [
            ChatTurn::new("system", "You answer only from the user's notes. Always search first."),
            ChatTurn::new("user", "What did Acme decide about the budget?"),
        ];
        let turn = ChatTurnSession::default();
        let cancel = CancelFlag::new();
        let first = turn.step(&client, "auto", &turns, std::slice::from_ref(&tool), &cancel, &mut |_| {}).await.unwrap();
        assert!(!first.tool_calls.is_empty(), "{first:?}");
        assert_eq!(first.tool_calls[0].name, "search_notes");
        assert!(turn.live.lock().await.is_some(), "the session waits for the results");

        let mut followed = turns.to_vec();
        followed.push(ChatTurn::assistant_tool_calls("", first.tool_calls.clone()));
        for call in &first.tool_calls {
            followed.push(ChatTurn::tool_result(&call.id, "Acme kept the budget at 2 MNOK (meeting 3 Oct)."));
        }
        let mut answer = turn.step(&client, "auto", &followed, std::slice::from_ref(&tool), &cancel, &mut |_| {}).await.unwrap();
        // The model may search once more; answer it the same way.
        while !answer.tool_calls.is_empty() {
            followed.push(ChatTurn::assistant_tool_calls("", answer.tool_calls.clone()));
            for call in &answer.tool_calls {
                followed.push(ChatTurn::tool_result(&call.id, "Nothing more about Acme."));
            }
            answer = turn.step(&client, "auto", &followed, std::slice::from_ref(&tool), &cancel, &mut |_| {}).await.unwrap();
        }
        assert!(answer.text.contains('2'), "{}", answer.text);
        assert!(turn.live.lock().await.is_none(), "an answered turn closes its session");
        rt.stop().await;
    }

    #[test]
    fn tool_requests_become_tool_calls_with_their_arguments_as_json() {
        let requests: Vec<AssistantMessageToolRequest> = serde_json::from_value(json!([
            { "toolCallId": "toolu_1", "name": "search_notes", "arguments": { "query": "Acme budsjett" }, "type": "function" },
            { "toolCallId": "toolu_2", "name": "list_notes", "type": "function" }
        ]))
        .unwrap();
        assert_eq!(
            step_from_message("", &requests),
            ChatStep {
                text: String::new(),
                tool_calls: vec![
                    ToolCall { id: "toolu_1".into(), name: "search_notes".into(), arguments: r#"{"query":"Acme budsjett"}"#.into() },
                    ToolCall { id: "toolu_2".into(), name: "list_notes".into(), arguments: "{}".into() },
                ],
            }
        );
    }
}
