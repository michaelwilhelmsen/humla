//! The client side of the organisation audit log: opening, exporting and saving
//! the audio of notes in an organisation's workspaces, queued in
//! `org_audit_queue` and reported to `POST /api/humla/org/audit` in batches.
//!
//! What is recorded comes from the cached organisation config alone: a
//! workspace one of its organisations lists, and for opens only under that
//! organisation's `audit_reads`. Personal notes and workspaces in no
//! organisation queue nothing. The queue outlives the notes it names, so
//! `db::purge_note_rows` leaves it alone.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use rusqlite::{params, Connection};
use serde::Serialize;

use crate::org_policy::{self, OrgConfig};

pub(crate) const AUDIT_PATH: &str = "/api/humla/org/audit";
/// The most events the server takes in one request.
pub(crate) const MAX_BATCH: usize = 100;
/// The most reports kept on this Mac; past it the oldest go.
pub(crate) const MAX_QUEUE: usize = 5_000;
/// One open per note in this window.
pub(crate) const OPEN_DEBOUNCE_MS: i64 = 5 * 60 * 1000;
/// How long to wait after a server without the route before asking again.
const UNSUPPORTED_WAIT: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Format {
    Pdf,
    /// Markdown and plain text.
    Text,
    /// A panel's Copy button.
    Copy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Open,
    Export(Format),
    AudioSave,
}

impl Action {
    fn name(self) -> &'static str {
        match self {
            Action::Open => "note.open",
            Action::Export(_) => "note.export",
            Action::AudioSave => "audio.save",
        }
    }

    fn format(self) -> Option<&'static str> {
        match self {
            Action::Export(Format::Pdf) => Some("pdf"),
            Action::Export(Format::Text) => Some("text"),
            Action::Export(Format::Copy) => Some("copy"),
            Action::Open | Action::AudioSave => None,
        }
    }
}

// ---- what is recorded --------------------------------------------------------

/// Each workspace an organisation in `config` lists, with whether opens are
/// recorded there. Blank ids never count. Mirrored for the UI as
/// `OrgStatus.audit`.
pub(crate) fn scope(config: &OrgConfig) -> BTreeMap<String, bool> {
    let mut out = BTreeMap::new();
    for org in config.orgs.iter().filter(|o| !o.id.trim().is_empty()) {
        for ws in org.workspaces.iter().filter(|w| !w.trim().is_empty()) {
            *out.entry(ws.clone()).or_insert(false) |= org.policy.audit_reads;
        }
    }
    out
}

/// [`scope`] from the cache. Nothing cached, or a cache that can't be read,
/// records nothing.
pub(crate) fn scope_in_force(conn: &Connection) -> BTreeMap<String, bool> {
    match org_policy::cached(conn) {
        Ok(Some(config)) => scope(&config),
        Ok(None) => BTreeMap::new(),
        Err(e) => {
            eprintln!("org audit: organisation config unreadable, nothing recorded: {e:#}");
            BTreeMap::new()
        }
    }
}

pub(crate) fn recorded(scope: &BTreeMap<String, bool>, action: Action, workspace_id: &str) -> bool {
    match scope.get(workspace_id) {
        Some(&reads) => action != Action::Open || reads,
        None => false,
    }
}

/// Ids the server takes: a PocketBase workspace id, and a note id of 1–64
/// characters. Anything else would get the whole batch refused.
fn reportable(workspace_id: &str, note_id: &str) -> bool {
    workspace_id.len() == 15
        && workspace_id.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && !note_id.is_empty()
        && note_id.len() <= 64
}

/// The notes whose open was queued in the last [`OPEN_DEBOUNCE_MS`].
#[derive(Debug, Default)]
pub(crate) struct Debounce(HashMap<String, i64>);

