//! Send a note to the user's own webhook (#213), so a knowledge base, an agent
//! or an automation can pick it up. One explicit POST per press of ⋯ → Send to
//! webhook, after a confirmation that names where it is going. Never automatic:
//! a note leaves only when the user sends that note.
//!
//! Where it goes is a secret: a URL often carries a token, and its headers
//! usually do. Both live in the Keychain under [`ACCOUNT`]. Only whether the
//! feature is on lives in SQLite (`webhook_enabled`), so the note menu can ask
//! without touching the Keychain. Header values are write-only from the
//! webview, like API keys: Settings learns each header's name, never its value.
//!
//! The payload is a contract the user's receiver is written against
//! (`schema_version`), so it is built from an explicit list of fields, kept as
//! small as is useful, and never serializes a whole `Note`. Fields may be
//! added; existing ones keep their meaning. It carries no audio and no path to
//! any: `keep_audio` stays the only gate on audio (#24). humla.team/docs
//! (`integrations/webhooks`) documents it for receivers.
use super::{err, summary::note_language};
use crate::{db, html_text::html_to_text, keychain, sessions, AppState};
use chrono::{DateTime, SecondsFormat, Utc};
use reqwest::{
    header::{HeaderMap, HeaderName, HeaderValue},
    Url,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;
use tauri::{AppHandle, Manager, State};

/// Keychain account for the endpoint. The name predates the split of the
/// on/off switch out into SQLite, and stays so a saved endpoint still reads.
const ACCOUNT: &str = "webhook_config";
const ENABLED: &str = "webhook_enabled";
const SCHEMA_VERSION: u32 = 1;
const SETTINGS: &str = "Settings → General → Integrations";

/// Where a note is sent: what the Keychain holds.
#[derive(Default, Serialize, Deserialize)]
struct Endpoint {
    #[serde(default)]
    url: String,
    #[serde(default)]
    headers: Vec<Header>,
}

#[derive(Serialize, Deserialize)]
struct Header {
    name: String,
    value: String,
}

/// What Settings shows of the endpoint: the URL and each header's name.
#[derive(Serialize)]
pub struct EndpointView {
    url: String,
    headers: Vec<String>,
}

/// What Settings saves. A header with no `value` keeps the value already
/// stored under that name, so a form that never received a value can still
/// save the rest.
#[derive(Deserialize)]
pub struct EndpointInput {
    url: String,
    #[serde(default)]
    headers: Vec<HeaderInput>,
}

#[derive(Deserialize)]
pub struct HeaderInput {
    name: String,
    value: Option<String>,
}

fn read_endpoint(state: &AppState) -> Result<Option<Endpoint>, String> {
    let Some(json) = keychain::read(&state.keychain_cache, ACCOUNT)? else {
        return Ok(None);
    };
    serde_json::from_str(&json)
        .map(Some)
        .map_err(|_| format!("The saved webhook couldn't be read. Save it again in {SETTINGS}."))
}

/// The endpoint to send to, or what the user has to do before there is one.
fn saved_endpoint(state: &AppState) -> Result<Endpoint, String> {
    read_endpoint(state)?
        .filter(|endpoint| !endpoint.url.is_empty())
        .ok_or_else(|| format!("Add a webhook URL in {SETTINGS} first."))
}

/// The endpoint `input` describes, with each header that came without a value
/// keeping the one `stored` has under that name. Names match ignoring case,
/// as they do in HTTP.
fn merge(input: EndpointInput, stored: Option<&Endpoint>) -> Result<Endpoint, String> {
    let headers = input
        .headers
        .into_iter()
        .map(|header| {
            let name = header.name.trim().to_string();
            let value = match header.value {
                Some(value) => value,
                None => stored
                    .into_iter()
                    .flat_map(|endpoint| &endpoint.headers)
                    .find(|old| old.name.eq_ignore_ascii_case(&name))
                    .map(|old| old.value.clone())
                    .ok_or_else(|| format!("Enter a value for the {name} header."))?,
            };
            Ok(Header { name, value })
        })
        .collect::<Result<_, String>>()?;
    Ok(Endpoint {
        url: input.url.trim().to_string(),
        headers,
    })
}

fn validate(endpoint: &Endpoint) -> Result<(Url, HeaderMap), String> {
    let url =
        Url::parse(endpoint.url.trim()).map_err(|_| "Enter a valid webhook URL.".to_string())?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err("Webhook URL must start with https:// or http://.".into());
    }
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err("Use headers for authentication and remove any URL fragment.".into());
    }
    let mut headers = HeaderMap::new();
    for header in &endpoint.headers {
        let name = HeaderName::from_bytes(header.name.trim().as_bytes())
            .map_err(|_| "Each header needs a valid name.".to_string())?;
        if matches!(
            name.as_str(),
            "host"
                | "content-length"
                | "content-type"
                | "connection"
                | "transfer-encoding"
                | "trailer"
                | "upgrade"
                | "expect"
        ) {
            return Err("Host, content type and transport headers are managed by Humla.".into());
        }
        let mut value = HeaderValue::from_str(&header.value).map_err(|_| {
            "Header values cannot contain line breaks or invalid characters.".to_string()
        })?;
        value.set_sensitive(true);
        if headers.insert(name, value).is_some() {
            return Err("Header names must be unique (ignoring capitalization).".into());
        }
    }
    Ok((url, headers))
}

