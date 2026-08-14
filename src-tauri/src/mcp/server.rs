//! The transport adapter: `rmcp`'s `ServerHandler` on top of [`super::tools`].
//!
//! Deliberately thin. Everything that decides an answer — SQL, filters,
//! formatting, truncation — lives below `tools::execute`; this file maps the
//! protocol's shapes onto that one call and does nothing else. What correctness it
//! has shows up as a connection that works, which is why it is verified by hand
//! against both clients rather than by unit tests of `rmcp`'s framing.
//!
//! Two things here are not merely plumbing:
//!
//! - **The enable gate is checked per call**, against the live database, so turning
//!   the integration off in Settings takes effect on the next tool call rather than
//!   at the client's next restart.
//! - **Resources are implemented and nothing depends on them.** Claude Code turns
//!   them into `@` mentions; Codex documents tools and server instructions only, so
//!   every capability stays reachable through tools alone. Listing them also dodges
//!   a Codex behaviour where a tools-only server is reported as unavailable because
//!   its resource list is empty — which is why the DISABLED state lists a
//!   placeholder rather than nothing, the default state being the one that would
//!   otherwise trip it.

use std::sync::Arc;

use parking_lot::Mutex;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData,
    Implementation, ListResourcesResult, ListToolsResult, PaginatedRequestParams, ProtocolVersion,
    ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Resource,
    ResourceContents, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::RequestContext;
use rmcp::RoleServer;
use rusqlite::Connection;
use serde_json::{Map, Value};

use super::tools;

/// The `humla://note/<id>` scheme notes are exposed under.
const RESOURCE_PREFIX: &str = "humla://note/";
/// The one resource offered while the integration is switched off. Deliberately NOT
/// under [`RESOURCE_PREFIX`]: it is not a note, it names none, and reading it must
/// not go anywhere near the library.
const RESOURCE_DISABLED: &str = "humla://disabled";
/// How many notes to advertise as resources. An `@` menu is a picker, not an
/// archive — the tools are the way to reach anything older, and a list of every
/// note in a years-old library would cost the client more than it is worth.
const RESOURCE_LIMIT: usize = 200;

#[derive(Clone)]
pub struct HumlaMcp {
    db: Arc<Mutex<Connection>>,
}

impl HumlaMcp {
    pub fn new(conn: Connection) -> Self {
        Self { db: Arc::new(Mutex::new(conn)) }
    }

    /// Run one tool against the live database, or say the integration is off.
    /// Returns the model-facing text and whether it is an error, which is all the
    /// protocol layer above needs to know.
    fn dispatch(&self, name: &str, args: Value) -> (String, bool) {
        let conn = self.db.lock();
        if !super::is_enabled(&conn) {
            return (super::disabled_message(), true);
        }
        let workspace = super::active_workspace(&conn);
        let out = tools::execute(&conn, &workspace, name, &args, crate::db::now_ms());
        (out.model_text, out.is_error)
    }

    /// The resources to advertise. Split out of the handler for the same reason
    /// [`Self::dispatch`] is: `rmcp`'s `RequestContext` cannot be built outside that
    /// crate, so anything left inside a handler signature is unreachable from a test
    /// — and what this decides (whether a disabled server answers at all, and whether
    /// its answer names a note) is exactly what needs pinning.
    fn resources(&self) -> Vec<Resource> {
        let conn = self.db.lock();
        if !super::is_enabled(&conn) {
            // A PLACEHOLDER, not an empty list — for exactly the reason resources are
            // implemented at all. Codex has reported a server whose resource list is
            // empty as unavailable, and "off" is the DEFAULT state, so answering an
            // honest `[]` here would make the feature's own shipping configuration
            // reproduce the failure it exists to avoid: the user turns the switch on
            // and finds a server the client already wrote off. `list_tools` answers
            // while disabled for the same reason.
            //
            // It carries no note title and no note id: the whole promise of the gate
            // is that nothing from the library is readable until the user opts in,
            // and a resource list is readable without any tool being called.
            return vec![Resource::new(RESOURCE_DISABLED, "Humla — integration turned off")
                .with_description(super::disabled_message())
                .with_mime_type("text/plain")];
        }
        let workspace = super::active_workspace(&conn);
        let notes =
            crate::db::list_notes_filtered(&conn, Default::default(), &workspace, RESOURCE_LIMIT)
                .unwrap_or_default();
        notes
            .into_iter()
            .map(|n| {
                let title = if n.title.trim().is_empty() { "(untitled)" } else { n.title.trim() };
                Resource::new(format!("{RESOURCE_PREFIX}{}", n.id), title.to_string())
                    .with_mime_type("text/plain")
            })
            .collect()
    }