impl Debounce {
    /// Whether an open of `note_id` at `now_ms` is queued, remembering it if so.
    pub(crate) fn admit(&mut self, note_id: &str, now_ms: i64) -> bool {
        self.0.retain(|_, at| (0..OPEN_DEBOUNCE_MS).contains(&(now_ms - *at)));
        if self.0.contains_key(note_id) {
            return false;
        }
        self.0.insert(note_id.to_string(), now_ms);
        true
    }
}

static OPENS: LazyLock<Mutex<Debounce>> = LazyLock::new(|| Mutex::new(Debounce::default()));

/// Queue `action` on a note, when its workspace's organisation records it.
/// Never fails its caller: a report that can't be queued is logged.
pub(crate) fn record(conn: &Connection, action: Action, workspace_id: &str, note_id: &str) -> bool {
    record_with(conn, &mut OPENS.lock(), action, workspace_id, note_id, crate::db::now_ms()).unwrap_or_else(|e| {
        eprintln!("org audit: couldn't queue {}: {e:#}", action.name());
        false
    })
}

/// [`record`] with its debounce and clock passed in. Returns whether a row was
/// queued.
pub(crate) fn record_with(
    conn: &Connection,
    opens: &mut Debounce,
    action: Action,
    workspace_id: &str,
    note_id: &str,
    now_ms: i64,
) -> anyhow::Result<bool> {
    if workspace_id.trim().is_empty() {
        return Ok(false);
    }
    if !recorded(&scope_in_force(conn), action, workspace_id) || !reportable(workspace_id, note_id) {
        return Ok(false);
    }
    if action == Action::Open && !opens.admit(note_id, now_ms) {
        return Ok(false);
    }
    conn.execute(
        "INSERT INTO org_audit_queue (action, workspace_id, note_id, client_at, format) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![action.name(), workspace_id, note_id, now_ms, action.format()],
    )?;
    trim(conn, MAX_QUEUE)?;
    Ok(true)
}

/// Drop the oldest reports past `max`. Returns how many went.
fn trim(conn: &Connection, max: usize) -> anyhow::Result<usize> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM org_audit_queue", [], |r| r.get(0))?;
    if count as usize <= max {
        return Ok(0);
    }
    let dropped = conn.execute(
        "DELETE FROM org_audit_queue WHERE id NOT IN (SELECT id FROM org_audit_queue ORDER BY id DESC LIMIT ?1)",
        params![max as i64],
    )?;
    eprintln!("org audit: queue full, dropped the {dropped} oldest reports");
    Ok(dropped)
}

/// Forget every report for `workspace_id`, after the user left it.
pub(crate) fn discard_workspace(conn: &Connection, workspace_id: &str) -> anyhow::Result<usize> {
    Ok(conn.execute("DELETE FROM org_audit_queue WHERE workspace_id = ?1", params![workspace_id])?)
}

/// Forget every report, once an authenticated answer says the user is in no
/// organisation.
pub(crate) fn discard_all(conn: &Connection) -> anyhow::Result<usize> {
    Ok(conn.execute("DELETE FROM org_audit_queue", [])?)
}

// ---- sending -------------------------------------------------------------------

/// One queued report, as the server's `events` entry.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct Queued {
    #[serde(skip)]
    pub id: i64,
    pub action: String,
    pub workspace_id: String,
    pub note_id: String,
    pub client_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
}