/// Under `local_only` for notes in `workspace_id` a note may go only to a
/// receiver on this Mac. Saving the receiver is checked against Personal: the
/// managed profile alone.
fn check_policy(conn: &rusqlite::Connection, workspace_id: Option<&str>, url: &Url) -> Result<(), String> {
    let policy = crate::org_policy::effective_for_or_strictest(conn, workspace_id);
    if policy.local_only && !crate::org_policy::is_loopback_url(url.as_str()) {
        return Err(policy.blocked().to_string());
    }
    Ok(())
}

/// The receiver as the confirmation names it: its host, and its port when that
/// isn't the scheme's default. Never the path or query, which often carry a
/// token.
fn destination(url: &Url) -> String {
    let host = url.host_str().unwrap_or_default();
    match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    }
}

#[tauri::command]
pub fn webhook_get_endpoint(state: State<AppState>) -> Result<EndpointView, String> {
    let endpoint = read_endpoint(&state)?.unwrap_or_default();
    Ok(EndpointView {
        url: endpoint.url,
        headers: endpoint
            .headers
            .into_iter()
            .map(|header| header.name)
            .collect(),
    })
}

/// Save the endpoint. An empty URL with no headers removes it.
#[tauri::command]
pub fn webhook_set_endpoint(state: State<AppState>, endpoint: EndpointInput) -> Result<(), String> {
    // A stored endpoint that can't be read is no reason to refuse a new one;
    // any header that needed its value then asks for it again.
    let stored = read_endpoint(&state).ok().flatten();
    let endpoint = merge(endpoint, stored.as_ref())?;
    if endpoint.url.is_empty() && endpoint.headers.is_empty() {
        return keychain::write(&state.keychain_cache, ACCOUNT, None);
    }
    let (url, _) = validate(&endpoint)?;
    check_policy(&state.db.lock(), None, &url)?;
    let json = serde_json::to_string(&endpoint).map_err(err)?;
    keychain::write(&state.keychain_cache, ACCOUNT, Some(&json))
}

/// Where a send would go, for the confirmation to name.
#[tauri::command]
pub fn webhook_destination(state: State<AppState>) -> Result<String, String> {
    let (url, _) = validate(&saved_endpoint(&state)?)?;
    Ok(destination(&url))
}