    /// What one resource URI reads as, or the error the client should see.
    fn read_uri(&self, uri: &str) -> Result<String, ErrorData> {
        // The placeholder the disabled listing advertises. Answered rather than
        // 404'd: a client that lists a resource and cannot read it concludes the
        // server is broken, which is the opposite of the message — and the message is
        // the entire point of offering it.
        if uri == RESOURCE_DISABLED {
            return Ok(super::disabled_message());
        }
        let Some(note_id) = uri.strip_prefix(RESOURCE_PREFIX) else {
            return Err(ErrorData::resource_not_found(format!("Not a Humla note URI: {uri}"), None));
        };
        // Straight through the same seam the tool takes, so a resource read and a
        // `get_note` call can never come to mean different things — including the
        // workspace check and the reference-material framing.
        let (text, is_error) = self.dispatch(tools::TOOL_GET, serde_json::json!({ "note_id": note_id }));
        if is_error {
            return Err(ErrorData::resource_not_found(text, None));
        }
        Ok(text)
    }
}

/// A JSON-Schema object as `rmcp` wants it. Our specs are always objects, but a
/// hand-written schema is data — if one is ever malformed, advertise an empty
/// parameter set rather than panic the server at `tools/list`.
fn schema_object(v: &Value) -> Arc<Map<String, Value>> {
    Arc::new(v.as_object().cloned().unwrap_or_default())
}

impl ServerHandler for HumlaMcp {
    fn get_info(&self) -> ServerInfo {
        // Resources are declared as well as tools: Claude Code turns them into `@`
        // mentions, and a server that declares none has been reported by Codex as
        // unavailable.
        ServerInfo::new(
            ServerCapabilities::builder().enable_tools().enable_resources().build(),
        )
        .with_protocol_version(ProtocolVersion::LATEST)
        .with_server_info(Implementation::new("humla", env!("CARGO_PKG_VERSION")))
        .with_instructions(super::INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        // Advertised whether or not the integration is enabled. A client that sees
        // no tools concludes the server is broken and stops asking; one that calls a
        // tool gets told, in words the user can act on, that it is switched off.
        let tools = tools::specs()
            .into_iter()
            .map(|s| Tool::new(s.name, s.description, schema_object(&s.parameters)))
            .collect();
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let args = request.arguments.map(Value::Object).unwrap_or(Value::Null);
        let (text, is_error) = self.dispatch(&request.name, args);
        let content = vec![ContentBlock::text(text)];
        // A tool failure is CONTENT, never a protocol error: a protocol error is
        // rendered opaquely by clients, so the model never reads the message that
        // would let it correct itself — and nothing here can make the server unable
        // to serve the next request.
        let result = if is_error {
            CallToolResult::error(content)
        } else {
            CallToolResult::success(content)
        };
        Ok(result.into())
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult::with_all_items(self.resources()))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let text = self.read_uri(&request.uri)?;
        Ok(ReadResourceResult::new(vec![ResourceContents::text(text, request.uri)]).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> ServerInfo {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open(&dir.path().join("t.sqlite")).unwrap();
        HumlaMcp::new(conn).get_info()
    }

    /// Resources are a Claude Code bonus, but declaring the capability is not
    /// optional: Codex has reported a healthy tools-only server as unavailable
    /// because its resource list was empty.
    #[test]
    fn the_server_advertises_both_tools_and_resources() {
        let caps = info().capabilities;
        assert!(caps.tools.is_some(), "tools capability");
        assert!(caps.resources.is_some(), "resources capability");
    }

    /// The instructions string is what decides whether a client goes looking for
    /// the tools at all, so an empty or absent one costs the whole integration.
    #[test]
    fn the_server_hands_the_client_its_instructions() {
        let instructions = info().instructions.unwrap_or_default();
        assert_eq!(instructions, super::super::INSTRUCTIONS);
        assert!(instructions.contains("meeting"));
    }

    /// Every tool advertised must be one the seam below can actually run — a name
    /// that lists but does not dispatch is a tool that exists only to fail.
    #[test]
    fn every_advertised_tool_dispatches() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open(&dir.path().join("t.sqlite")).unwrap();
        crate::db::set_setting(&conn, super::super::SETTING_ENABLED, "true").unwrap();
        let server = HumlaMcp::new(conn);
        for spec in tools::specs() {
            let (text, _) = server.dispatch(spec.name, serde_json::json!({ "query": "x", "note_id": "x" }));
            assert!(!text.contains("Unknown tool"), "{} does not dispatch", spec.name);
        }
    }

    /// The disabled state is the DEFAULT state, so it is the one that decides whether
    /// a client ever comes back. An empty resource list is what Codex reads as "this
    /// server is unavailable" — the exact failure resources are implemented to avoid —
    /// so the off switch must still answer with something, and that something must
    /// carry nothing from the library: a resource list is readable without any tool
    /// being called, so a note title here would walk straight past the gate.
    #[test]
    fn a_disabled_server_still_lists_a_resource_and_it_names_no_note() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open(&dir.path().join("t.sqlite")).unwrap();
        let note = crate::db::create_note(&conn, "en", "meeting", "").unwrap();
        crate::db::update_note(
            &conn,
            &note.id,
            &crate::db::NotePatch { title: Some("Secret meeting".into()), ..Default::default() },
        )
        .unwrap();
        let server = HumlaMcp::new(conn);

        let listed = server.resources();
        assert!(!listed.is_empty(), "an empty list reads as a broken server");
        let rendered = serde_json::to_string(&listed).unwrap();
        assert!(!rendered.contains("Secret meeting"), "leaked a title while disabled: {rendered}");
        assert!(!rendered.contains(&note.id), "leaked an id while disabled: {rendered}");
        assert!(rendered.contains("Settings"), "says how to turn it on: {rendered}");

        // …and the placeholder must READ, not 404: a listed resource that cannot be
        // opened is the same "broken server" conclusion by another route.
        let text = server.read_uri(RESOURCE_DISABLED).expect("the placeholder reads");
        assert!(text.contains("Settings"), "the read explains the switch: {text}");
        assert!(!text.contains("Secret meeting"), "{text}");
    }