/// Up to [`MAX_BATCH`] of the reports for the workspace with the oldest one,
/// oldest first, leaving out the workspaces in `skip`. One workspace per
/// batch, so a refusal names the workspace it is about.
pub(crate) fn next_batch(conn: &Connection, skip: &[String]) -> anyhow::Result<Vec<Queued>> {
    let workspace: Option<String> = {
        let mut stmt =
            conn.prepare("SELECT workspace_id FROM org_audit_queue GROUP BY workspace_id ORDER BY MIN(id)")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut found = None;
        for ws in rows {
            let ws = ws?;
            if !skip.contains(&ws) {
                found = Some(ws);
                break;
            }
        }
        found
    };
    let Some(workspace) = workspace else { return Ok(Vec::new()) };
    let mut stmt = conn.prepare(
        "SELECT id, action, workspace_id, note_id, client_at, format FROM org_audit_queue
         WHERE workspace_id = ?1 ORDER BY id LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![workspace, MAX_BATCH as i64], |r| {
        Ok(Queued {
            id: r.get(0)?,
            action: r.get(1)?,
            workspace_id: r.get(2)?,
            note_id: r.get(3)?,
            client_at: r.get(4)?,
            format: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub(crate) fn body(batch: &[Queued]) -> serde_json::Value {
    serde_json::json!({ "events": batch })
}

/// What the server said to one batch.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Reply {
    /// 200: each event recorded or dropped, and either is final.
    Done,
    /// 403: the user isn't a member of the batch's workspace any more.
    NotMember,
    /// 403: the workspace wants a second factor the user hasn't set up.
    TwoFactorRequired,
    /// 401: the session token was refused.
    Unauthorized,
    /// 429: too many reports; wait this long.
    RateLimited(Duration),
    /// 400: the server will never take this batch.
    Malformed(String),
    /// 404: a server without the route.
    Unsupported,
    /// 503, any other answer, or none: try again later.
    Later(String),
}

pub(crate) fn classify(status: u16, body: &str, retry_after: Option<&str>) -> Reply {
    let reason = || {
        serde_json::from_str::<serde_json::Value>(body)
            .ok()
            .and_then(|v| v.get("reason").and_then(|r| r.as_str()).map(String::from))
            .unwrap_or_default()
    };
    match status {
        200..=299 => Reply::Done,
        400 => Reply::Malformed(reason()),
        401 => Reply::Unauthorized,
        403 if reason() == "two_factor_required" => Reply::TwoFactorRequired,
        403 => Reply::NotMember,
        404 => Reply::Unsupported,
        429 => {
            let secs = retry_after.and_then(|v| v.trim().parse::<u64>().ok()).unwrap_or(60).clamp(1, 3600);
            Reply::RateLimited(Duration::from_secs(secs))
        }
        _ => Reply::Later(format!("HTTP {status}")),
    }
}

/// What the round does after a reply.
#[derive(Debug, PartialEq)]
pub(crate) enum Next {
    Continue,
    /// Leave this workspace's reports for a later round.
    Skip(String),
    /// End the round; wait at least this long before the next.
    Stop(Option<Duration>),
}

/// Apply a reply to the queue: a 200 settles the batch, a non-member's queue
/// for that workspace is discarded, a batch the server will never take is
/// dropped, and everything else keeps the reports.
pub(crate) fn settle(conn: &Connection, batch: &[Queued], reply: &Reply) -> anyhow::Result<Next> {
    let Some(workspace) = batch.first().map(|q| q.workspace_id.clone()) else {
        return Ok(Next::Continue);
    };
    let delete_batch = || -> anyhow::Result<()> {
        let mut stmt = conn.prepare("DELETE FROM org_audit_queue WHERE id = ?1")?;
        for q in batch {
            stmt.execute(params![q.id])?;
        }
        Ok(())
    };
    Ok(match reply {
        Reply::Done => {
            delete_batch()?;
            Next::Continue
        }
        Reply::NotMember => {
            let n = discard_workspace(conn, &workspace)?;
            eprintln!("org audit: no longer a member of a workspace, discarded its {n} reports");
            Next::Continue
        }
        Reply::Malformed(reason) => {
            delete_batch()?;
            eprintln!("org audit: the server refused a batch of {} reports ({reason}); dropped", batch.len());
            Next::Continue
        }
        Reply::TwoFactorRequired => Next::Skip(workspace),
        Reply::RateLimited(wait) => Next::Stop(Some(*wait)),
        Reply::Unsupported => Next::Stop(Some(UNSUPPORTED_WAIT)),
        Reply::Unauthorized | Reply::Later(_) => Next::Stop(None),
    })
}

/// One round at a time, so two callers never send the same batch.
static FLUSHING: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));
/// No round before this, after a 429 or a server without the route.
static NOT_BEFORE: Mutex<Option<Instant>> = Mutex::new(None);

/// Send the queue, a batch at a time, as the signed-in user. An empty queue
/// costs one query and no session. Runs from the sync worker's tick, after
/// `cloud_status` and before leaving a workspace.
pub(crate) async fn flush(db: &Arc<Mutex<Connection>>, cache: &crate::keychain::Cache) {
    let Ok(_round) = FLUSHING.try_lock() else { return };
    if NOT_BEFORE.lock().is_some_and(|t| Instant::now() < t) {
        return;
    }
    let mut skip: Vec<String> = Vec::new();
    loop {
        let batch = match next_batch(&db.lock(), &skip) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("org audit: couldn't read the queue: {e:#}");
                return;
            }
        };
        if batch.is_empty() {
            return;
        }
        let reply = send(db, cache, &batch).await;
        let next = settle(&db.lock(), &batch, &reply).unwrap_or_else(|e| {
            eprintln!("org audit: couldn't update the queue: {e:#}");
            Next::Stop(None)
        });
        match next {
            Next::Continue => {}
            Next::Skip(ws) => skip.push(ws),
            Next::Stop(wait) => {
                if let Some(wait) = wait {
                    *NOT_BEFORE.lock() = Some(Instant::now() + wait);
                }
                return;
            }
        }
    }
}