#[tauri::command]
pub async fn webhook_send_note(
    app: AppHandle,
    state: State<'_, AppState>,
    note_id: String,
) -> Result<(), String> {
    let (note, facts) = {
        let conn = state.db.lock();
        if db::get_setting(&conn, ENABLED).map_err(err)?.as_deref() != Some("true") {
            return Err(format!("Turn on the webhook in {SETTINGS} first."));
        }
        let note = db::get_note(&conn, &note_id).map_err(err)?;
        if note.deleted_at.is_some() {
            return Err("Restore this note before sending it.".into());
        }
        let facts = NoteFacts {
            language: note_language(&conn, &note).map_err(err)?,
            folder: named(note.folder_id.as_deref(), |id| db::folder_name(&conn, id))
                .map_err(err)?,
            client: named(note.client_id.as_deref(), |id| db::client_name(&conn, id))
                .map_err(err)?,
        };
        (note, facts)
    };
    let endpoint = saved_endpoint(&state)?;
    let (url, _) = validate(&endpoint)?;
    check_policy(&state.db.lock(), Some(&note.workspace_id), &url)?;
    let app_dir = app.path().app_data_dir().map_err(err)?;
    let takes: Vec<_> = sessions::resolve_sessions(&sessions::recordings_dir(&app_dir, &note_id))
        .into_iter()
        .map(|(entry, _)| entry)
        .collect();
    let in_progress = state.recording.lock().note_id.as_deref() == Some(note_id.as_str());
    let payload = build_payload(&note, &facts, recording(&takes, in_progress), Utc::now());
    deliver(&endpoint, &payload).await?;
    let format = crate::org_audit::Action::Export(crate::org_audit::Format::Webhook);
    crate::org_audit::record(&state.db.lock(), format, &note.workspace_id, &note.id);
    Ok(())
}

/// What the payload says about a note that the note row alone can't.
struct NoteFacts {
    /// [`note_language`]: `auto` when nothing was detected.
    language: String,
    folder: Value,
    client: Value,
}

/// `{ id, name }` for a folder or client the note is filed under, or `null`.
/// A receiver can't resolve an id on its own, and a name alone can't follow a
/// rename. An id whose row is gone reads as `null`, like no reference at all.
fn named(
    id: Option<&str>,
    name_of: impl FnOnce(&str) -> anyhow::Result<Option<String>>,
) -> anyhow::Result<Value> {
    let Some(id) = id.filter(|id| !id.is_empty()) else {
        return Ok(Value::Null);
    };
    Ok(name_of(id)?.map_or(Value::Null, |name| json!({ "id": id, "name": name })))
}

fn timestamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn timestamp_ms(ms: i64) -> Value {
    DateTime::from_timestamp_millis(ms).map_or(Value::Null, |at| timestamp(at).into())
}

fn build_payload(
    note: &db::Note,
    facts: &NoteFacts,
    recording: Value,
    sent_at: DateTime<Utc>,
) -> Value {
    let language = Some(facts.language.trim()).filter(|l| !l.is_empty() && *l != "auto");
    json!({
        "schema_version": SCHEMA_VERSION,
        "event": "note.sent",
        "sent_at": timestamp(sent_at),
        "note": {
            "id": note.id,
            "title": note.title,
            "created_at": timestamp_ms(note.created_at),
            "updated_at": timestamp_ms(note.updated_at),
            "language": language,
            "folder": facts.folder,
            "client": facts.client,
            "notes_html": note.body,
            "notes_text": html_to_text(&note.body),
            "summary": note.summary,
            "transcript": note.transcript,
            "speakers": db::transcript_speakers(&note.transcript),
            "recording": recording,
        }
    })
}

/// When the note was recorded, or `null` for a note that never was.
///
/// The "Earlier transcript" session at index 0 is text a repair gave a
/// timeline to (#169), not a take, so it doesn't count. A total covers every
/// take or is `null`: a sum or an earliest start that silently skips a take
/// whose time isn't known (a take from before sessions kept one) would be
/// wrong without saying so. While the note is recording (or importing), the
/// take in progress isn't in the manifest yet, so there is no duration to give.
fn recording(entries: &[sessions::SessionEntry], in_progress: bool) -> Value {
    let takes: Vec<(u32, Option<DateTime<Utc>>, Option<u64>)> = entries
        .iter()
        .filter(|entry| entry.index > 0)
        .map(|entry| {
            let started_at = DateTime::parse_from_rfc3339(&entry.started_at)
                .ok()
                .map(|at| at.with_timezone(&Utc));
            (
                entry.index,
                started_at,
                (entry.duration_ms > 0).then_some(entry.duration_ms),
            )
        })
        .collect();
    if takes.is_empty() && !in_progress {
        return Value::Null;
    }
    let started_at = takes
        .iter()
        .map(|(_, at, _)| *at)
        .collect::<Option<Vec<_>>>()
        .and_then(|starts| starts.into_iter().min());
    let duration_ms = if in_progress {
        None
    } else {
        takes
            .iter()
            .try_fold(0u64, |total, (_, _, ms)| total.checked_add((*ms)?))
    };
    json!({
        "started_at": started_at.map(timestamp),
        "duration_ms": duration_ms,
        "in_progress": in_progress,
        "takes": takes.iter().map(|(index, at, ms)| json!({
            "index": index,
            "started_at": at.map(timestamp),
            "duration_ms": ms,
        })).collect::<Vec<_>>(),
    })
}