    /// Once enabled, the listing is the `@` menu: real notes, each reachable by the
    /// URI it is listed under. The placeholder must be gone — leaving it in an
    /// enabled list would advertise "turned off" beside notes that plainly are not.
    #[test]
    fn an_enabled_server_lists_the_notes_themselves_and_they_read() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open(&dir.path().join("t.sqlite")).unwrap();
        let note = crate::db::create_note(&conn, "en", "meeting", "").unwrap();
        crate::db::update_note(
            &conn,
            &note.id,
            &crate::db::NotePatch { title: Some("Kickoff".into()), ..Default::default() },
        )
        .unwrap();
        crate::db::set_setting(&conn, super::super::SETTING_ENABLED, "true").unwrap();
        let server = HumlaMcp::new(conn);

        let listed = server.resources();
        let rendered = serde_json::to_string(&listed).unwrap();
        assert!(rendered.contains("Kickoff"), "{rendered}");
        assert!(!rendered.contains(RESOURCE_DISABLED), "the placeholder outstayed its state");

        let uri = format!("{RESOURCE_PREFIX}{}", note.id);
        assert!(server.read_uri(&uri).unwrap().contains("Kickoff"));
        // A URI in no scheme this server owns is a genuine not-found.
        assert!(server.read_uri("file:///etc/passwd").is_err());
    }

    /// The gate is the whole privacy promise: until the user turns it on, no tool
    /// may answer with anything from the library, and the message has to tell them
    /// what to do rather than read as a malfunction.
    #[test]
    fn nothing_is_readable_while_the_integration_is_switched_off() {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open(&dir.path().join("t.sqlite")).unwrap();
        let note = crate::db::create_note(&conn, "en", "meeting", "").unwrap();
        crate::db::update_note(
            &conn,
            &note.id,
            &crate::db::NotePatch {
                title: Some("Secret meeting".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let server = HumlaMcp::new(conn);

        for spec in tools::specs() {
            let (text, is_error) =
                server.dispatch(spec.name, serde_json::json!({ "query": "secret", "note_id": note.id }));
            assert!(is_error, "{}", spec.name);
            assert!(!text.contains("Secret meeting"), "{} leaked while disabled", spec.name);
            assert!(text.contains("Settings"), "{} says how to turn it on", spec.name);
        }
        // …and enabling it makes the same call answer.
        {
            let conn = server.db.lock();
            crate::db::set_setting(&conn, super::super::SETTING_ENABLED, "true").unwrap();
        }
        let (text, is_error) = server.dispatch(tools::TOOL_LIST, serde_json::json!({}));
        assert!(!is_error);
        assert!(text.contains("Secret meeting"));
    }
}