/// Post one batch, renewing the session once if the server refuses its token.
async fn send(db: &Arc<Mutex<Connection>>, cache: &crate::keychain::Cache, batch: &[Queued]) -> Reply {
    use crate::commands::cloud_auth;
    for attempt in 0..2 {
        let (base, session) = match cloud_auth::ensure_session_from(db, cache).await {
            Ok(s) => s,
            Err(e) => return Reply::Later(String::from(e)),
        };
        let url = format!("{}{AUDIT_PATH}", base.trim_end_matches('/'));
        let resp = match crate::http::client().post(url).bearer_auth(&session.token).json(&body(batch)).send().await {
            Ok(r) => r,
            Err(e) => return Reply::Later(e.to_string()),
        };
        let status = resp.status().as_u16();
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        let text = resp.text().await.unwrap_or_default();
        let reply = classify(status, &text, retry_after.as_deref());
        if reply == Reply::Unauthorized {
            cloud_auth::forget_session_token(&session.token);
            if attempt == 0 {
                continue;
            }
        }
        return reply;
    }
    Reply::Unauthorized
}

#[cfg(test)]
mod tests {
    use super::*;

    const W1: &str = "w1aaaaaaaaaaaaa";
    const W2: &str = "w2aaaaaaaaaaaaa";
    const W3: &str = "w3aaaaaaaaaaaaa";