/// POST `payload` once: no retries, no redirects, bounded in time. No error
/// carries the URL (reqwest's would, token and all) or anything the receiver
/// answered, which could echo credentials or the meeting back.
async fn deliver(endpoint: &Endpoint, payload: &Value) -> Result<(), String> {
    let (url, headers) = validate(endpoint)?;
    let body = serde_json::to_vec(payload)
        .map_err(|_| "Could not encode the webhook JSON.".to_string())?;
    let payload_bytes = body.len();
    let client = crate::http::client_builder()
        .user_agent(concat!("Humla/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| "Could not create the webhook connection.".to_string())?;
    let response = client
        .post(url)
        .headers(headers)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                "The webhook timed out. It may still have received the note, so check the receiver before sending again.".to_string()
            } else {
                "Could not reach the webhook. Check its URL and your connection. It may still have received the note.".to_string()
            }
        })?;
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    if status == reqwest::StatusCode::PAYLOAD_TOO_LARGE {
        return Err(format!(
            "The webhook returned HTTP 413: the note ({payload_bytes} bytes of JSON) is larger than the receiver accepts. Raise its request body limit."
        ));
    }
    Err(format!(
        "The webhook returned HTTP {}.{}",
        status.as_u16(),
        if status.is_redirection() {
            " Redirects aren't followed, so use the final URL in Settings."
        } else if matches!(status.as_u16(), 401 | 403) {
            " Check its authentication headers in Settings."
        } else {
            " Check the receiver's logs."
        }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn endpoint(url: &str) -> Endpoint {
        Endpoint {
            url: url.into(),
            headers: vec![Header {
                name: "Authorization".into(),
                value: "Bearer test-token".into(),
            }],
        }
    }

    fn input(url: &str, headers: &[(&str, Option<&str>)]) -> EndpointInput {
        EndpointInput {
            url: url.into(),
            headers: headers
                .iter()
                .map(|(name, value)| HeaderInput {
                    name: (*name).into(),
                    value: value.map(Into::into),
                })
                .collect(),
        }
    }

    #[test]
    fn validates_urls_and_headers_without_exposing_values() {
        for url in [
            "file:///tmp/secret",
            "not-a-url",
            "https://user:secret@example.com",
            "https://example.com/#secret",
        ] {
            assert!(validate(&endpoint(url)).is_err());
        }
        assert!(validate(&endpoint("https://example.com/hook?token=secret")).is_ok());
        let mut cfg = endpoint("http://127.0.0.1/hook");
        cfg.headers[0].value = "secret\r\nInjected: true".into();
        let error = validate(&cfg).unwrap_err();
        assert!(!error.contains("secret"));
        cfg.headers[0].value = "secret".into();
        cfg.headers.push(Header {
            name: "authorization".into(),
            value: "other".into(),
        });
        assert!(validate(&cfg).is_err());
        for name in [
            "Content-Type",
            "Host",
            "Content-Length",
            "Transfer-Encoding",
            "bad name",
        ] {
            cfg.headers = vec![Header {
                name: name.into(),
                value: "secret".into(),
            }];
            assert!(validate(&cfg).is_err());
        }
    }

    #[test]
    fn a_header_saved_without_a_value_keeps_the_stored_one() {
        let stored = endpoint("https://example.com/hook");
        let merged = merge(
            input(
                " https://example.com/new ",
                &[("authorization", None), ("X-Workspace", Some("team"))],
            ),
            Some(&stored),
        )
        .unwrap();
        assert_eq!(merged.url, "https://example.com/new");
        assert_eq!(merged.headers[0].name, "authorization");
        assert_eq!(
            merged.headers[0].value, "Bearer test-token",
            "matched ignoring case"
        );
        assert_eq!(merged.headers[1].value, "team");

        // A new value replaces the stored one; a header left out is dropped.
        let replaced = merge(
            input(
                "https://example.com",
                &[("Authorization", Some("Bearer new"))],
            ),
            Some(&stored),
        )
        .unwrap();
        assert_eq!(replaced.headers.len(), 1);
        assert_eq!(replaced.headers[0].value, "Bearer new");
        assert!(merge(input("https://example.com", &[]), Some(&stored))
            .unwrap()
            .headers
            .is_empty());

        // A renamed header has no stored value to keep, so it has to be given one.
        // (No `unwrap_err`: that needs `Endpoint: Debug`, and a Debug that
        // prints header values is one log line away from leaking them.)
        let Err(error) = merge(
            input("https://example.com", &[("X-Api-Key", None)]),
            Some(&stored),
        ) else {
            panic!("a renamed header must be given a value");
        };
        assert_eq!(error, "Enter a value for the X-Api-Key header.");
        assert!(merge(
            input("https://example.com", &[("Authorization", None)]),
            None
        )
        .is_err());
    }

    #[test]
    fn the_destination_names_the_host_and_never_the_path_or_query() {
        let named = |url: &str| destination(&Url::parse(url).unwrap());
        assert_eq!(
            named("https://hooks.example.com/catch/123/s3cret?token=abc"),
            "hooks.example.com"
        );
        assert_eq!(
            named("https://hooks.example.com:443/x"),
            "hooks.example.com"
        );
        assert_eq!(named("http://localhost:8787/ingest"), "localhost:8787");
    }

    #[test]
    fn local_only_sends_a_governed_note_only_to_a_receiver_on_this_mac() {
        use crate::org_policy::TEST_ORG_WORKSPACE;
        let dir = tempfile::tempdir().unwrap();
        let conn = db::open(&dir.path().join("t.sqlite")).unwrap();
        let url = |u: &str| Url::parse(u).unwrap();
        let ws = Some(TEST_ORG_WORKSPACE);
        assert!(check_policy(&conn, ws, &url("https://hooks.example.com/x")).is_ok());
        crate::org_policy::cache_local_only(&conn, "Northwind");
        assert_eq!(
            check_policy(&conn, ws, &url("https://hooks.example.com/x")).unwrap_err(),
            "Blocked by Northwind: processing stays on this Mac."
        );
        assert!(check_policy(&conn, ws, &url("http://192.168.1.20:8787/ingest")).is_err());
        assert!(check_policy(&conn, ws, &url("http://localhost:8787/ingest")).is_ok());
        assert!(check_policy(&conn, ws, &url("http://127.0.0.1:8787/ingest")).is_ok());
        for other in [None, Some(""), Some("w-other")] {
            assert!(check_policy(&conn, other, &url("https://hooks.example.com/x")).is_ok(), "{other:?}");
        }
        db::set_setting(&conn, "org_config", "{not json").unwrap();
        assert!(check_policy(&conn, ws, &url("http://localhost:8787/ingest")).is_ok());
        assert!(check_policy(&conn, ws, &url("https://hooks.example.com/x")).is_err());
        assert!(check_policy(&conn, None, &url("https://hooks.example.com/x")).is_ok(), "Personal never reads the cache");
    }

    fn facts() -> NoteFacts {
        NoteFacts {
            language: "en".into(),
            folder: json!({ "id": "f1", "name": "Customers" }),
            client: Value::Null,
        }
    }

    #[test]
    fn the_payload_is_an_allowlist_with_no_audio_or_internal_settings() {
        let dir = tempfile::tempdir().unwrap();
        let conn = db::open(&dir.path().join("test.sqlite")).unwrap();
        let mut note = db::create_note(&conn, "en", "meeting", "").unwrap();
        note.title = "Weekly planning".into();
        note.body = "<p>Plan &amp; ship</p>".into();
        note.transcript =
            "Intro\nÅse: Hello\nYou: Hi\nÅse: https://example.com\n12:30 standup".into();
        note.summary = "- Ship it".into();
        note.created_at = 1_791_795_600_000;
        note.audio_path = Some("/private/recording.wav".into());
        let sent_at = DateTime::parse_from_rfc3339("2026-10-12T09:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let payload = build_payload(&note, &facts(), Value::Null, sent_at);

        assert_eq!(payload["schema_version"], 1);
        assert_eq!(payload["event"], "note.sent");
        assert_eq!(payload["sent_at"], "2026-10-12T09:30:00.000Z");
        let body = &payload["note"];
        let mut keys: Vec<_> = body
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "client",
                "created_at",
                "folder",
                "id",
                "language",
                "notes_html",
                "notes_text",
                "recording",
                "speakers",
                "summary",
                "title",
                "transcript",
                "updated_at",
            ],
            "a new field is a change to the documented contract (humla.team/docs/integrations/webhooks)"
        );
        assert_eq!(body["id"], note.id);
        assert_eq!(body["created_at"], "2026-10-12T09:00:00.000Z");
        assert_eq!(body["notes_text"], "Plan & ship");
        assert_eq!(body["speakers"], json!(["Åse", "You"]));
        assert_eq!(body["folder"], json!({ "id": "f1", "name": "Customers" }));
        assert!(body["client"].is_null());
        assert!(body["recording"].is_null());
        assert!(!payload.to_string().contains("/private/"));
    }

    #[test]
    fn an_undetected_language_is_null_rather_than_auto() {
        let dir = tempfile::tempdir().unwrap();
        let conn = db::open(&dir.path().join("test.sqlite")).unwrap();
        let note = db::create_note(&conn, "auto", "meeting", "").unwrap();
        let facts = NoteFacts {
            language: "auto".into(),
            ..facts()
        };
        assert!(
            build_payload(&note, &facts, Value::Null, Utc::now())["note"]["language"].is_null()
        );
    }

    #[test]
    fn folders_and_clients_carry_their_name_or_nothing() {
        let name = |found: Option<&str>| {
            let found = found.map(String::from);
            move |_: &str| Ok(found)
        };
        assert_eq!(
            named(Some("c1"), name(Some("Acme"))).unwrap(),
            json!({ "id": "c1", "name": "Acme" })
        );
        assert!(
            named(Some("c1"), name(None)).unwrap().is_null(),
            "a deleted row reads as no reference"
        );
        assert!(named(None, name(Some("Acme"))).unwrap().is_null());
        assert!(named(Some(""), name(Some("Acme"))).unwrap().is_null());
    }

    fn sessions(entries: Value) -> Vec<sessions::SessionEntry> {
        serde_json::from_value(entries).unwrap()
    }

    #[test]
    fn recording_times_cover_every_take_without_counting_gaps() {
        let takes = sessions(json!([
            { "id": "earlier", "index": 0 },
            { "id": "take-1", "index": 1, "startedAt": "2026-10-12T09:00:00Z", "durationMs": 60000 },
            { "id": "take-2", "index": 2, "startedAt": "2026-10-12T09:05:00+00:00", "durationMs": 120000 }
        ]));
        let timing = recording(&takes, false);
        assert_eq!(timing["started_at"], "2026-10-12T09:00:00.000Z");
        assert_eq!(timing["duration_ms"], 180000);
        assert_eq!(timing["in_progress"], false);
        assert_eq!(
            timing["takes"],
            json!([
                { "index": 1, "started_at": "2026-10-12T09:00:00.000Z", "duration_ms": 60000 },
                { "index": 2, "started_at": "2026-10-12T09:05:00.000Z", "duration_ms": 120000 }
            ]),
            "the repaired \"Earlier transcript\" at index 0 is not a take"
        );

        let live = recording(&takes, true);
        assert_eq!(live["started_at"], "2026-10-12T09:00:00.000Z");
        assert!(
            live["duration_ms"].is_null(),
            "the take in progress has no duration yet"
        );
        assert_eq!(live["in_progress"], true);
    }

    #[test]
    fn a_take_with_unknown_times_nulls_the_totals_but_not_the_other_takes() {
        let takes = sessions(json!([
            { "id": "legacy", "index": 1 },
            { "id": "take-2", "index": 2, "startedAt": "2026-10-12T09:05:00Z", "durationMs": 120000 }
        ]));
        let timing = recording(&takes, false);
        assert!(timing["started_at"].is_null());
        assert!(timing["duration_ms"].is_null());
        assert!(timing["takes"][0]["started_at"].is_null());
        assert_eq!(timing["takes"][1]["duration_ms"], 120000);

        let overflowing = sessions(json!([
            { "id": "a", "index": 1, "startedAt": "2026-10-12T09:00:00Z", "durationMs": u64::MAX },
            { "id": "b", "index": 2, "startedAt": "2026-10-12T09:05:00Z", "durationMs": 1 }
        ]));
        assert!(recording(&overflowing, false)["duration_ms"].is_null());
    }

    #[test]
    fn a_note_never_recorded_has_no_recording() {
        assert!(recording(&[], false).is_null());
        assert!(recording(&sessions(json!([{ "id": "earlier", "index": 0 }])), false).is_null());
        // The first take is still being captured: not in the manifest yet, but
        // a recording all the same.
        let first = recording(&[], true);
        assert_eq!(first["in_progress"], true);
        assert_eq!(first["takes"], json!([]));
    }

    async fn receiver(status: &str, extra: &str) -> (String, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/hook?token=private-url-token",
            listener.local_addr().unwrap()
        );
        let response =
            format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n{extra}\r\n");
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut buf = [0u8; 4096];
                let n = socket.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
                if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&request[..end]).to_lowercase();
                    let length: usize = head
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .unwrap()
                        .parse()
                        .unwrap();
                    if request.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            socket.write_all(response.as_bytes()).await.unwrap();
            String::from_utf8(request).unwrap()
        });
        (url, task)
    }

    #[tokio::test]
    async fn posts_json_with_its_auth_headers_and_accepts_204() {
        let (url, request) = receiver("204 No Content", "").await;
        let payload = json!({ "note": { "id": "stable-id", "transcript": "Åse: Hello" } });
        deliver(&endpoint(&url), &payload).await.unwrap();
        let raw = request.await.unwrap();
        let (headers, body) = raw.split_once("\r\n\r\n").unwrap();
        let headers = headers.to_lowercase();
        assert!(headers.starts_with("post /hook?"));
        assert!(headers.contains("authorization: bearer test-token"));
        assert!(headers.contains("content-type: application/json"));
        assert!(headers.contains("user-agent: humla/"));
        assert_eq!(serde_json::from_str::<Value>(body).unwrap(), payload);
    }

    #[tokio::test]
    async fn reports_http_failures_and_does_not_follow_redirects() {
        for status in [
            "401 Unauthorized",
            "413 Content Too Large",
            "500 Internal Server Error",
            "302 Found",
        ] {
            let (url, request) = receiver(
                status,
                "Location: http://127.0.0.1:1/should-not-be-called\r\n",
            )
            .await;
            let error = deliver(&endpoint(&url), &json!({})).await.unwrap_err();
            assert!(error.contains(&status[..3]));
            assert!(!error.contains("private-url-token"));
            match &status[..3] {
                "302" => assert!(error.contains("Redirects aren't followed")),
                "413" => assert!(error.contains("2 bytes") && error.contains("request body limit")),
                // "Settings" is what gives the toast its Open Settings shortcut.
                "401" => assert!(error.contains("authentication headers in Settings")),
                _ => assert!(error.contains("receiver's logs")),
            }
            request.await.unwrap();
        }
    }

    #[tokio::test]
    async fn an_unreachable_receiver_names_neither_the_url_nor_its_token() {
        // Port 1 on loopback refuses the connection.
        let error = deliver(
            &endpoint("http://127.0.0.1:1/hook?token=private-url-token"),
            &json!({}),
        )
        .await
        .unwrap_err();
        assert!(error.starts_with("Could not reach the webhook."));
        assert!(!error.contains("private-url-token") && !error.contains("127.0.0.1"));
    }
}
