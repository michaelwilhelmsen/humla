//! The client side of the organisation audit log: opening, exporting and saving
//! the audio of notes in an organisation's workspaces, queued in
//! `org_audit_queue` and reported to `POST /api/humla/org/audit` in batches.
//!
//! What is recorded comes from the cached organisation config alone: a
//! workspace one of its organisations lists, and for opens only under that
//! organisation's `audit_reads`. Personal notes and workspaces in no
//! organisation queue nothing. Each report carries the account it was recorded
//! under and is only ever sent as that account. The queue outlives the notes it
//! names, so `db::purge_note_rows` leaves it alone.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use rusqlite::{params, Connection};
use serde::Serialize;

use crate::org_policy::{self, OrgConfig};

pub(crate) const AUDIT_PATH: &str = "/api/humla/org/audit";
/// The user id of the account reports are being recorded under.
pub(crate) const SETTING_ACCOUNT: &str = "org_audit_account";
/// The most events the server takes in one request.
pub(crate) const MAX_BATCH: usize = 100;
/// The most reports kept on this Mac; past it the oldest go.
pub(crate) const MAX_QUEUE: usize = 5_000;
/// One open per note in this window.
pub(crate) const OPEN_DEBOUNCE_MS: i64 = 5 * 60 * 1000;
/// A report the server holds as pending (its note hasn't reached it yet) is
/// tried again after this, and dropped once it has been pending this long.
pub(crate) const PENDING_RETRY_MS: i64 = 5 * 60 * 1000;
pub(crate) const PENDING_MAX_MS: i64 = 30 * 86_400_000;
/// The wait after a server without the route, and for a workspace that
/// refuses for any reason but membership.
const BACKOFF: Duration = Duration::from_secs(15 * 60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Format {
    Pdf,
    /// Markdown and plain text.
    Text,
    /// A panel's Copy button.
    Copy,
    /// Send to webhook.
    Webhook,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    /// Shown in the app.
    Open,
    /// Read by an MCP client.
    OpenViaMcp,
    Export(Format),
    AudioSave,
}

impl Action {
    fn name(self) -> &'static str {
        match self {
            Action::Open | Action::OpenViaMcp => "note.open",
            Action::Export(_) => "note.export",
            Action::AudioSave => "audio.save",
        }
    }

    fn format(self) -> Option<&'static str> {
        match self {
            Action::Export(Format::Pdf) => Some("pdf"),
            Action::Export(Format::Text) => Some("text"),
            Action::Export(Format::Copy) => Some("copy"),
            Action::Export(Format::Webhook) => Some("webhook"),
            Action::Open | Action::OpenViaMcp | Action::AudioSave => None,
        }
    }

    fn via(self) -> Option<&'static str> {
        (self == Action::OpenViaMcp).then_some("mcp")
    }

    fn is_open(self) -> bool {
        matches!(self, Action::Open | Action::OpenViaMcp)
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
        Some(&reads) => !action.is_open() || reads,
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

/// The notes whose open was queued in the last [`OPEN_DEBOUNCE_MS`], per way of
/// opening.
#[derive(Debug, Default)]
pub(crate) struct Debounce(HashMap<(String, bool), i64>);

impl Debounce {
    /// Whether this open at `now_ms` is queued, remembering it if so.
    pub(crate) fn admit(&mut self, action: Action, note_id: &str, now_ms: i64) -> bool {
        self.0.retain(|_, at| (0..OPEN_DEBOUNCE_MS).contains(&(now_ms - *at)));
        let key = (note_id.to_string(), action == Action::OpenViaMcp);
        if self.0.contains_key(&key) {
            return false;
        }
        self.0.insert(key, now_ms);
        true
    }
}

static OPENS: LazyLock<Mutex<Debounce>> = LazyLock::new(|| Mutex::new(Debounce::default()));

/// The account reports are recorded under: `live`, the signed-in user when
/// this process knows one, else the last one recorded (the MCP server, or the
/// app offline). Empty when none is known.
fn account_for(conn: &Connection, live: &str) -> String {
    let live = live.trim();
    if !live.is_empty() {
        if let Err(e) = adopt_account(conn, live) {
            eprintln!("org audit: couldn't switch accounts: {e:#}");
            return String::new();
        }
        return live.to_string();
    }
    crate::db::get_setting(conn, SETTING_ACCOUNT).ok().flatten().unwrap_or_default()
}

/// `user_id` is now signed in on this Mac. When reports were recorded under
/// another account they are discarded, never sent as this one. A user with no
/// organisation and nothing queued writes nothing.
pub(crate) fn adopt_account(conn: &Connection, user_id: &str) -> anyhow::Result<()> {
    let user_id = user_id.trim();
    if user_id.is_empty() {
        return Ok(());
    }
    let recorded_as = crate::db::get_setting(conn, SETTING_ACCOUNT)?.unwrap_or_default();
    if recorded_as == user_id {
        return Ok(());
    }
    let queued: i64 = conn.query_row("SELECT COUNT(*) FROM org_audit_queue", [], |r| r.get(0))?;
    if queued == 0 && scope_in_force(conn).is_empty() {
        return Ok(());
    }
    let dropped = conn.execute("DELETE FROM org_audit_queue WHERE account != ?1", params![user_id])?;
    if dropped > 0 {
        eprintln!("org audit: another account signed in, discarded {dropped} reports of the previous one");
    }
    crate::db::set_setting(conn, SETTING_ACCOUNT, user_id)?;
    Ok(())
}

/// Queue `action` on a note, when its workspace's organisation records it.
/// Never fails its caller: a report that can't be queued is logged.
pub(crate) fn record(conn: &Connection, action: Action, workspace_id: &str, note_id: &str) -> bool {
    if workspace_id.trim().is_empty() || !recorded(&scope_in_force(conn), action, workspace_id) {
        return false;
    }
    let account = account_for(conn, &crate::commands::cloud_auth::current_user_id());
    record_with(conn, &mut OPENS.lock(), action, workspace_id, note_id, &account, crate::db::now_ms())
        .unwrap_or_else(|e| {
            eprintln!("org audit: couldn't queue {}: {e:#}", action.name());
            false
        })
}

/// [`record`] with its debounce, account and clock passed in. Returns whether a
/// row was queued. Nothing is queued without an account to send it as.
pub(crate) fn record_with(
    conn: &Connection,
    opens: &mut Debounce,
    action: Action,
    workspace_id: &str,
    note_id: &str,
    account: &str,
    now_ms: i64,
) -> anyhow::Result<bool> {
    if workspace_id.trim().is_empty() || account.trim().is_empty() {
        return Ok(false);
    }
    if !recorded(&scope_in_force(conn), action, workspace_id) || !reportable(workspace_id, note_id) {
        return Ok(false);
    }
    if action.is_open() && !opens.admit(action, note_id, now_ms) {
        return Ok(false);
    }
    conn.execute(
        "INSERT INTO org_audit_queue (action, workspace_id, note_id, client_at, format, via, account)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![action.name(), workspace_id, note_id, now_ms, action.format(), action.via(), account.trim()],
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

/// Drop reports the server has held as pending for [`PENDING_MAX_MS`].
pub(crate) fn expire_pending(conn: &Connection, now_ms: i64) -> anyhow::Result<usize> {
    let dropped = conn.execute(
        "DELETE FROM org_audit_queue WHERE pending_at > 0 AND pending_at < ?1",
        params![now_ms - PENDING_MAX_MS],
    )?;
    if dropped > 0 {
        eprintln!("org audit: dropped {dropped} reports whose note never reached the server");
    }
    Ok(dropped)
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
}

/// Up to [`MAX_BATCH`] of `account`'s reports that are due at `now_ms`, from
/// the workspace with the oldest one, oldest first, leaving out the workspaces
/// in `skip`. One workspace per batch, so a refusal names the workspace it is
/// about. A note with an upload still in the sync outbox waits, so the server
/// normally has it by the time its report arrives.
pub(crate) fn next_batch(conn: &Connection, account: &str, skip: &[String], now_ms: i64) -> anyhow::Result<Vec<Queued>> {
    let has_outbox: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'sync_outbox')",
        [],
        |r| r.get(0),
    )?;
    let outbox = if has_outbox {
        "AND NOT EXISTS (SELECT 1 FROM sync_outbox o
                         WHERE o.entity = 'note' AND o.op = 'upsert' AND o.entity_id = q.note_id)"
    } else {
        ""
    };
    let due = format!("q.account = ?1 AND q.retry_at <= ?2 {outbox}");
    let workspace: Option<String> = {
        let mut stmt = conn.prepare(&format!(
            "SELECT q.workspace_id FROM org_audit_queue q WHERE {due} GROUP BY q.workspace_id ORDER BY MIN(q.id)"
        ))?;
        let rows = stmt.query_map(params![account, now_ms], |r| r.get::<_, String>(0))?;
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
    let mut stmt = conn.prepare(&format!(
        "SELECT q.id, q.action, q.workspace_id, q.note_id, q.client_at, q.format, q.via FROM org_audit_queue q
         WHERE {due} AND q.workspace_id = ?3 ORDER BY q.id LIMIT ?4"
    ))?;
    let rows = stmt.query_map(params![account, now_ms, workspace, MAX_BATCH as i64], |r| {
        Ok(Queued {
            id: r.get(0)?,
            action: r.get(1)?,
            workspace_id: r.get(2)?,
            note_id: r.get(3)?,
            client_at: r.get(4)?,
            format: r.get(5)?,
            via: r.get(6)?,
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
    /// 200: each event recorded or dropped, and either is final, except those
    /// at the `pending` indices, whose note the server doesn't have yet.
    Done { pending: Vec<usize> },
    /// 403 `not_a_member`: the user isn't in the batch's workspace any more.
    NotMember,
    /// Any other 403, a missing second factor included.
    Refused(String),
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
    let json = serde_json::from_str::<serde_json::Value>(body).ok();
    let reason = json
        .as_ref()
        .and_then(|v| v.get("reason").and_then(|r| r.as_str()).map(String::from))
        .unwrap_or_default();
    match status {
        200..=299 => {
            let pending = json
                .as_ref()
                .and_then(|v| v.get("pending").and_then(|p| p.as_array()).cloned())
                .unwrap_or_default()
                .iter()
                .filter_map(|i| i.as_u64().map(|i| i as usize))
                .collect();
            Reply::Done { pending }
        }
        400 => Reply::Malformed(reason),
        401 => Reply::Unauthorized,
        403 if reason == "not_a_member" => Reply::NotMember,
        403 => Reply::Refused(reason),
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
    /// Leave this workspace's reports alone for this long.
    Skip(String, Duration),
    /// End the round; wait at least this long before the next.
    Stop(Option<Duration>),
}

/// Apply a reply to the queue at `now_ms`: a 200 settles the batch but for
/// its pending events, which wait and are tried again; a workspace the user
/// has left loses its queue; a batch the server will never take is dropped;
/// everything else keeps the reports.
pub(crate) fn settle(conn: &Connection, batch: &[Queued], reply: &Reply, now_ms: i64) -> anyhow::Result<Next> {
    let Some(workspace) = batch.first().map(|q| q.workspace_id.clone()) else {
        return Ok(Next::Continue);
    };
    let delete = |rows: &[&Queued]| -> anyhow::Result<()> {
        let mut stmt = conn.prepare("DELETE FROM org_audit_queue WHERE id = ?1")?;
        for q in rows {
            stmt.execute(params![q.id])?;
        }
        Ok(())
    };
    Ok(match reply {
        Reply::Done { pending } => {
            let held = |i: &usize| pending.contains(i);
            let settled: Vec<&Queued> = batch.iter().enumerate().filter(|(i, _)| !held(i)).map(|(_, q)| q).collect();
            let wait: Vec<&Queued> = batch.iter().enumerate().filter(|(i, _)| held(i)).map(|(_, q)| q).collect();
            delete(&settled)?;
            let mut stmt = conn.prepare(
                "UPDATE org_audit_queue SET retry_at = ?2,
                     pending_at = CASE WHEN pending_at = 0 THEN ?3 ELSE pending_at END
                 WHERE id = ?1",
            )?;
            for q in wait {
                stmt.execute(params![q.id, now_ms + PENDING_RETRY_MS, now_ms])?;
            }
            Next::Continue
        }
        Reply::NotMember => {
            let n = discard_workspace(conn, &workspace)?;
            eprintln!("org audit: no longer a member of a workspace, discarded its {n} reports");
            Next::Continue
        }
        Reply::Malformed(reason) => {
            delete(&batch.iter().collect::<Vec<_>>())?;
            eprintln!("org audit: the server refused a batch of {} reports ({reason}); dropped", batch.len());
            Next::Continue
        }
        Reply::Refused(_) => Next::Skip(workspace, BACKOFF),
        Reply::RateLimited(wait) => Next::Stop(Some(*wait)),
        Reply::Unsupported => Next::Stop(Some(BACKOFF)),
        Reply::Unauthorized | Reply::Later(_) => Next::Stop(None),
    })
}

/// One round at a time, so two callers never send the same batch.
static FLUSHING: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));
/// No round before this, after a 429 or a server without the route.
static NOT_BEFORE: Mutex<Option<Instant>> = Mutex::new(None);
/// Workspaces left alone until then, after a refusal that wasn't about membership.
static WORKSPACE_NOT_BEFORE: LazyLock<Mutex<HashMap<String, Instant>>> = LazyLock::new(Default::default);

/// Send the queue, a batch at a time, as the signed-in user, and only the
/// reports recorded under that user. An empty queue costs one query and no
/// session. Runs from the sync worker's tick, after `cloud_status` and before
/// leaving a workspace.
pub(crate) async fn flush(db: &Arc<Mutex<Connection>>, cache: &crate::keychain::Cache) {
    let Ok(_round) = FLUSHING.try_lock() else { return };
    if NOT_BEFORE.lock().is_some_and(|t| Instant::now() < t) {
        return;
    }
    let queued = db.lock().query_row("SELECT EXISTS (SELECT 1 FROM org_audit_queue)", [], |r| r.get::<_, bool>(0));
    if !queued.unwrap_or(false) {
        return;
    }
    let Ok((_, session)) = crate::commands::cloud_auth::ensure_session_from(db, cache).await else { return };
    let account = session.user_id.trim().to_string();
    if account.is_empty() {
        return;
    }
    {
        let conn = db.lock();
        let ready = adopt_account(&conn, &account).and_then(|_| expire_pending(&conn, crate::db::now_ms()));
        if let Err(e) = ready {
            eprintln!("org audit: couldn't prepare the queue: {e:#}");
            return;
        }
    }
    let mut skip: Vec<String> = {
        let mut held = WORKSPACE_NOT_BEFORE.lock();
        let now = Instant::now();
        held.retain(|_, until| *until > now);
        held.keys().cloned().collect()
    };
    loop {
        let batch = match next_batch(&db.lock(), &account, &skip, crate::db::now_ms()) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("org audit: couldn't read the queue: {e:#}");
                return;
            }
        };
        if batch.is_empty() {
            return;
        }
        let reply = send(db, cache, &account, &batch).await;
        let next = settle(&db.lock(), &batch, &reply, crate::db::now_ms()).unwrap_or_else(|e| {
            eprintln!("org audit: couldn't update the queue: {e:#}");
            Next::Stop(None)
        });
        match next {
            Next::Continue => {}
            Next::Skip(ws, wait) => {
                WORKSPACE_NOT_BEFORE.lock().insert(ws.clone(), Instant::now() + wait);
                skip.push(ws);
            }
            Next::Stop(wait) => {
                if let Some(wait) = wait {
                    *NOT_BEFORE.lock() = Some(Instant::now() + wait);
                }
                return;
            }
        }
    }
}

/// Post one batch as `account`, renewing the session once if the server
/// refuses its token. A session that now belongs to someone else sends nothing.
async fn send(db: &Arc<Mutex<Connection>>, cache: &crate::keychain::Cache, account: &str, batch: &[Queued]) -> Reply {
    use crate::commands::cloud_auth;
    for attempt in 0..2 {
        let (base, session) = match cloud_auth::ensure_session_from(db, cache).await {
            Ok(s) => s,
            Err(e) => return Reply::Later(String::from(e)),
        };
        if session.user_id.trim() != account {
            return Reply::Later("the signed-in account changed".into());
        }
        let url = format!("{}{AUDIT_PATH}", base.trim_end_matches('/'));
        let resp = match crate::http::client()
            .post(url)
            .bearer_auth(&session.token)
            .timeout(REQUEST_TIMEOUT)
            .json(&body(batch))
            .send()
            .await
        {
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
    const ME: &str = "u1";
    const NOW: i64 = 1_800_000_000_000;

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

    type Row = (String, String, String, Option<String>, Option<String>);

    fn queued(conn: &Connection) -> Vec<Row> {
        let mut stmt = conn
            .prepare("SELECT action, workspace_id, note_id, format, via FROM org_audit_queue ORDER BY id")
            .unwrap();
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).unwrap();
        rows.collect::<rusqlite::Result<_>>().unwrap()
    }

    fn count(conn: &Connection) -> usize {
        conn.query_row("SELECT COUNT(*) FROM org_audit_queue", [], |r| r.get::<_, i64>(0)).unwrap() as usize
    }

    fn rec(conn: &Connection, d: &mut Debounce, action: Action, ws: &str, note: &str, now: i64) -> bool {
        record_with(conn, d, action, ws, note, ME, now).unwrap()
    }

    fn batch(conn: &Connection, skip: &[String]) -> Vec<Queued> {
        next_batch(conn, ME, skip, NOW).unwrap()
    }

    const EVERY_ACTION: [Action; 7] = [
        Action::Open,
        Action::OpenViaMcp,
        Action::Export(Format::Pdf),
        Action::Export(Format::Text),
        Action::Export(Format::Copy),
        Action::Export(Format::Webhook),
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
        adopt_account(&conn, ME).unwrap();
        assert_eq!(count(&conn), 0);
        assert_eq!(crate::db::get_setting(&conn, SETTING_ACCOUNT).unwrap(), None, "not even the account is written");
        assert!(batch(&conn, &[]).is_empty(), "so a flush stops before any session or request");
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
            assert!(rec(&conn, &mut d, Action::Export(Format::Webhook), ws, "n1", 1_000));
            assert!(rec(&conn, &mut d, Action::AudioSave, ws, "n1", 1_000));
        }
        let q = queued(&conn);
        assert_eq!(q.len(), 8);
        assert_eq!(q[0], ("note.export".into(), W1.into(), "n1".into(), Some("pdf".into()), None));
        assert_eq!(q[1].3.as_deref(), Some("copy"));
        assert_eq!(q[2].3.as_deref(), Some("webhook"));
        assert_eq!(q[3], ("audio.save".into(), W1.into(), "n1".into(), None, None));
    }

    #[test]
    fn opens_are_queued_only_under_audit_reads() {
        let (_d, conn) = conn();
        cache_orgs(&conn);
        let mut d = Debounce::default();
        assert!(rec(&conn, &mut d, Action::Open, W1, "n1", 1_000));
        assert!(rec(&conn, &mut d, Action::OpenViaMcp, W1, "n1", 1_000), "an MCP read is its own open");
        assert!(!rec(&conn, &mut d, Action::Open, W2, "n2", 1_000), "Contoso has audit_reads off");
        assert!(!rec(&conn, &mut d, Action::OpenViaMcp, W2, "n2", 1_000));
        assert_eq!(
            queued(&conn),
            [
                ("note.open".to_string(), W1.to_string(), "n1".to_string(), None, None),
                ("note.open".to_string(), W1.to_string(), "n1".to_string(), None, Some("mcp".to_string())),
            ]
        );
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
        assert!(d.admit(Action::Open, "n1", 10_000));
        assert!(d.admit(Action::Open, "n2", 10_000 + OPEN_DEBOUNCE_MS));
        assert_eq!(d.0.len(), 1, "n1 was pruned");
        assert!(d.admit(Action::Open, "n1", 5_000), "a clock set back doesn't hold a note forever");
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
    fn nothing_is_queued_without_an_account_to_send_it_as() {
        let (_d, conn) = conn();
        cache_orgs(&conn);
        let mut d = Debounce::default();
        assert!(!record_with(&conn, &mut d, Action::AudioSave, W1, "n1", "", 1).unwrap());
        assert_eq!(account_for(&conn, ""), "", "no session and no account recorded");
        assert_eq!(count(&conn), 0);
    }

    #[test]
    fn the_mcp_server_records_under_the_last_account() {
        let (_d, conn) = conn();
        cache_orgs(&conn);
        adopt_account(&conn, ME).unwrap();
        assert_eq!(account_for(&conn, ""), ME, "no session in this process");
        assert_eq!(account_for(&conn, "bob"), "bob", "a live session wins, and becomes the account");
        assert_eq!(account_for(&conn, ""), "bob");
    }

    #[test]
    fn reports_never_go_out_as_another_account() {
        let (_d, conn) = conn();
        cache_orgs(&conn);
        let mut d = Debounce::default();
        adopt_account(&conn, "alice").unwrap();
        record_with(&conn, &mut d, Action::Export(Format::Pdf), W1, "n1", "alice", 1).unwrap();
        assert!(next_batch(&conn, "bob", &[], NOW).unwrap().is_empty(), "bob's flush never sees alice's report");
        assert_eq!(next_batch(&conn, "alice", &[], NOW).unwrap().len(), 1);
        adopt_account(&conn, "bob").unwrap();
        assert_eq!(count(&conn), 0, "bob signing in discards it");
        assert_eq!(crate::db::get_setting(&conn, SETTING_ACCOUNT).unwrap().as_deref(), Some("bob"));
        record_with(&conn, &mut d, Action::AudioSave, W1, "n2", "bob", 2).unwrap();
        adopt_account(&conn, "bob").unwrap();
        assert_eq!(count(&conn), 1, "the same account keeps its own");
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
        let b = batch(&conn, &[]);
        assert_eq!(b.iter().map(|q| q.note_id.as_str()).collect::<Vec<_>>(), ["first", "last"]);
        let b = batch(&conn, &[W2.to_string()]);
        assert_eq!(b.len(), MAX_BATCH);
        assert!(b.iter().all(|q| q.workspace_id == W1));
        assert_eq!(b[0].note_id, "n0");
        assert!(batch(&conn, &[W1.to_string(), W2.to_string()]).is_empty());
    }

    #[test]
    fn a_note_still_waiting_to_upload_holds_its_report() {
        let (_d, conn) = conn();
        cache_orgs(&conn);
        let mut d = Debounce::default();
        rec(&conn, &mut d, Action::AudioSave, W1, "fresh", 1);
        rec(&conn, &mut d, Action::AudioSave, W1, "synced", 2);
        conn.execute_batch(
            "CREATE TABLE sync_outbox (seq INTEGER PRIMARY KEY, entity TEXT, entity_id TEXT, op TEXT, workspace TEXT,
                                       attempts INTEGER, enqueued_at INTEGER);
             INSERT INTO sync_outbox (entity, entity_id, op, workspace, attempts, enqueued_at)
             VALUES ('note', 'fresh', 'upsert', 'w', 0, 0), ('note', 'synced', 'delete', 'w', 0, 0);",
        )
        .unwrap();
        assert_eq!(batch(&conn, &[]).iter().map(|q| q.note_id.as_str()).collect::<Vec<_>>(), ["synced"]);
        conn.execute("DELETE FROM sync_outbox", []).unwrap();
        assert_eq!(batch(&conn, &[]).len(), 2);
    }

    #[test]
    fn the_body_is_the_servers_event_shape() {
        let q = |id, action: &str, note: &str, format: Option<&str>, via: Option<&str>| Queued {
            id,
            action: action.into(),
            workspace_id: W1.into(),
            note_id: note.into(),
            client_at: 40 + id,
            format: format.map(String::from),
            via: via.map(String::from),
        };
        let b = [q(1, "note.export", "n1", Some("webhook"), None), q(2, "note.open", "n2", None, Some("mcp")), q(3, "note.open", "n3", None, None)];
        assert_eq!(
            body(&b),
            serde_json::json!({ "events": [
                { "action": "note.export", "workspace_id": W1, "note_id": "n1", "client_at": 41, "format": "webhook" },
                { "action": "note.open", "workspace_id": W1, "note_id": "n2", "client_at": 42, "via": "mcp" },
                { "action": "note.open", "workspace_id": W1, "note_id": "n3", "client_at": 43 },
            ]})
        );
    }

    #[test]
    fn classifies_each_answer() {
        assert_eq!(classify(200, r#"{"recorded":1,"dropped":2}"#, None), Reply::Done { pending: vec![] });
        assert_eq!(classify(200, r#"{"recorded":1,"dropped":0,"pending":[0,2,"x",-1]}"#, None), Reply::Done { pending: vec![0, 2] });
        assert_eq!(classify(200, "", None), Reply::Done { pending: vec![] });
        assert_eq!(classify(403, r#"{"reason":"not_a_member"}"#, None), Reply::NotMember);
        assert_eq!(classify(403, r#"{"reason":"two_factor_required"}"#, None), Reply::Refused("two_factor_required".into()));
        assert_eq!(classify(403, "<html>Forbidden</html>", None), Reply::Refused(String::new()));
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
        let b = batch(conn, &[]);
        assert_eq!(b.len(), 2);
        b
    }

    #[test]
    fn a_200_settles_the_batch_dropped_entries_included() {
        let (_d, conn) = conn();
        let b = two_workspaces(&conn);
        rec(&conn, &mut Debounce::default(), Action::AudioSave, W1, "later", 4);
        assert_eq!(settle(&conn, &b, &classify(200, r#"{"recorded":1,"dropped":1}"#, None), NOW).unwrap(), Next::Continue);
        let left: Vec<String> = queued(&conn).into_iter().map(|q| q.2).collect();
        assert_eq!(left, ["b", "later"], "only the rows sent go");
    }

    #[test]
    fn pending_events_wait_are_retried_and_expire() {
        let (_d, conn) = conn();
        let b = two_workspaces(&conn);
        assert_eq!(settle(&conn, &b, &Reply::Done { pending: vec![1, 7] }, NOW).unwrap(), Next::Continue);
        let left: Vec<String> = queued(&conn).into_iter().map(|q| q.2).collect();
        assert_eq!(left, ["b", "c"], "the pending one stays");
        assert_eq!(batch(&conn, &[W2.to_string()]).len(), 0, "and waits");
        let due = NOW + PENDING_RETRY_MS;
        let again = next_batch(&conn, ME, &[W2.to_string()], due).unwrap();
        assert_eq!(again.iter().map(|q| q.note_id.as_str()).collect::<Vec<_>>(), ["c"]);
        settle(&conn, &again, &Reply::Done { pending: vec![0] }, due).unwrap();
        assert_eq!(expire_pending(&conn, NOW + PENDING_MAX_MS).unwrap(), 0, "its age runs from the first pending answer");
        assert_eq!(expire_pending(&conn, NOW + PENDING_MAX_MS + 1).unwrap(), 1);
        assert_eq!(queued(&conn).into_iter().map(|q| q.2).collect::<Vec<_>>(), ["b"], "a report never held pending never expires");
    }

    #[test]
    fn only_not_a_member_discards_that_workspaces_queue() {
        let (_d, conn) = conn();
        let b = two_workspaces(&conn);
        assert_eq!(settle(&conn, &b, &Reply::NotMember, NOW).unwrap(), Next::Continue);
        assert_eq!(queued(&conn).iter().map(|q| q.1.as_str()).collect::<Vec<_>>(), [W2]);
    }

    #[test]
    fn any_other_refusal_keeps_the_reports_and_backs_the_workspace_off() {
        let (_d, conn) = conn();
        let b = two_workspaces(&conn);
        for reason in ["two_factor_required", "", "proxy_blocked"] {
            assert_eq!(settle(&conn, &b, &Reply::Refused(reason.into()), NOW).unwrap(), Next::Skip(W1.into(), BACKOFF));
            assert_eq!(count(&conn), 3, "{reason:?}");
        }
        assert_eq!(batch(&conn, &[W1.to_string()])[0].workspace_id, W2);
    }

    #[test]
    fn failures_keep_every_report() {
        let (_d, conn) = conn();
        let b = two_workspaces(&conn);
        let cases = [
            (Reply::Later("HTTP 503".into()), Next::Stop(None)),
            (Reply::Later("connection refused".into()), Next::Stop(None)),
            (Reply::Unauthorized, Next::Stop(None)),
            (Reply::RateLimited(Duration::from_secs(60)), Next::Stop(Some(Duration::from_secs(60)))),
            (Reply::Unsupported, Next::Stop(Some(BACKOFF))),
        ];
        for (reply, next) in cases {
            assert_eq!(settle(&conn, &b, &reply, NOW).unwrap(), next, "{reply:?}");
            assert_eq!(count(&conn), 3, "{reply:?}");
        }
    }

    #[test]
    fn a_batch_the_server_will_never_take_is_dropped() {
        let (_d, conn) = conn();
        let b = two_workspaces(&conn);
        assert_eq!(settle(&conn, &b, &Reply::Malformed("bad_request".into()), NOW).unwrap(), Next::Continue);
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