    fn conn() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::db::open(&dir.path().join("notes.sqlite")).unwrap();
        (dir, conn)
    }

    /// Northwind (W1, opens recorded) and Contoso (W2, opens not recorded).
    fn cache_orgs(conn: &Connection) {
        let policy = |reads: bool| {
            format!(
                r#"{{"local_only":false,"retention_days":0,"telemetry_off":false,"audio_forbidden":false,"audit_reads":{reads}}}"#
            )
        };
        let body = format!(
            r#"{{"orgs":[
              {{"id":"o1","name":"Northwind","policy":{},"workspaces":["{W1}"]}},
              {{"id":"o2","name":"Contoso","policy":{},"workspaces":["{W2}"]}}
            ]}}"#,
            policy(true),
            policy(false)
        );
        assert!(org_policy::store(conn, &org_policy::classify(200, &body)).unwrap());
    }

    fn queued(conn: &Connection) -> Vec<(String, String, String, Option<String>)> {
        let mut stmt = conn
            .prepare("SELECT action, workspace_id, note_id, format FROM org_audit_queue ORDER BY id")
            .unwrap();
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap();
        rows.collect::<rusqlite::Result<_>>().unwrap()
    }

    fn count(conn: &Connection) -> usize {
        conn.query_row("SELECT COUNT(*) FROM org_audit_queue", [], |r| r.get::<_, i64>(0)).unwrap() as usize
    }

    fn rec(conn: &Connection, d: &mut Debounce, action: Action, ws: &str, note: &str, now: i64) -> bool {
        record_with(conn, d, action, ws, note, now).unwrap()
    }

    const EVERY_ACTION: [Action; 5] = [
        Action::Open,
        Action::Export(Format::Pdf),
        Action::Export(Format::Text),
        Action::Export(Format::Copy),
        Action::AudioSave,
    ];

    #[test]
    fn a_user_in_no_organisation_queues_nothing_and_sends_nothing() {
        let (_d, conn) = conn();
        let mut d = Debounce::default();
        for ws in ["", W1, "local"] {
            for action in EVERY_ACTION {
                assert!(!rec(&conn, &mut d, action, ws, "n1", 1_000), "nothing cached: {action:?} in {ws:?}");
            }
        }
        org_policy::store(&conn, &org_policy::classify(200, r#"{"orgs":[]}"#)).unwrap();
        for action in EVERY_ACTION {
            assert!(!rec(&conn, &mut d, action, W1, "n1", 2_000), "no organisation: {action:?}");
        }
        assert_eq!(count(&conn), 0);
        assert!(next_batch(&conn, &[]).unwrap().is_empty(), "so a flush stops before any session or request");
    }

    #[test]
    fn personal_notes_and_workspaces_outside_every_organisation_queue_nothing() {
        let (_d, conn) = conn();
        cache_orgs(&conn);
        let mut d = Debounce::default();
        for action in EVERY_ACTION {
            assert!(!rec(&conn, &mut d, action, "", "n1", 1_000), "Personal: {action:?}");
            assert!(!rec(&conn, &mut d, action, W3, "n1", 1_000), "no organisation: {action:?}");
        }
        assert_eq!(count(&conn), 0);
    }

    #[test]
    fn exports_and_saved_audio_are_queued_for_every_organisation_note() {
        let (_d, conn) = conn();
        cache_orgs(&conn);
        let mut d = Debounce::default();
        for ws in [W1, W2] {
            assert!(rec(&conn, &mut d, Action::Export(Format::Pdf), ws, "n1", 1_000));
            assert!(rec(&conn, &mut d, Action::Export(Format::Copy), ws, "n1", 1_000));
            assert!(rec(&conn, &mut d, Action::AudioSave, ws, "n1", 1_000));
        }
        let q = queued(&conn);
        assert_eq!(q.len(), 6);
        assert_eq!(q[0], ("note.export".into(), W1.into(), "n1".into(), Some("pdf".into())));
        assert_eq!(q[1].3.as_deref(), Some("copy"));
        assert_eq!(q[2], ("audio.save".into(), W1.into(), "n1".into(), None));
    }

    #[test]
    fn opens_are_queued_only_under_audit_reads() {
        let (_d, conn) = conn();
        cache_orgs(&conn);
        let mut d = Debounce::default();
        assert!(rec(&conn, &mut d, Action::Open, W1, "n1", 1_000));
        assert!(!rec(&conn, &mut d, Action::Open, W2, "n2", 1_000), "Contoso has audit_reads off");
        assert_eq!(queued(&conn), [("note.open".to_string(), W1.to_string(), "n1".to_string(), None)]);
    }

    #[test]
    fn an_open_is_queued_once_per_note_per_window() {
        let (_d, conn) = conn();
        cache_orgs(&conn);
        let mut d = Debounce::default();
        assert!(rec(&conn, &mut d, Action::Open, W1, "n1", 1_000));
        assert!(!rec(&conn, &mut d, Action::Open, W1, "n1", 2_000), "switching back to it");
        assert!(rec(&conn, &mut d, Action::Open, W1, "n2", 2_000), "another note");
        assert!(!rec(&conn, &mut d, Action::Open, W1, "n1", 1_000 + OPEN_DEBOUNCE_MS - 1));
        assert!(rec(&conn, &mut d, Action::Open, W1, "n1", 1_000 + OPEN_DEBOUNCE_MS));
        assert!(rec(&conn, &mut d, Action::Export(Format::Text), W1, "n1", 1_000 + OPEN_DEBOUNCE_MS), "exports aren't debounced");
        assert_eq!(count(&conn), 4);
    }

    #[test]
    fn a_debounced_note_is_forgotten_once_its_window_passes() {
        let mut d = Debounce::default();
        assert!(d.admit("n1", 10_000));
        assert!(d.admit("n2", 10_000 + OPEN_DEBOUNCE_MS));
        assert_eq!(d.0.len(), 1, "n1 was pruned");
        assert!(d.admit("n1", 5_000), "a clock set back doesn't hold a note forever");
    }

    #[test]
    fn ids_the_server_would_refuse_are_never_queued() {
        let (_d, conn) = conn();
        let body = r#"{"orgs":[{"id":"o1","name":"N","policy":{"local_only":false,"retention_days":0,"telemetry_off":false,"audio_forbidden":false},
            "workspaces":["SHORT","W1AAAAAAAAAAAAA","w1aaaaaaaaaaaaa"]}]}"#;
        org_policy::store(&conn, &org_policy::classify(200, body)).unwrap();
        let mut d = Debounce::default();
        let export = Action::Export(Format::Pdf);
        assert!(!rec(&conn, &mut d, export, "SHORT", "n1", 1));
        assert!(!rec(&conn, &mut d, export, "W1AAAAAAAAAAAAA", "n1", 1));
        assert!(!rec(&conn, &mut d, export, W1, &"x".repeat(65), 1));
        assert!(rec(&conn, &mut d, export, W1, &"x".repeat(64), 1));
    }

    #[test]
    fn an_unreadable_config_records_nothing() {
        let (_d, conn) = conn();
        crate::db::set_setting(&conn, org_policy::SETTING_ORG_CONFIG, "{not json").unwrap();
        assert!(!rec(&conn, &mut Debounce::default(), Action::AudioSave, W1, "n1", 1));
    }

    #[test]
    fn a_full_queue_keeps_the_newest() {
        let (_d, conn) = conn();
        cache_orgs(&conn);
        let mut d = Debounce::default();
        for i in 0..5 {
            rec(&conn, &mut d, Action::AudioSave, W1, &format!("n{i}"), i);
        }
        assert_eq!(trim(&conn, 3).unwrap(), 2);
        let notes: Vec<String> = queued(&conn).into_iter().map(|q| q.2).collect();
        assert_eq!(notes, ["n2", "n3", "n4"]);
        assert_eq!(trim(&conn, 3).unwrap(), 0);
    }

    #[test]
    fn a_batch_is_one_workspace_oldest_first_and_capped() {
        let (_d, conn) = conn();
        cache_orgs(&conn);
        let mut d = Debounce::default();
        rec(&conn, &mut d, Action::AudioSave, W2, "first", 1);
        for i in 0..(MAX_BATCH + 5) {
            rec(&conn, &mut d, Action::Export(Format::Text), W1, &format!("n{i}"), 2 + i as i64);
        }
        rec(&conn, &mut d, Action::AudioSave, W2, "last", 1_000);
        let batch = next_batch(&conn, &[]).unwrap();
        assert_eq!(batch.iter().map(|q| q.note_id.as_str()).collect::<Vec<_>>(), ["first", "last"]);
        let batch = next_batch(&conn, &[W2.to_string()]).unwrap();
        assert_eq!(batch.len(), MAX_BATCH);
        assert!(batch.iter().all(|q| q.workspace_id == W1));
        assert_eq!(batch[0].note_id, "n0");
        assert!(next_batch(&conn, &[W1.to_string(), W2.to_string()]).unwrap().is_empty());
    }

    #[test]
    fn the_body_is_the_servers_event_shape() {
        let batch = [
            Queued { id: 7, action: "note.export".into(), workspace_id: W1.into(), note_id: "n1".into(), client_at: 42, format: Some("pdf".into()) },
            Queued { id: 8, action: "note.open".into(), workspace_id: W1.into(), note_id: "n2".into(), client_at: 43, format: None },
        ];
        assert_eq!(
            body(&batch),
            serde_json::json!({ "events": [
                { "action": "note.export", "workspace_id": W1, "note_id": "n1", "client_at": 42, "format": "pdf" },
                { "action": "note.open", "workspace_id": W1, "note_id": "n2", "client_at": 43 },
            ]})
        );
    }

    #[test]
    fn classifies_each_answer() {
        assert_eq!(classify(200, r#"{"recorded":1,"dropped":2}"#, None), Reply::Done);
        assert_eq!(classify(403, r#"{"reason":"not_a_member"}"#, None), Reply::NotMember);
        assert_eq!(classify(403, r#"{"reason":"two_factor_required"}"#, None), Reply::TwoFactorRequired);
        assert_eq!(classify(401, "", None), Reply::Unauthorized);
        assert_eq!(classify(429, "", Some("60")), Reply::RateLimited(Duration::from_secs(60)));
        assert_eq!(classify(429, "", Some(" 5 ")), Reply::RateLimited(Duration::from_secs(5)));
        assert_eq!(classify(429, "", None), Reply::RateLimited(Duration::from_secs(60)));
        assert_eq!(classify(429, "", Some("Wed, 21 Oct 2026 07:28:00 GMT")), Reply::RateLimited(Duration::from_secs(60)));
        assert_eq!(classify(400, r#"{"reason":"bad_request"}"#, None), Reply::Malformed("bad_request".into()));
        assert_eq!(classify(404, "", None), Reply::Unsupported);
        assert_eq!(classify(503, r#"{"reason":"org_policy_unavailable"}"#, None), Reply::Later("HTTP 503".into()));
        assert_eq!(classify(502, "<html>", None), Reply::Later("HTTP 502".into()));
    }

    /// Two workspaces queued; returns the first batch (W1's).
    fn two_workspaces(conn: &Connection) -> Vec<Queued> {
        cache_orgs(conn);
        let mut d = Debounce::default();
        rec(conn, &mut d, Action::AudioSave, W1, "a", 1);
        rec(conn, &mut d, Action::AudioSave, W2, "b", 2);
        rec(conn, &mut d, Action::Open, W1, "c", 3);
        let batch = next_batch(conn, &[]).unwrap();
        assert_eq!(batch.len(), 2);
        batch
    }

    #[test]
    fn a_200_settles_the_batch_dropped_entries_included() {
        let (_d, conn) = conn();
        let batch = two_workspaces(&conn);
        rec(&conn, &mut Debounce::default(), Action::AudioSave, W1, "later", 4);
        assert_eq!(settle(&conn, &batch, &classify(200, r#"{"recorded":1,"dropped":1}"#, None)).unwrap(), Next::Continue);
        let left: Vec<String> = queued(&conn).into_iter().map(|q| q.2).collect();
        assert_eq!(left, ["b", "later"], "only the rows sent go");
    }

    #[test]
    fn a_403_discards_that_workspaces_queue_only() {
        let (_d, conn) = conn();
        let batch = two_workspaces(&conn);
        assert_eq!(settle(&conn, &batch, &Reply::NotMember).unwrap(), Next::Continue);
        assert_eq!(queued(&conn).iter().map(|q| q.1.as_str()).collect::<Vec<_>>(), [W2]);
    }

    #[test]
    fn a_missing_second_factor_keeps_the_reports_and_skips_the_workspace() {
        let (_d, conn) = conn();
        let batch = two_workspaces(&conn);
        assert_eq!(settle(&conn, &batch, &Reply::TwoFactorRequired).unwrap(), Next::Skip(W1.into()));
        assert_eq!(count(&conn), 3);
        assert_eq!(next_batch(&conn, &[W1.to_string()]).unwrap()[0].workspace_id, W2);
    }

    #[test]
    fn failures_keep_every_report() {
        let (_d, conn) = conn();
        let batch = two_workspaces(&conn);
        let cases = [
            (Reply::Later("HTTP 503".into()), Next::Stop(None)),
            (Reply::Later("connection refused".into()), Next::Stop(None)),
            (Reply::Unauthorized, Next::Stop(None)),
            (Reply::RateLimited(Duration::from_secs(60)), Next::Stop(Some(Duration::from_secs(60)))),
            (Reply::Unsupported, Next::Stop(Some(UNSUPPORTED_WAIT))),
        ];
        for (reply, next) in cases {
            assert_eq!(settle(&conn, &batch, &reply).unwrap(), next, "{reply:?}");
            assert_eq!(count(&conn), 3, "{reply:?}");
        }
    }

    #[test]
    fn a_batch_the_server_will_never_take_is_dropped() {
        let (_d, conn) = conn();
        let batch = two_workspaces(&conn);
        assert_eq!(settle(&conn, &batch, &Reply::Malformed("bad_request".into())).unwrap(), Next::Continue);
        assert_eq!(count(&conn), 1);
    }

    #[test]
    fn leaving_discards_the_queue() {
        let (_d, conn) = conn();
        two_workspaces(&conn);
        assert_eq!(discard_workspace(&conn, W2).unwrap(), 1);
        assert_eq!(discard_all(&conn).unwrap(), 2);
        assert_eq!(count(&conn), 0);
    }

    #[test]
    fn purging_a_note_keeps_its_trail() {
        let (_d, conn) = conn();
        cache_orgs(&conn);
        let note = crate::db::create_note(&conn, "", "meeting", W1).unwrap();
        assert!(rec(&conn, &mut Debounce::default(), Action::AudioSave, W1, &note.id, 1));
        crate::db::purge_note(&conn, &note.id).unwrap();
        assert_eq!(count(&conn), 1);
    }

    #[test]
    fn scope_marks_where_opens_are_recorded() {
        let config: OrgConfig = serde_json::from_str(&format!(
            r#"{{"orgs":[
              {{"id":"o1","name":"A","policy":{{"local_only":false,"retention_days":0,"telemetry_off":false,"audio_forbidden":false,"audit_reads":true}},"workspaces":["{W1}",""," "]}},
              {{"id":"o2","name":"B","policy":{{"local_only":false,"retention_days":0,"telemetry_off":false,"audio_forbidden":false}},"workspaces":["{W2}"]}},
              {{"id":" ","name":"C","policy":{{"local_only":false,"retention_days":0,"telemetry_off":false,"audio_forbidden":false,"audit_reads":true}},"workspaces":["{W3}"]}}
            ]}}"#
        ))
        .unwrap();
        let expected: BTreeMap<String, bool> = [(W1.to_string(), true), (W2.to_string(), false)].into();
        assert_eq!(scope(&config), expected);
    }

    #[test]
    fn a_managed_profile_never_turns_audit_reads_on() {
        use org_policy::{force_for_test, ManagedValue};
        force_for_test(&[("local_only", ManagedValue::Bool(true)), ("audit_reads", ManagedValue::Bool(true))]);
        org_policy::reload_managed();
        assert!(!org_policy::managed().layer.unwrap().policy.audit_reads);
        force_for_test(&[]);
        org_policy::reload_managed();
    }
}
