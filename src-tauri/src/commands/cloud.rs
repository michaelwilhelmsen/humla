//! Cloud / teams management commands.
//!
//! Plain PocketBase HTTP via reqwest — deliberately NO dependency on the
//! private `cloud-sync` crate, so the open-source core still builds standalone.
//! This covers auth, workspaces, and membership/roles so the teams UI is fully
//! functional. The live note/folder *sync worker* (cloud-sync) is wired
//! separately and is gated by the open-core packaging decision (see the
//! humla-cloud repo).
//!
//! Session model:
//!  - `cloud_base_url` + `cloud_workspace_id` persist in the settings table.
//!  - Credentials (email + password) live in the macOS Keychain.
//!  - A process-lifetime `SESSION` cache holds the auth token + user; it
//!    auto-logs-in from the stored credentials when a command needs a token.
//!
//! Roles are derived from a workspace's relation fields: `owner` (single) =
//! owner, anyone in `admins` = admin, anyone else in `members` = member.

use super::err;
use crate::db;
use crate::AppState;
use serde::Serialize;
use std::sync::{LazyLock, Mutex};
use tauri::{Manager, State};

const CRED_ACCOUNT: &str = "cloud_credentials";
// `pub(crate)` so the cloud-sync worker glue (`commands::cloud_worker`) can read
// the same persisted config keys + credentials instead of duplicating them.
pub(crate) const SETTING_BASE_URL: &str = "cloud_base_url";
pub(crate) const SETTING_WORKSPACE: &str = "cloud_workspace_id";

#[derive(Clone, Default)]
struct Session {
    token: String,
    user_id: String,
    email: String,
    name: String,
    verified: bool,
}

/// Process-lifetime auth cache. Kept out of `AppState` so the whole cloud layer
/// stays self-contained and easy to extract.
static SESSION: LazyLock<Mutex<Option<Session>>> = LazyLock::new(|| Mutex::new(None));

#[derive(Serialize, Clone)]
pub struct CloudUser {
    pub id: String,
    pub email: String,
    pub name: String,
    /// Whether the user has confirmed their email. Drives the Account tab's
    /// "verify your email / resend" banner.
    pub verified: bool,
}

#[derive(Serialize, Clone)]
pub struct CloudWorkspace {
    pub id: String,
    pub name: String,
    pub role: String,
    /// Stripe subscription status for this workspace: "active" / "trialing" /
    /// "past_due" / "canceled" / "none". Only meaningful when billing is enabled
    /// on the server; self-host leaves it "none" and the UI ignores it.
    pub plan_status: String,
    /// Number of billed seats (= workspace members) on the subscription. `None`
    /// when the server predates per-seat billing or the row carries no usable
    /// count; the UI hides the seat/price rows in that case.
    pub seats: Option<u32>,
}

#[derive(Serialize)]
pub struct CloudStatus {
    /// A server URL is configured.
    pub configured: bool,
    /// We hold (or could auto-acquire) a valid session.
    pub logged_in: bool,
    pub base_url: String,
    pub user: Option<CloudUser>,
    pub current_workspace: Option<CloudWorkspace>,
    pub workspaces: Vec<CloudWorkspace>,
    /// True when the server enforces billing (humla-cloud). Self-host → false, and
    /// the client hides the billing UI and never treats a workspace as locked.
    pub billing_enabled: bool,
    /// Per-seat price in the smallest currency unit (Stripe `unit_amount`, e.g.
    /// 500 = $5.00). `None` when the server doesn't advertise it (older builds),
    /// in which case the UI shows the seat count without pricing.
    pub seat_price_cents: Option<u32>,
    /// Lowercase ISO currency for `seat_price_cents` (e.g. "usd"). `None` when
    /// unknown; the formatter falls back to USD.
    pub seat_currency: Option<String>,
    /// Managed chat add-on config, advertised when the server has it set up
    /// (issue #75). `None` on self-host / older servers → the client drops the
    /// add-on pitch entirely.
    pub chat_addon: Option<ChatAddon>,
}

/// The managed chat add-on the server advertises in its billing config (#75).
/// `available` gates the add-on pitch; the price fields format it; `price_id`
/// is for the future purchase UI (activation is via Stripe today).
#[derive(Serialize, Default, PartialEq, Debug)]
pub struct ChatAddon {
    pub available: bool,
    pub price_id: Option<String>,
    pub price_cents: Option<u32>,
    pub currency: Option<String>,
}

#[derive(Serialize)]
pub struct CloudMember {
    pub id: String,
    pub email: String,
    pub name: String,
    pub role: String,
}

fn http() -> reqwest::Client {
    reqwest::Client::new()
}

// ---- persisted config (settings table) -------------------------------------

fn read_base_url(state: &State<'_, AppState>) -> Option<String> {
    let conn = state.db.lock();
    db::get_setting(&conn, SETTING_BASE_URL)
        .ok()
        .flatten()
        .filter(|s| !s.trim().is_empty())
}

fn read_workspace_id(state: &State<'_, AppState>) -> Option<String> {
    let conn = state.db.lock();
    db::get_setting(&conn, SETTING_WORKSPACE)
        .ok()
        .flatten()
        .filter(|s| !s.trim().is_empty())
}

/// The active workspace id (PocketBase workspace), or `""` for Personal /
/// local-only. Used to stamp new notes/folders and to scope the note/folder
/// lists. Takes a live connection so callers that already hold the db guard can
/// reuse it.
pub(crate) fn active_workspace(conn: &rusqlite::Connection) -> String {
    db::get_setting(conn, SETTING_WORKSPACE).ok().flatten().unwrap_or_default()
}

// ---- credentials (Keychain) ------------------------------------------------

fn cred_entry() -> Result<keyring::Entry, String> {
    keyring::Entry::new(crate::stt::KEYCHAIN_SERVICE, CRED_ACCOUNT).map_err(|e| e.to_string())
}

pub(crate) fn read_creds() -> Option<(String, String)> {
    // Dev convenience: in debug builds, take credentials from env vars if set,
    // skipping the Keychain entirely. `tauri dev` re-signs the binary on every
    // rebuild, so the macOS Keychain "Always Allow" grant never sticks — and
    // because the sync worker reads creds at boot, that means a prompt on every
    // relaunch. Setting HUMLA_DEV_SYNC_EMAIL + HUMLA_DEV_SYNC_PASSWORD avoids it.
    // Release builds are Developer-ID signed (grant sticks) and always use the
    // Keychain; the env path is compiled out entirely.
    #[cfg(debug_assertions)]
    {
        let email = std::env::var("HUMLA_DEV_SYNC_EMAIL").unwrap_or_default();
        let password = std::env::var("HUMLA_DEV_SYNC_PASSWORD").unwrap_or_default();
        if !email.is_empty() && !password.is_empty() {
            return Some((email, password));
        }
    }
    let raw = cred_entry().ok()?.get_password().ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    Some((
        v.get("email")?.as_str()?.to_string(),
        v.get("password")?.as_str()?.to_string(),
    ))
}

fn write_creds(email: &str, password: &str) -> Result<(), String> {
    let json = serde_json::json!({ "email": email, "password": password }).to_string();
    cred_entry()?.set_password(&json).map_err(|e| e.to_string())
}

fn clear_creds() {
    if let Ok(entry) = cred_entry() {
        let _ = entry.delete_credential();
    }
}

// ---- HTTP helpers ----------------------------------------------------------

async fn pb_json(resp: reqwest::Response) -> Result<serde_json::Value, String> {
    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        // Stale/expired token — drop the cached session so the next command
        // auto-re-authenticates from the stored credentials (ensure_session)
        // instead of reusing the dead token.
        *SESSION.lock().unwrap() = None;
    }
    let val: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("pocketbase: invalid response ({e})"))?;
    if !status.is_success() {
        let msg = val
            .get("message")
            .and_then(|m| m.as_str())
            .or_else(|| val.get("error").and_then(|m| m.as_str()))
            .unwrap_or("request failed");
        return Err(format!("pocketbase {status}: {msg}"));
    }
    Ok(val)
}

async fn authed_get(
    base: &str,
    token: &str,
    path: &str,
    query: &[(&str, &str)],
) -> Result<serde_json::Value, String> {
    let resp = http()
        .get(format!("{base}{path}"))
        .bearer_auth(token)
        .query(query)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    pb_json(resp).await
}

async fn authed_post(
    base: &str,
    token: &str,
    path: &str,
    body: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let resp = http()
        .post(format!("{base}{path}"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    pb_json(resp).await
}

async fn authed_patch(
    base: &str,
    token: &str,
    path: &str,
    body: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let resp = http()
        .patch(format!("{base}{path}"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    pb_json(resp).await
}

/// DELETE a record. PocketBase returns 204 No Content on success (empty body),
/// so this checks status directly instead of parsing JSON like `pb_json`.
async fn authed_delete(base: &str, token: &str, path: &str) -> Result<(), String> {
    let resp = http()
        .delete(format!("{base}{path}"))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if resp.status().is_success() {
        return Ok(());
    }
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    let msg = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(String::from))
        .unwrap_or(body);
    Err(format!("pocketbase {status}: {msg}"))
}

// ---- auth ------------------------------------------------------------------

async fn login_request(base: &str, email: &str, password: &str) -> Result<Session, String> {
    let resp = http()
        .post(format!("{base}/api/collections/users/auth-with-password"))
        .json(&serde_json::json!({ "identity": email, "password": password }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let val = pb_json(resp)
        .await
        .map_err(|e| format!("login failed — check the server URL, email, and password ({e})"))?;
    let token = val.get("token").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let record = val.get("record");
    Ok(Session {
        token,
        user_id: record.and_then(|r| r.get("id")).and_then(|v| v.as_str()).unwrap_or_default().to_string(),
        email: record.and_then(|r| r.get("email")).and_then(|v| v.as_str()).unwrap_or_default().to_string(),
        name: record.and_then(|r| r.get("name")).and_then(|v| v.as_str()).unwrap_or_default().to_string(),
        verified: record.and_then(|r| r.get("verified")).and_then(|v| v.as_bool()).unwrap_or(false),
    })
}

/// Refresh the signed-in user's record (to pick up `verified` after they click
/// the email link) and rotate the auth token. Best-effort, used by `cloud_status`.
async fn auth_refresh(base: &str, token: &str) -> Result<Session, String> {
    let resp = http()
        .post(format!("{base}/api/collections/users/auth-refresh"))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let val = pb_json(resp).await?;
    let new_token = val.get("token").and_then(|v| v.as_str()).unwrap_or(token).to_string();
    let record = val.get("record");
    Ok(Session {
        token: new_token,
        user_id: record.and_then(|r| r.get("id")).and_then(|v| v.as_str()).unwrap_or_default().to_string(),
        email: record.and_then(|r| r.get("email")).and_then(|v| v.as_str()).unwrap_or_default().to_string(),
        name: record.and_then(|r| r.get("name")).and_then(|v| v.as_str()).unwrap_or_default().to_string(),
        verified: record.and_then(|r| r.get("verified")).and_then(|v| v.as_bool()).unwrap_or(false),
    })
}

/// Return a usable (base_url, session). Uses the cached session, else
/// auto-logs-in from stored credentials.
async fn ensure_session(state: &State<'_, AppState>) -> Result<(String, Session), String> {
    let base = read_base_url(state).ok_or("Cloud isn't configured — set the server URL first.")?;
    let cached = SESSION.lock().unwrap().clone();
    if let Some(s) = cached {
        return Ok((base, s));
    }
    let (email, password) = read_creds().ok_or("Not signed in.")?;
    let session = login_request(&base, &email, &password).await?;
    *SESSION.lock().unwrap() = Some(session.clone());
    Ok((base, session))
}

/// Cloud `(base_url, auth token)` for a command that calls the server directly
/// (Teams chat, issue #50). Reuses the cached session and the auto-login-from-
/// Keychain path. The token may be stale — a 401 from the endpoint should call
/// [`forget_session`] and retry once, mirroring `pb_json`'s reactive refresh.
pub(crate) async fn cloud_session(state: &State<'_, AppState>) -> Result<(String, String), String> {
    let (base, session) = ensure_session(state).await?;
    Ok((base, session.token))
}

/// Drop the cached session so the next [`cloud_session`] re-authenticates from
/// stored credentials — call after a 401 from a direct endpoint call.
pub(crate) fn forget_session() {
    *SESSION.lock().unwrap() = None;
}

// ---- direct endpoint requests (chat service + hook routes) ------------------
// The chat surfaces don't go through the records API, so `pb_json` and the
// `authed_*` helpers above don't serve them: they call the chat service and the
// PocketBase hook routes directly, and every one of those calls needs the same
// skeleton — resolve the session, bearer the PB token, re-authenticate once on a
// 401, then read either the JSON body or the server's `{reason, error}` pair.
// `cloud_get_json` / `cloud_post_json` / `cloud_delete_json` own that skeleton so
// each call site is left with only what actually differs: its 404 behavior and
// its error taxonomy (issue #72). The one direct call that can't use them is
// `chat_send_cloud`, which streams SSE rather than returning a JSON body.

/// Why a direct cloud request didn't produce a JSON body.
///
/// Transport and session failures arrive pre-rendered (they're already
/// user-facing strings). A non-2xx keeps its `status` — so a caller can branch
/// on 404, the "route absent on an older/self-hosted server" signal — and hands
/// back the server's `{reason, error}` pair verbatim, because the chat surfaces
/// map the same reasons to different wording (workspace chat through
/// `cloud_chat_error_message`, the BYOK key routes through
/// `chat_key_error_message`). Use [`CloudReqError::message`] to render.
#[derive(Debug)]
pub(crate) enum CloudReqError {
    /// Not configured / not signed in / network failure — already a user message.
    Unreachable(String),
    /// A non-2xx response, including a 401 that survived the one-shot re-auth.
    Status {
        status: reqwest::StatusCode,
        reason: String,
        server_message: String,
    },
}

impl CloudReqError {
    /// True only for a 404 — the endpoint isn't on this server.
    pub(crate) fn is_not_found(&self) -> bool {
        matches!(self, Self::Status { status, .. } if *status == reqwest::StatusCode::NOT_FOUND)
    }

    /// Render with the caller's error taxonomy: a transport/session failure
    /// passes through verbatim, a server status maps its `{reason, error}` pair
    /// through `map` (e.g. `chat::cloud::cloud_chat_error_message`).
    pub(crate) fn message(self, map: fn(&str, &str) -> String) -> String {
        match self {
            Self::Unreachable(m) => m,
            Self::Status { reason, server_message, .. } => map(&reason, &server_message),
        }
    }
}

/// A direct request: the verb plus what it carries. One value, so a body can't
/// be handed to a GET or query pairs to a POST.
#[derive(Clone, Copy)]
enum Request<'a> {
    Get(&'a [(&'a str, &'a str)]),
    Post(&'a serde_json::Value),
    Patch(&'a serde_json::Value),
    Delete(&'a [(&'a str, &'a str)]),
}

/// GET a direct cloud endpoint as JSON. `path` is absolute (`/api/chat/usage`).
pub(crate) async fn cloud_get_json(
    state: &State<'_, AppState>,
    path: &str,
    query: &[(&str, &str)],
) -> Result<serde_json::Value, CloudReqError> {
    request_json(state, path, Request::Get(query)).await
}

/// POST a JSON body to a direct cloud endpoint. `path` is absolute.
pub(crate) async fn cloud_post_json(
    state: &State<'_, AppState>,
    path: &str,
    body: &serde_json::Value,
) -> Result<serde_json::Value, CloudReqError> {
    request_json(state, path, Request::Post(body)).await
}

/// DELETE a direct cloud endpoint, reading the JSON body it returns. `path` is
/// absolute. (Unlike [`authed_delete`], these hook routes answer with a body.)
pub(crate) async fn cloud_delete_json(
    state: &State<'_, AppState>,
    path: &str,
    query: &[(&str, &str)],
) -> Result<serde_json::Value, CloudReqError> {
    request_json(state, path, Request::Delete(query)).await
}

/// PATCH a direct cloud endpoint as JSON — a partial update of one record
/// (issue #109's conversation rename is the first).
pub(crate) async fn cloud_patch_json(
    state: &State<'_, AppState>,
    path: &str,
    body: &serde_json::Value,
) -> Result<serde_json::Value, CloudReqError> {
    request_json(state, path, Request::Patch(body)).await
}

/// The shared skeleton: session → bearer → send → one-shot 401 re-auth → JSON.
/// A malformed body is `Value::Null` rather than an error, matching the call
/// sites' own `unwrap_or_default` (their parsers already treat a missing field
/// as absent), so a garbled response degrades instead of surfacing a parse error.
async fn request_json(
    state: &State<'_, AppState>,
    path: &str,
    request: Request<'_>,
) -> Result<serde_json::Value, CloudReqError> {
    let mut attempt = 0u8;
    loop {
        let (base, token) = cloud_session(state).await.map_err(CloudReqError::Unreachable)?;
        let url = format!("{}{}", base.trim_end_matches('/'), path);
        let req = match request {
            Request::Get(q) => http().get(url).query(q),
            Request::Post(b) => http().post(url).json(b),
            Request::Patch(b) => http().patch(url).json(b),
            Request::Delete(q) => http().delete(url).query(q),
        };
        let resp = req
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| CloudReqError::Unreachable(format!("Couldn't reach Team chat: {e}")))?;
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED && attempt == 0 {
            // Stale token — drop the cached session and retry once with a fresh one.
            forget_session();
            attempt += 1;
            continue;
        }
        if !status.is_success() {
            let body: serde_json::Value = resp.json().await.unwrap_or_default();
            let field = |k: &str| {
                body.get(k)
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string()
            };
            return Err(CloudReqError::Status {
                status,
                reason: field("reason"),
                server_message: field("error"),
            });
        }
        return Ok(resp.json().await.unwrap_or_default());
    }
}

/// Read-through fetch of a workspace conversation's messages (issue #50).
/// Workspace chat is server-authoritative: history lives in PocketBase's
/// member-readable `chat_messages`, scoped by the caller's token. Returns the
/// raw records sorted by `seq`; the chat command maps them to the UI DTO.
pub(crate) async fn fetch_chat_messages(
    state: &State<'_, AppState>,
    conversation_id: &str,
) -> Result<Vec<serde_json::Value>, String> {
    let (base, session) = ensure_session(state).await?;
    let filter = format!("conversation=\"{conversation_id}\"");
    let val = authed_get(
        &base,
        &session.token,
        "/api/collections/chat_messages/records",
        &[("filter", filter.as_str()), ("sort", "seq"), ("perPage", "200")],
    )
    .await?;
    Ok(val
        .get("items")
        .and_then(|i| i.as_array())
        .cloned()
        .unwrap_or_default())
}

// ---- role / relation helpers -----------------------------------------------

fn ids(val: &serde_json::Value, field: &str) -> Vec<String> {
    val.get(field)
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

fn derive_role(ws: &serde_json::Value, user_id: &str) -> String {
    if ws.get("owner").and_then(|v| v.as_str()) == Some(user_id) {
        return "owner".into();
    }
    if ids(ws, "admins").iter().any(|a| a == user_id) {
        return "admin".into();
    }
    if ids(ws, "viewers").iter().any(|a| a == user_id) {
        return "viewer".into();
    }
    "member".into()
}

/// Parse a subscription row's `seats` count. The server encodes it as a JSON
/// number (integer or float, depending on how PocketBase/Stripe round-trips it),
/// so accept either. Missing, zero, negative, or non-numeric all mean "no usable
/// seat count" → `None`, which the UI treats as pre-per-seat-billing.
fn parse_seats(sub: &serde_json::Value) -> Option<u32> {
    let n = sub.get("seats")?.as_f64()?;
    if !n.is_finite() || n < 1.0 {
        return None;
    }
    Some(n.round() as u32)
}

async fn list_workspaces_inner(base: &str, session: &Session) -> Result<Vec<CloudWorkspace>, String> {
    // No client-side filter: the workspaces collection listRule is already
    // `members.id ?= @request.auth.id`, so the server returns exactly the
    // workspaces this user belongs to. Re-sending `members.id ?= '<id>'` as a
    // query filter 400s in PocketBase 0.39 — the user-filter's join collides with
    // the rule's own join on `members` — which `cloud_status` then swallowed into
    // an empty workspace list (the switcher showed nothing for members/viewers).
    let val = authed_get(
        base,
        &session.token,
        "/api/collections/workspaces/records",
        &[("perPage", "200"), ("sort", "name")],
    )
    .await?;
    let items = val.get("items").and_then(|v| v.as_array()).cloned().unwrap_or_default();

    // Per-workspace subscription (status + billed seat count). The subscriptions
    // listRule scopes to the user's memberships, so an unfiltered fetch returns
    // exactly their rows. Best-effort: any error just leaves statuses "none"
    // (billing_enabled gates the UI), and self-host servers simply have no rows.
    let mut sub_by_ws: std::collections::HashMap<String, (String, Option<u32>)> =
        std::collections::HashMap::new();
    if let Ok(subs) =
        authed_get(base, &session.token, "/api/collections/subscriptions/records", &[("perPage", "200")]).await
    {
        if let Some(arr) = subs.get("items").and_then(|v| v.as_array()) {
            for it in arr {
                let ws = it.get("workspace").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                let st = it.get("status").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                if !ws.is_empty() {
                    sub_by_ws.insert(ws, (st, parse_seats(it)));
                }
            }
        }
    }

    Ok(items
        .iter()
        .map(|it| {
            let id = it.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            let (plan_status, seats) = sub_by_ws
                .get(&id)
                .cloned()
                .unwrap_or_else(|| ("none".to_string(), None));
            CloudWorkspace {
                id,
                name: it.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                role: derive_role(it, &session.user_id),
                plan_status,
                seats,
            }
        })
        .collect())
}

/// Public billing config from `/api/humla/billing/config`. `enabled` says whether
/// the server enforces billing (humla-cloud) vs runs free (self-host); the seat
/// price fields are advertised only by servers that support per-seat billing.
#[derive(Default)]
struct BillingConfig {
    enabled: bool,
    seat_price_cents: Option<u32>,
    seat_currency: Option<String>,
    chat_addon: Option<ChatAddon>,
}

/// Parse the `chat_addon` object from the billing config (issue #75). Returns
/// None when the key is absent (self-host / add-on not configured) so the client
/// drops the pitch; a present-but-malformed object degrades to `available:false`.
fn parse_chat_addon(val: &serde_json::Value) -> Option<ChatAddon> {
    let obj = val.get("chat_addon")?.as_object()?;
    Some(ChatAddon {
        available: obj.get("available").and_then(|v| v.as_bool()).unwrap_or(false),
        price_id: obj
            .get("price_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string),
        // Stripe `unit_amount` in cents; accept int/float, drop negatives/NaN.
        price_cents: obj
            .get("price_cents")
            .and_then(|v| v.as_f64())
            .filter(|n| n.is_finite() && *n >= 0.0)
            .map(|n| n.round() as u32),
        currency: obj
            .get("currency")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string),
    })
}

/// Read the public billing config. Any failure (older server, network) degrades
/// to the free/self-host experience (`enabled: false`, no pricing). The seat
/// price fields are optional — today's prod returns only `{ "enabled": true }`.
async fn fetch_billing_config(base: &str) -> BillingConfig {
    let Ok(resp) = http().get(format!("{base}/api/humla/billing/config")).send().await else {
        return BillingConfig::default();
    };
    let Ok(val) = resp.json::<serde_json::Value>().await else {
        return BillingConfig::default();
    };
    let enabled = val.get("enabled").and_then(|b| b.as_bool()).unwrap_or(false);
    // Stripe `unit_amount` in cents. Accept integer or float encodings; drop
    // non-numeric / negative values so the UI falls back to "price unknown".
    let seat_price_cents = val
        .get("seat_price_cents")
        .and_then(|v| v.as_f64())
        .filter(|n| n.is_finite() && *n >= 0.0)
        .map(|n| n.round() as u32);
    let seat_currency = val
        .get("seat_currency")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.trim().is_empty());
    let chat_addon = parse_chat_addon(&val);
    BillingConfig { enabled, seat_price_cents, seat_currency, chat_addon }
}

// ---- commands --------------------------------------------------------------

#[tauri::command]
pub async fn cloud_status(state: State<'_, AppState>) -> Result<CloudStatus, String> {
    let base = read_base_url(&state);
    let configured = base.is_some();
    let base_url = base.clone().unwrap_or_default();

    let Some((base_url, session)) = ensure_session(&state).await.ok() else {
        return Ok(CloudStatus {
            configured,
            logged_in: false,
            base_url,
            user: None,
            current_workspace: None,
            workspaces: vec![],
            billing_enabled: false,
            seat_price_cents: None,
            seat_currency: None,
            chat_addon: None,
        });
    };

    // The cached session's `verified` can be stale (the user may have clicked the
    // verification link since login). Refresh the record + rotate the token,
    // best-effort — fall back to the cached session on any failure.
    let session = match auth_refresh(&base_url, &session.token).await {
        Ok(s) => {
            *SESSION.lock().unwrap() = Some(s.clone());
            s
        }
        Err(_) => session,
    };

    let workspaces = match list_workspaces_inner(&base_url, &session).await {
        Ok(ws) => ws,
        // Don't crash status on a transient list failure, but don't swallow it
        // silently either — an empty list reads as "no workspaces" and hid a 400
        // here for a long time.
        Err(e) => {
            eprintln!("cloud: failed to list workspaces: {e}");
            vec![]
        }
    };
    let current_id = read_workspace_id(&state);
    let current_workspace = current_id
        .as_ref()
        .and_then(|id| workspaces.iter().find(|w| &w.id == id).cloned());

    let billing = fetch_billing_config(&base_url).await;
    Ok(CloudStatus {
        configured: true,
        logged_in: true,
        base_url,
        user: Some(CloudUser {
            id: session.user_id,
            email: session.email,
            name: session.name,
            verified: session.verified,
        }),
        current_workspace,
        workspaces,
        billing_enabled: billing.enabled,
        seat_price_cents: billing.seat_price_cents,
        seat_currency: billing.seat_currency,
        chat_addon: billing.chat_addon,
    })
}

/// Start (or resume) a Stripe Checkout for a workspace's team subscription.
/// Returns a hosted Checkout URL the client opens in the browser. Owner-only +
/// billing-config checks happen server-side in the billing hook.
///
/// `source` is the surface that sent the user to checkout (e.g. "onboarding",
/// "settings_organization"). It rides onto the Stripe subscription metadata so a
/// trial start can be attributed — Stripe itself only sees the checkout session.
/// Optional: older clients omit it and the server treats it as absent.
#[tauri::command]
pub async fn cloud_billing_checkout(
    state: State<'_, AppState>,
    workspace_id: String,
    source: Option<String>,
) -> Result<String, String> {
    let (base, session) = ensure_session(&state).await?;
    let mut body = serde_json::json!({ "workspace_id": workspace_id });
    // Only send a non-empty source; the server rejects anything outside
    // /^[a-z0-9_]{1,40}$/ rather than forwarding junk into Stripe metadata.
    if let Some(s) = source.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        body["source"] = serde_json::Value::String(s.to_string());
    }
    let val = authed_post(&base, &session.token, "/api/humla/billing/checkout", body).await?;
    val.get("url")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| "No checkout URL returned.".to_string())
}

/// Open the Stripe Customer Portal for a subscribed workspace (manage/cancel).
/// Returns a hosted portal URL the client opens in the browser.
#[tauri::command]
pub async fn cloud_billing_portal(state: State<'_, AppState>, workspace_id: String) -> Result<String, String> {
    let (base, session) = ensure_session(&state).await?;
    let val = authed_post(
        &base,
        &session.token,
        "/api/humla/billing/portal",
        serde_json::json!({ "workspace_id": workspace_id }),
    )
    .await?;
    val.get("url")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| "No portal URL returned.".to_string())
}

#[tauri::command]
pub fn cloud_configure(state: State<'_, AppState>, base_url: String) -> Result<(), String> {
    let trimmed = base_url.trim().trim_end_matches('/').to_string();
    {
        let conn = state.db.lock();
        db::set_setting(&conn, SETTING_BASE_URL, &trimmed).map_err(err)?;
    }
    *SESSION.lock().unwrap() = None; // a server change invalidates any session
    state.sync.config_changed();
    Ok(())
}

#[tauri::command]
pub async fn cloud_login(
    state: State<'_, AppState>,
    email: String,
    password: String,
) -> Result<CloudStatus, String> {
    let base = read_base_url(&state)
        .ok_or("Cloud isn't configured — set the server URL first.")?;
    let session = login_request(&base, email.trim(), &password).await?;
    write_creds(email.trim(), &password)?;
    *SESSION.lock().unwrap() = Some(session);
    state.sync.config_changed(); // creds now present → start the sync worker
    cloud_status(state).await
}

/// Create a new account on the configured server, then sign in. Public sign-up
/// is enabled server-side (`users.createRule = ""`, humla-cloud migration
/// 1718900100). `emailVisibility` is set so workspace co-members can see the
/// email in the roster.
#[tauri::command]
pub async fn cloud_signup(
    state: State<'_, AppState>,
    email: String,
    password: String,
    name: String,
) -> Result<CloudStatus, String> {
    let base = read_base_url(&state).ok_or("Cloud isn't configured — set the server URL first.")?;
    let email = email.trim();
    let name = name.trim();
    if email.is_empty() || password.is_empty() {
        return Err("Email and password are required.".into());
    }

    let resp = http()
        .post(format!("{base}/api/collections/users/records"))
        .json(&serde_json::json!({
            "email": email,
            "password": password,
            "passwordConfirm": password,
            "name": name,
            "emailVisibility": true,
        }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    let val: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("pocketbase: invalid response ({e})"))?;
    if !status.is_success() {
        // PocketBase puts per-field validation errors under `data` (e.g. email
        // already in use, password too short) — surface the first one, which is
        // far more useful than the generic top-level message.
        let detail = val
            .get("data")
            .and_then(|d| d.as_object())
            .and_then(|o| o.values().find_map(|v| v.get("message").and_then(|m| m.as_str())));
        let msg = detail
            .or_else(|| val.get("message").and_then(|m| m.as_str()))
            .unwrap_or("sign-up failed");
        return Err(msg.to_string());
    }

    // Account created → sign in and persist credentials, mirroring cloud_login.
    let session = login_request(&base, email, &password).await?;
    write_creds(email, &password)?;
    *SESSION.lock().unwrap() = Some(session);
    state.sync.config_changed();
    cloud_status(state).await
}

/// Resend the email-verification message to the signed-in user's address. Uses
/// PocketBase's public `request-verification` endpoint (it no-ops server-side if
/// already verified). Surfaced in the Account tab when the user is unverified.
#[tauri::command]
pub async fn cloud_resend_verification(state: State<'_, AppState>) -> Result<(), String> {
    let (base, session) = ensure_session(&state).await?;
    if session.email.is_empty() {
        return Err("Not signed in.".into());
    }
    let resp = http()
        .post(format!("{base}/api/collections/users/request-verification"))
        .json(&serde_json::json!({ "email": session.email }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if resp.status().is_success() {
        return Ok(()); // 204 No Content
    }
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    Err(format!("couldn't resend verification ({status}): {body}"))
}

/// Ask the server to email a password-reset link. Signed-out flow, so it needs
/// only the configured base URL, not a session. PocketBase answers 204 even
/// for unknown addresses (no account enumeration), so success here only means
/// "request accepted", not "account exists".
#[tauri::command]
pub async fn cloud_request_password_reset(
    state: State<'_, AppState>,
    email: String,
) -> Result<(), String> {
    let base = read_base_url(&state)
        .ok_or("Cloud isn't configured — set the server URL first.")?;
    let resp = http()
        .post(format!("{base}/api/collections/users/request-password-reset"))
        .json(&serde_json::json!({ "email": email.trim() }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if resp.status().is_success() {
        return Ok(()); // 204 No Content
    }
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    Err(format!("couldn't request a password reset ({status}): {body}"))
}

#[tauri::command]
pub fn cloud_logout(state: State<'_, AppState>) -> Result<(), String> {
    clear_creds();
    *SESSION.lock().unwrap() = None;
    state.sync.config_changed(); // creds gone → stop the sync worker
    Ok(())
}

#[tauri::command]
pub async fn cloud_create_workspace(
    state: State<'_, AppState>,
    name: String,
) -> Result<CloudWorkspace, String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("Workspace name cannot be empty".into());
    }
    let (base, session) = ensure_session(&state).await?;
    let body = serde_json::json!({
        "name": name,
        "owner": session.user_id,
        "members": [session.user_id],
        "admins": [session.user_id],
    });
    let val = authed_post(&base, &session.token, "/api/collections/workspaces/records", body).await?;
    let id = val.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    {
        let conn = state.db.lock();
        db::set_setting(&conn, SETTING_WORKSPACE, &id).map_err(err)?;
    }
    state.sync.config_changed(); // new workspace selected → (re)start the worker
    // A freshly created workspace has no subscription yet → no seat count.
    Ok(CloudWorkspace { id, name, role: "owner".into(), plan_status: "none".into(), seats: None })
}

#[tauri::command]
pub fn cloud_select_workspace(state: State<'_, AppState>, id: String) -> Result<(), String> {
    {
        let conn = state.db.lock();
        db::set_setting(&conn, SETTING_WORKSPACE, &id).map_err(err)?;
    }
    // Drop the db guard before notifying — the observer contract forbids
    // re-locking the db while a guard is held. Switching to "" (Personal) makes
    // `read_config` return None, which stops the worker.
    state.sync.config_changed();
    Ok(())
}

#[tauri::command]
pub async fn cloud_workspace_members(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<Vec<CloudMember>, String> {
    let (base, session) = ensure_session(&state).await?;
    let path = format!("/api/collections/workspaces/records/{workspace_id}");
    let val = authed_get(&base, &session.token, &path, &[("expand", "members")]).await?;

    let owner_id = val.get("owner").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let admin_ids = ids(&val, "admins");
    let viewer_ids = ids(&val, "viewers");
    let members = val
        .get("expand")
        .and_then(|e| e.get("members"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    Ok(members
        .iter()
        .map(|m| {
            let id = m.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            let role = if id == owner_id {
                "owner"
            } else if admin_ids.contains(&id) {
                "admin"
            } else if viewer_ids.contains(&id) {
                "viewer"
            } else {
                "member"
            };
            CloudMember {
                email: m.get("email").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                name: m.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                role: role.to_string(),
                id,
            }
        })
        .collect())
}

/// Fetch the workspace's current `members` + `admins` + `viewers` id arrays.
async fn fetch_relations(
    base: &str,
    token: &str,
    workspace_id: &str,
) -> Result<(Vec<String>, Vec<String>, Vec<String>), String> {
    let path = format!("/api/collections/workspaces/records/{workspace_id}");
    let val = authed_get(base, token, &path, &[]).await?;
    Ok((ids(&val, "members"), ids(&val, "admins"), ids(&val, "viewers")))
}

#[tauri::command]
pub async fn cloud_add_member(
    state: State<'_, AppState>,
    workspace_id: String,
    email: String,
) -> Result<(), String> {
    let (base, session) = ensure_session(&state).await?;
    let email = email.trim();

    // Resolve email → user id via the server-side superuser hook, so the client
    // needs no read access to the users collection (the list/view rules are
    // locked down to prevent enumeration — see humla-cloud migration
    // 1718900000_tighten_users). The hook returns 404 when no account exists.
    let found = match authed_post(
        &base,
        &session.token,
        "/api/humla/find-user",
        serde_json::json!({ "email": email }),
    )
    .await
    {
        Ok(v) => v,
        Err(e) if e.contains("404") => {
            return Err(format!(
                "No Humla account found for {email}. Email invites aren't wired up yet — ask them to sign up first."
            ));
        }
        Err(e) => return Err(e),
    };
    let user_id = found
        .get("id")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or("find-user returned no id")?;

    let (mut members, _admins, _viewers) = fetch_relations(&base, &session.token, &workspace_id).await?;
    if !members.contains(&user_id) {
        members.push(user_id);
        let path = format!("/api/collections/workspaces/records/{workspace_id}");
        authed_patch(&base, &session.token, &path, serde_json::json!({ "members": members })).await?;
    }
    Ok(())
}

/// Invite someone to a workspace by email. If they already have an account they
/// are added immediately (returns "added"); otherwise a pending invite is
/// recorded and they auto-join on sign-up (returns "invited"). Owner/admin only
/// (enforced by the hook). Supersedes `cloud_add_member` for the UI.
#[tauri::command]
pub async fn cloud_invite_member(
    state: State<'_, AppState>,
    workspace_id: String,
    email: String,
    role: String,
) -> Result<String, String> {
    let (base, session) = ensure_session(&state).await?;
    let resp = http()
        .post(format!("{base}/api/humla/invite"))
        .bearer_auth(&session.token)
        .json(&serde_json::json!({
            "workspace_id": workspace_id,
            "email": email.trim(),
            "role": role,
        }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let val = pb_json(resp).await?;
    Ok(val.get("status").and_then(|v| v.as_str()).unwrap_or("invited").to_string())
}

#[tauri::command]
pub async fn cloud_remove_member(
    state: State<'_, AppState>,
    workspace_id: String,
    user_id: String,
) -> Result<(), String> {
    let (base, session) = ensure_session(&state).await?;
    let (mut members, mut admins, mut viewers) =
        fetch_relations(&base, &session.token, &workspace_id).await?;
    members.retain(|m| m != &user_id);
    admins.retain(|a| a != &user_id);
    viewers.retain(|v| v != &user_id);
    let path = format!("/api/collections/workspaces/records/{workspace_id}");
    authed_patch(
        &base,
        &session.token,
        &path,
        serde_json::json!({ "members": members, "admins": admins, "viewers": viewers }),
    )
    .await?;
    Ok(())
}

/// Set a member's role to `admin`, `member`, or `viewer` (read-only). Owner is
/// immutable here (transfer ownership separately). Admins and viewers are kept
/// in `members` (which drives read access); the roles are mutually exclusive.
#[tauri::command]
pub async fn cloud_set_member_role(
    state: State<'_, AppState>,
    workspace_id: String,
    user_id: String,
    role: String,
) -> Result<(), String> {
    let (base, session) = ensure_session(&state).await?;
    let (members, mut admins, mut viewers) =
        fetch_relations(&base, &session.token, &workspace_id).await?;
    match role.as_str() {
        "admin" => {
            if !admins.contains(&user_id) {
                admins.push(user_id.clone());
            }
            viewers.retain(|v| v != &user_id);
        }
        "viewer" => {
            if !viewers.contains(&user_id) {
                viewers.push(user_id.clone());
            }
            admins.retain(|a| a != &user_id);
        }
        "member" => {
            admins.retain(|a| a != &user_id);
            viewers.retain(|v| v != &user_id);
        }
        other => return Err(format!("unknown role: {other}")),
    }
    // Admins + viewers must also be members (members drives read access).
    let mut members = members;
    for id in admins.iter().chain(viewers.iter()) {
        if !members.contains(id) {
            members.push(id.clone());
        }
    }
    let path = format!("/api/collections/workspaces/records/{workspace_id}");
    authed_patch(
        &base,
        &session.token,
        &path,
        serde_json::json!({ "members": members, "admins": admins, "viewers": viewers }),
    )
    .await?;
    Ok(())
}

/// Rename a workspace. The server `updateRule` allows owner or admin.
#[tauri::command]
pub async fn cloud_rename_workspace(
    state: State<'_, AppState>,
    workspace_id: String,
    name: String,
) -> Result<(), String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("Workspace name cannot be empty".into());
    }
    let (base, session) = ensure_session(&state).await?;
    let path = format!("/api/collections/workspaces/records/{workspace_id}");
    authed_patch(&base, &session.token, &path, serde_json::json!({ "name": name })).await?;
    Ok(())
}

/// Delete a workspace. The server `deleteRule` allows the owner only; the
/// `workspace` relation cascade-deletes its notes/folders/prompts on the
/// server. Local copies are left in place (they simply stop syncing). If the
/// deleted workspace was the active one, fall back to Personal and stop the
/// sync worker.
#[tauri::command]
pub async fn cloud_delete_workspace(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<(), String> {
    let (base, session) = ensure_session(&state).await?;
    let path = format!("/api/collections/workspaces/records/{workspace_id}");
    authed_delete(&base, &session.token, &path).await?;
    if read_workspace_id(&state).as_deref() == Some(workspace_id.as_str()) {
        {
            let conn = state.db.lock();
            db::set_setting(&conn, SETTING_WORKSPACE, "").map_err(err)?;
        }
        state.sync.config_changed(); // deleted the active workspace → stop syncing
    }
    Ok(())
}

/// Leave a workspace (remove yourself). Goes through the server-side hook
/// `POST /api/humla/leave-workspace` because the workspaces `updateRule` is
/// owner/admin-only — a plain member can't PATCH the workspace to drop their own
/// membership. The hook rejects the owner (who must delete or transfer instead).
/// If you leave the active workspace, fall back to Personal and stop the worker.
#[tauri::command]
pub async fn cloud_leave_workspace(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<(), String> {
    let (base, session) = ensure_session(&state).await?;
    let resp = http()
        .post(format!("{base}/api/humla/leave-workspace"))
        .bearer_auth(&session.token)
        .json(&serde_json::json!({ "workspace_id": workspace_id }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    pb_json(resp).await?; // surfaces the hook's message (e.g. owner-can't-leave)
    if read_workspace_id(&state).as_deref() == Some(workspace_id.as_str()) {
        {
            let conn = state.db.lock();
            db::set_setting(&conn, SETTING_WORKSPACE, "").map_err(err)?;
        }
        state.sync.config_changed(); // left the active workspace → stop syncing
    }
    Ok(())
}

/// Transfer workspace ownership to another member. Goes through the server-side
/// hook `POST /api/humla/transfer-workspace` because `owner` is immutable via
/// the normal API (an admin must not be able to seize it). The hook enforces
/// owner-only, requires the new owner to already be a member, promotes them to
/// admin, and keeps the outgoing owner on as an admin. The active workspace is
/// unchanged (still syncing), but the caller should refresh cloud status since
/// their own role drops from owner → admin.
#[tauri::command]
pub async fn cloud_transfer_workspace(
    state: State<'_, AppState>,
    workspace_id: String,
    new_owner_id: String,
) -> Result<(), String> {
    let (base, session) = ensure_session(&state).await?;
    let resp = http()
        .post(format!("{base}/api/humla/transfer-workspace"))
        .bearer_auth(&session.token)
        .json(&serde_json::json!({ "workspace_id": workspace_id, "new_owner_id": new_owner_id }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    pb_json(resp).await?; // surfaces the hook's message (e.g. owner-only, not-a-member)
    Ok(())
}

// ---- audio sync (paid-tier capability) -------------------------------------

/// Resolve a note's remote record (PB id + current audio filename) by its
/// client_id within a workspace. `None` when it isn't on the server yet.
async fn find_note_remote(
    base: &str,
    token: &str,
    client_id: &str,
    workspace: &str,
) -> Result<Option<(String, String)>, String> {
    let filter = format!("client_id='{client_id}' && workspace='{workspace}'");
    let val = authed_get(
        base,
        token,
        "/api/collections/notes/records",
        &[("filter", filter.as_str()), ("perPage", "1"), ("fields", "id,audio")],
    )
    .await?;
    let item = val.get("items").and_then(|v| v.as_array()).and_then(|a| a.first());
    Ok(item.map(|it| {
        (
            it.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            it.get("audio").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
        )
    }))
}

/// Local path for the note's single synced `playback.wav`.
///
/// Per-session storage (#16) keeps each take in its own subdir, but the cloud
/// contract is still one audio file per note (the per-session sync is a
/// separate follow-up). So the upload source is the *latest* session's
/// playback.wav, and the download target is the same resolved path — which
/// for a note never recorded locally (no manifest) falls back to the flat
/// `recordings/<note_id>/playback.wav`, exactly as before this feature.
fn playback_path(app: &tauri::AppHandle, note_id: &str) -> Result<std::path::PathBuf, String> {
    let app_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let recordings = crate::sessions::recordings_dir(&app_dir, note_id);
    Ok(crate::sessions::latest_session_dir(&recordings).join("playback.wav"))
}

/// Upload a finished recording's mixed `playback.wav` to its workspace note
/// record, so teammates can play it back. No-op for Personal notes or when the
/// `sync_audio` setting is "false". Waits for the post-stop pipeline to write
/// the playback file and for the note record to exist on the server (both happen
/// asynchronously after a recording stops), so the caller can fire-and-forget.
pub(crate) async fn upload_note_audio(app: &tauri::AppHandle, note_id: &str) -> Result<(), String> {
    let state = app.state::<AppState>();
    let workspace = {
        let conn = state.db.lock();
        if db::get_setting(&conn, "sync_audio").ok().flatten().as_deref() == Some("false") {
            return Ok(());
        }
        db::get_note(&conn, note_id).map_err(err)?.workspace_id
    };
    if workspace.is_empty() {
        return Ok(()); // Personal — never leaves the device
    }

    // Wait for write_playback_assets to produce the file (post-stop diarize can
    // take a while on long recordings). Bounded; give up quietly if never ready.
    let path = playback_path(app, note_id)?;
    let mut ready = false;
    for _ in 0..40 {
        if path.exists() {
            ready = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    }
    if !ready {
        return Ok(());
    }
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;

    let (base, session) = ensure_session(&state).await?;
    // The note record syncs asynchronously post-stop; wait for it to exist.
    let mut pb_id = None;
    for _ in 0..20 {
        if let Some((id, _)) = find_note_remote(&base, &session.token, note_id, &workspace).await? {
            pb_id = Some(id);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    }
    let Some(pb_id) = pb_id else {
        return Ok(()); // note never synced — skip rather than error
    };

    let part = reqwest::multipart::Part::bytes(bytes)
        .file_name("playback.wav")
        .mime_str("audio/wav")
        .map_err(|e| e.to_string())?;
    let form = reqwest::multipart::Form::new().part("audio", part);
    let resp = http()
        .patch(format!("{base}/api/collections/notes/records/{pb_id}"))
        .bearer_auth(&session.token)
        .multipart(form)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    pb_json(resp).await?;
    Ok(())
}

/// Download a workspace note's audio to its local playback path so it can be
/// played back. Returns true if the file is present locally afterwards. No-op
/// for Personal notes, when local audio already exists, or when the note has no
/// remote audio. The `audio` field is protected, so it's fetched with a
/// short-lived file token.
pub(crate) async fn download_note_audio(app: &tauri::AppHandle, note_id: &str) -> Result<bool, String> {
    let state = app.state::<AppState>();
    let workspace = {
        let conn = state.db.lock();
        db::get_note(&conn, note_id).map_err(err)?.workspace_id
    };
    if workspace.is_empty() {
        return Ok(false);
    }
    let path = playback_path(app, note_id)?;
    if path.exists() {
        return Ok(true); // already local
    }
    let (base, session) = ensure_session(&state).await?;
    let Some((pb_id, filename)) = find_note_remote(&base, &session.token, note_id, &workspace).await?
    else {
        return Ok(false);
    };
    if filename.is_empty() {
        return Ok(false); // no remote audio
    }
    // `pb_id` and `filename` are server-controlled and go into the request path.
    // The local write target is the fixed playback.wav, so there's no local
    // traversal — but treat them as opaque segments and reject anything that
    // could traverse or rewrite the URL, in case the server is hostile.
    let safe_seg = |s: &str| {
        !s.is_empty() && s.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
    };
    if !safe_seg(&pb_id) || !safe_seg(&filename) || filename.contains("..") {
        return Err("audio download: unexpected server file reference".into());
    }
    // Protected file → mint a short-lived file token, then fetch.
    let tok = authed_post(&base, &session.token, "/api/files/token", serde_json::json!({})).await?;
    let file_token = tok.get("token").and_then(|v| v.as_str()).unwrap_or_default();
    let resp = http()
        .get(format!("{base}/api/files/notes/{pb_id}/{filename}"))
        .query(&[("token", file_token)])
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("audio download failed ({})", resp.status()));
    }
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
    Ok(true)
}

#[tauri::command]
pub async fn cloud_upload_note_audio(app: tauri::AppHandle, note_id: String) -> Result<(), String> {
    upload_note_audio(&app, &note_id).await
}

#[tauri::command]
pub async fn cloud_download_note_audio(app: tauri::AppHandle, note_id: String) -> Result<bool, String> {
    download_note_audio(&app, &note_id).await
}

// ---- per-session asset sync (#16) ------------------------------------------
//
// Session METADATA (the `note_sessions` records) syncs through the cloud-sync
// crate's outbox/pull machinery, pinged from the backend post-stop chain
// (`SyncObserver::session_upserted`). The BINARY ASSETS (playback / timeline /
// mic / sys / chunks) are handled here, mirroring the single-file
// `upload_note_audio` / `download_note_audio` pattern: upload is fire-and-forget
// after a recording (waits for the record to exist, then multipart-PATCHes),
// download is triggered on note-open (reconstructs sessions.json + fetches the
// assets a teammate needs to read/play the note).
//
// The legacy single-file `notes.audio` path above is UNCHANGED and still used,
// so old clients and pre-#16 notes keep playing exactly as before.

/// One remote `note_sessions` record, as needed for asset up/download.
struct RemoteSession {
    pb_id: String,
    client_id: String,
    index: u32,
    started_at_ms: i64,
    duration_ms: u64,
    streams: Vec<String>,
    deleted: bool,
    /// Stored (suffixed) filename per file field; empty when the field is unset.
    files: std::collections::HashMap<&'static str, String>,
}

fn parse_remote_session(it: &serde_json::Value) -> RemoteSession {
    let s = |k: &str| it.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let mut files = std::collections::HashMap::new();
    for f in ["playback", "mic", "sys", "timeline", "chunks"] {
        files.insert(f, it.get(f).and_then(|v| v.as_str()).unwrap_or_default().to_string());
    }
    RemoteSession {
        pb_id: s("id"),
        client_id: s("client_id"),
        index: it.get("session_index").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
        started_at_ms: it.get("started_at").and_then(|v| v.as_i64()).unwrap_or(0),
        duration_ms: it.get("duration_ms").and_then(|v| v.as_u64()).unwrap_or(0),
        streams: it
            .get("streams")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default(),
        deleted: it.get("deleted").and_then(|v| v.as_bool()).unwrap_or(false),
        files,
    }
}

/// List every `note_sessions` record (incl. tombstones) for a note by its PB id.
async fn find_session_records(
    base: &str,
    token: &str,
    note_pb_id: &str,
) -> Result<Vec<RemoteSession>, String> {
    let filter = format!("note='{note_pb_id}'");
    let val = authed_get(
        base,
        token,
        "/api/collections/note_sessions/records",
        &[
            ("filter", filter.as_str()),
            ("perPage", "200"),
            ("sort", "session_index"),
            (
                "fields",
                "id,client_id,session_index,started_at,duration_ms,streams,deleted,playback,mic,sys,timeline,chunks",
            ),
        ],
    )
    .await?;
    Ok(val
        .get("items")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().map(parse_remote_session).collect())
        .unwrap_or_default())
}

/// A server-controlled path segment (PB id or stored filename) is opaque; reject
/// anything that could traverse or rewrite the request URL.
fn safe_path_seg(s: &str) -> bool {
    !s.is_empty()
        && !s.contains("..")
        && s.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

/// Drop any pulled session record whose server-controlled `client_id` isn't a
/// safe path segment. `client_id` is used verbatim as a filesystem path
/// segment (`session_dir` → `remove_dir_all` for tombstones, `create_dir_all` +
/// file writes for live takes) *and* baked into `sessions.json` via
/// `reconcile_manifest`. A malicious workspace member could set it to
/// `../../../../Desktop` so a victim opening the shared note deletes an
/// arbitrary directory. Filtering here — before either the FS ops or the
/// manifest reconstruction see the records — closes both paths at once.
fn retain_safe_sessions(records: Vec<RemoteSession>) -> Vec<RemoteSession> {
    records
        .into_iter()
        .filter(|r| {
            let ok = safe_path_seg(&r.client_id);
            if !ok {
                eprintln!(
                    "cloud: dropping note_session with unsafe client_id {:?}",
                    r.client_id
                );
            }
            ok
        })
        .collect()
}

/// Which asset fields exist on disk for a session dir.
fn local_present_assets(session_dir: &std::path::Path) -> Vec<crate::sessions::AssetField> {
    crate::sessions::AssetField::ALL
        .into_iter()
        .filter(|f| session_dir.join(f.file_name()).exists())
        .collect()
}

/// Which asset fields the remote record already holds a file for.
fn remote_present_assets(rec: &RemoteSession) -> Vec<crate::sessions::AssetField> {
    crate::sessions::AssetField::ALL
        .into_iter()
        .filter(|f| rec.files.get(f.field()).is_some_and(|n| !n.is_empty()))
        .collect()
}

/// Upload a note's per-session assets to the cloud (#16). For each locally
/// recorded session (skipping the synthesized legacy flat entry), attach the
/// assets the server doesn't have yet — plus the timeline every time, since
/// re-diarize / unification rewrites it. No-op for Personal notes or when
/// `sync_audio` is "false". Fire-and-forget: waits (bounded) for the note +
/// session records to sync, then PATCHes.
pub(crate) async fn upload_note_sessions(app: &tauri::AppHandle, note_id: &str) -> Result<(), String> {
    let state = app.state::<AppState>();
    let workspace = {
        let conn = state.db.lock();
        if db::get_setting(&conn, "sync_audio").ok().flatten().as_deref() == Some("false") {
            return Ok(());
        }
        db::get_note(&conn, note_id).map_err(err)?.workspace_id
    };
    if workspace.is_empty() {
        return Ok(()); // Personal — never leaves the device
    }
    let app_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let recordings = crate::sessions::recordings_dir(&app_dir, note_id);
    // Only real (manifest-backed) sessions sync per-session; a legacy flat note
    // keeps using notes.audio until it gains a second take (which migrates it).
    let sessions: Vec<(crate::sessions::SessionEntry, std::path::PathBuf)> =
        crate::sessions::resolve_sessions(&recordings)
            .into_iter()
            .filter(|(e, _)| e.id != crate::sessions::LEGACY_SESSION_ID)
            .collect();
    if sessions.is_empty() {
        return Ok(());
    }

    let (base, session) = ensure_session(&state).await?;
    // The note record syncs asynchronously post-stop; wait for it to exist.
    let mut note_pb = None;
    for _ in 0..20 {
        if let Some((id, _)) = find_note_remote(&base, &session.token, note_id, &workspace).await? {
            note_pb = Some(id);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    }
    let Some(note_pb) = note_pb else {
        return Ok(()); // note never synced — skip rather than error
    };

    // The session records are pushed by the sync worker after our
    // session_upserted ping; wait until they cover every local session.
    let wanted: std::collections::HashSet<String> =
        sessions.iter().map(|(e, _)| e.id.clone()).collect();
    let mut records: Vec<RemoteSession> = Vec::new();
    for _ in 0..20 {
        records = find_session_records(&base, &session.token, &note_pb).await?;
        let have: std::collections::HashSet<&str> =
            records.iter().map(|r| r.client_id.as_str()).collect();
        if wanted.iter().all(|w| have.contains(w.as_str())) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    }
    let by_client: std::collections::HashMap<&str, &RemoteSession> =
        records.iter().map(|r| (r.client_id.as_str(), r)).collect();

    for (entry, dir) in &sessions {
        let Some(rec) = by_client.get(entry.id.as_str()) else {
            continue; // its metadata hasn't synced yet — try again next time
        };
        let plan = crate::sessions::session_upload_plan(
            &local_present_assets(dir),
            &remote_present_assets(rec),
        );
        if plan.is_empty() {
            continue;
        }
        let mut form = reqwest::multipart::Form::new();
        let mut any = false;
        for field in plan {
            let path = dir.join(field.file_name());
            let Ok(bytes) = std::fs::read(&path) else { continue };
            let part = reqwest::multipart::Part::bytes(bytes)
                .file_name(field.file_name())
                .mime_str(field.mime())
                .map_err(|e| e.to_string())?;
            form = form.part(field.field(), part);
            any = true;
        }
        if !any {
            continue;
        }
        let resp = http()
            .patch(format!("{base}/api/collections/note_sessions/records/{}", rec.pb_id))
            .bearer_auth(&session.token)
            .multipart(form)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        pb_json(resp).await?;
    }
    Ok(())
}

/// Download a shared note's per-session assets and rebuild `sessions.json` (#16).
/// Reconstructs the local manifest from the remote `note_sessions` records
/// (honouring tombstones), then fetches each live session's `playback.wav` +
/// `timeline.jsonl` — the assets the reader/player/carousel need — via the
/// protected file-token flow. Heavy raw `mic`/`sys`/`chunks` are left on the
/// server (fetched only if a future re-diarize needs them). Returns true when
/// at least one session was reconstructed. No-op for Personal notes.
pub(crate) async fn download_note_sessions(app: &tauri::AppHandle, note_id: &str) -> Result<bool, String> {
    let state = app.state::<AppState>();
    let workspace = {
        let conn = state.db.lock();
        db::get_note(&conn, note_id).map_err(err)?.workspace_id
    };
    if workspace.is_empty() {
        return Ok(false);
    }
    let (base, session) = ensure_session(&state).await?;
    let Some((note_pb, _)) = find_note_remote(&base, &session.token, note_id, &workspace).await?
    else {
        return Ok(false);
    };
    let records = find_session_records(&base, &session.token, &note_pb).await?;
    // Trust boundary: `client_id` is server-controlled but becomes a local path
    // segment + a persisted manifest id below. Drop hostile ids before anything
    // touches the filesystem or `reconcile_manifest`.
    let records = retain_safe_sessions(records);
    if records.is_empty() {
        return Ok(false); // no per-session data — caller falls back to notes.audio
    }

    let app_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let recordings = crate::sessions::recordings_dir(&app_dir, note_id);

    // Reconstruct the manifest from the remote metadata (tombstones drop entries,
    // local-only takes are preserved). Skip the synthesized legacy pseudo-entry
    // so a flat local note doesn't leak `__legacy__` into the written manifest.
    let remote_meta: Vec<crate::sessions::RemoteSessionMeta> = records
        .iter()
        .map(|r| crate::sessions::RemoteSessionMeta {
            client_id: r.client_id.clone(),
            index: r.index,
            started_at_ms: r.started_at_ms,
            duration_ms: r.duration_ms,
            streams: r.streams.clone(),
            deleted: r.deleted,
        })
        .collect();
    {
        // Serialize the read-modify-write against the post-stop chain
        // (migrate/append) so a finalize landing mid-pull isn't lost. The guard
        // scopes to just the reconcile → write, not the asset downloads below.
        let manifest_lock = state.manifest_lock.clone();
        let _manifest_guard = manifest_lock.lock().await;
        let existing = crate::sessions::read_manifest(&recordings);
        let manifest = crate::sessions::reconcile_manifest(existing, &remote_meta);
        crate::sessions::write_manifest(&recordings, &manifest).map_err(|e| e.to_string())?;
    }

    // Mint one protected-file token and fetch each live session's core assets.
    let tok = authed_post(&base, &session.token, "/api/files/token", serde_json::json!({})).await?;
    let file_token = tok.get("token").and_then(|v| v.as_str()).unwrap_or_default().to_string();

    for rec in &records {
        let dir = crate::sessions::session_dir(&recordings, &rec.client_id);
        if rec.deleted {
            let _ = std::fs::remove_dir_all(&dir); // honour the tombstone locally
            continue;
        }
        if !safe_path_seg(&rec.pb_id) {
            continue;
        }
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        for field in [crate::sessions::AssetField::Playback, crate::sessions::AssetField::Timeline] {
            let filename = rec.files.get(field.field()).cloned().unwrap_or_default();
            if filename.is_empty() || !safe_path_seg(&filename) {
                continue;
            }
            let dest = dir.join(field.file_name());
            if dest.exists() {
                continue; // already local
            }
            let url = format!("{base}/api/files/note_sessions/{}/{}", rec.pb_id, filename);
            let resp = http()
                .get(url)
                .query(&[("token", file_token.as_str())])
                .send()
                .await
                .map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                eprintln!("session asset download failed ({}) for {}", resp.status(), field.field());
                continue;
            }
            let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
            std::fs::write(&dest, &bytes).map_err(|e| e.to_string())?;
        }
    }
    Ok(true)
}

#[tauri::command]
pub async fn cloud_upload_note_sessions(app: tauri::AppHandle, note_id: String) -> Result<(), String> {
    upload_note_sessions(&app, &note_id).await
}

#[tauri::command]
pub async fn cloud_download_note_sessions(app: tauri::AppHandle, note_id: String) -> Result<bool, String> {
    download_note_sessions(&app, &note_id).await
}

// ---- recording lock (shared-note mutual exclusion) -------------------------
//
// Two teammates recording the same workspace note at once would clobber each
// other: each pushes a whole-note last-write-wins record carrying only its own
// transcript, so one stream wins and the other is scattered into conflict
// copies. We prevent it server-side with a `note_locks` collection whose `note`
// field carries a UNIQUE index — the atomic mutex. The first claimant's INSERT
// wins; a concurrent second INSERT is rejected by the index. A crashed
// recorder's lock is reaped via the delete rule (`expires < @now`), so nothing
// stays locked forever. See `docs/cloud/note-locks.md` for the exact schema.
//
// Strength vs. cost: the claim is decided by the server at INSERT time, not
// optimistically per-client and reconciled later. The only soft edge is that an
// unreachable server degrades to recording UNLOCKED (a flaky network must never
// block capture) — but when the server is unreachable, sync is down anyway.

/// Seconds a lock stays valid without a heartbeat. Long enough to ride out a
/// brief network stall, short enough that a crashed recorder frees the note
/// quickly.
const LOCK_TTL_SECS: i64 = 45;
/// Heartbeat cadence — comfortably under `LOCK_TTL_SECS` so two missed pings in
/// a row still don't expire a live lock.
const LOCK_HEARTBEAT_SECS: u64 = 15;

/// The teammate currently recording a note. Returned to the UI so it can show
/// "X is recording…" and disable Record. `holder_id` lets the client tell when
/// the lock is its own (during local recording) and skip the banner.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RecordingLockStatus {
    pub holder_id: String,
    pub holder_name: String,
}

/// Outcome of a claim attempt. `Skipped` = no lock was needed (Personal note)
/// or the cloud was unreachable → record unlocked.
pub(crate) enum LockClaim {
    Granted(String),
    Held(String),
    Skipped,
}

enum LockError {
    /// The unique index rejected the create — the note is already locked.
    Conflict,
    Other(String),
}

struct ExistingLock {
    id: String,
    holder_id: String,
    holder_name: String,
}

/// PocketBase's canonical datetime format, in UTC (e.g. `2026-06-20 12:34:56.789Z`).
fn lock_expiry_value() -> String {
    let at = chrono::Utc::now() + chrono::Duration::seconds(LOCK_TTL_SECS);
    at.format("%Y-%m-%d %H:%M:%S%.3fZ").to_string()
}

fn display_name(s: &Session) -> String {
    if !s.name.trim().is_empty() {
        s.name.clone()
    } else if let Some(local) = s.email.split('@').next().filter(|p| !p.is_empty()) {
        local.to_string()
    } else {
        "a teammate".to_string()
    }
}

/// POST a fresh lock. `Err(Conflict)` means the unique index rejected it (note
/// already locked); other non-2xx → `Err(Other)`.
async fn create_lock(
    base: &str,
    token: &str,
    note: &str,
    workspace: &str,
    holder_id: &str,
    holder_name: &str,
) -> Result<String, LockError> {
    let body = serde_json::json!({
        "note": note,
        "workspace": workspace,
        "holder": holder_id,
        "holder_name": holder_name,
        "expires": lock_expiry_value(),
    });
    let resp = http()
        .post(format!("{base}/api/collections/note_locks/records"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .map_err(|e| LockError::Other(e.to_string()))?;
    let status = resp.status();
    if status.is_success() {
        let v: serde_json::Value =
            resp.json().await.map_err(|e| LockError::Other(e.to_string()))?;
        return Ok(v.get("id").and_then(|x| x.as_str()).unwrap_or_default().to_string());
    }
    let text = resp.text().await.unwrap_or_default();
    // A uniqueness violation on `note` is PocketBase's `validation_not_unique`.
    // That — and only that — is a real "someone else holds it" conflict; any
    // other 4xx (e.g. a createRule rejection for a non-member) falls through to
    // Other and degrades to recording unlocked rather than falsely blocking.
    if status == reqwest::StatusCode::BAD_REQUEST && text.contains("validation_not_unique") {
        return Err(LockError::Conflict);
    }
    Err(LockError::Other(format!("note_locks create {status}: {text}")))
}

/// Fetch the current lock for a note (regardless of expiry), or `None`.
async fn get_lock(base: &str, token: &str, note: &str) -> Result<Option<ExistingLock>, String> {
    let filter = format!("note='{note}'");
    let v = authed_get(
        base,
        token,
        "/api/collections/note_locks/records",
        &[("filter", filter.as_str()), ("perPage", "1"), ("fields", "id,holder,holder_name")],
    )
    .await?;
    let item = v.get("items").and_then(|a| a.as_array()).and_then(|a| a.first());
    Ok(item.map(|it| ExistingLock {
        id: it.get("id").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
        holder_id: it.get("holder").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
        holder_name: it.get("holder_name").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
    }))
}

/// Try to claim the recording lock for `note_id`. Never errors: any cloud
/// failure resolves to `Skipped` so a flaky network can't stop a recording.
pub(crate) async fn claim_recording_lock(app: &tauri::AppHandle, note_id: &str) -> LockClaim {
    let state = app.state::<AppState>();
    let workspace = {
        let conn = state.db.lock();
        db::get_note(&conn, note_id).map(|n| n.workspace_id).unwrap_or_default()
    };
    if workspace.is_empty() {
        return LockClaim::Skipped; // Personal note — no coordination needed
    }
    if read_base_url(&state).is_none() {
        return LockClaim::Skipped; // cloud not configured
    }
    match try_claim(&state, note_id, &workspace).await {
        Ok(outcome) => outcome,
        Err(e) => {
            eprintln!("recording lock: claim skipped ({e})");
            LockClaim::Skipped
        }
    }
}

async fn try_claim(
    state: &State<'_, AppState>,
    note_id: &str,
    workspace: &str,
) -> Result<LockClaim, String> {
    let (base, session) = ensure_session(state).await?;
    let holder_name = display_name(&session);
    match create_lock(&base, &session.token, note_id, workspace, &session.user_id, &holder_name)
        .await
    {
        Ok(id) => return Ok(LockClaim::Granted(id)),
        Err(LockError::Conflict) => {}
        Err(LockError::Other(e)) => return Err(e),
    }
    // Locked. Reuse our own (a leftover from a failed start), else best-effort
    // reap a stale one and retry once. The server's delete rule (`expires <
    // @now`) is what makes the reap safe: a still-valid lock won't delete, so
    // the retry conflicts again and we report the real holder.
    let Some(existing) = get_lock(&base, &session.token, note_id).await? else {
        // Vanished between the conflict and the lookup — race; one clean retry.
        return match create_lock(
            &base, &session.token, note_id, workspace, &session.user_id, &holder_name,
        )
        .await
        {
            Ok(id) => Ok(LockClaim::Granted(id)),
            Err(LockError::Conflict) => Ok(LockClaim::Held("a teammate".to_string())),
            Err(LockError::Other(e)) => Err(e),
        };
    };
    if existing.holder_id == session.user_id {
        return Ok(LockClaim::Granted(existing.id)); // already ours → reuse
    }
    let _ = authed_delete(
        &base,
        &session.token,
        &format!("/api/collections/note_locks/records/{}", existing.id),
    )
    .await;
    match create_lock(&base, &session.token, note_id, workspace, &session.user_id, &holder_name)
        .await
    {
        Ok(id) => Ok(LockClaim::Granted(id)),
        Err(LockError::Conflict) => Ok(LockClaim::Held(existing.holder_name)),
        Err(LockError::Other(e)) => Err(e),
    }
}

/// Release a held lock on stop. Best-effort: a missed delete just lets the lock
/// expire on its own.
pub(crate) async fn release_recording_lock(app: &tauri::AppHandle, lock_id: String) {
    let state = app.state::<AppState>();
    if let Ok((base, session)) = ensure_session(&state).await {
        let _ = authed_delete(
            &base,
            &session.token,
            &format!("/api/collections/note_locks/records/{lock_id}"),
        )
        .await;
    }
}

/// Keep a held lock alive by extending `expires` on a cadence. Runs until
/// aborted (recording_stop / crash cleanup own the handle).
pub(crate) fn spawn_lock_heartbeat(
    app: tauri::AppHandle,
    lock_id: String,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick =
            tokio::time::interval(std::time::Duration::from_secs(LOCK_HEARTBEAT_SECS));
        tick.tick().await; // the first tick fires immediately; consume it
        loop {
            tick.tick().await;
            let state = app.state::<AppState>();
            let Ok((base, session)) = ensure_session(&state).await else { continue };
            let body = serde_json::json!({ "expires": lock_expiry_value() });
            let _ = authed_patch(
                &base,
                &session.token,
                &format!("/api/collections/note_locks/records/{lock_id}"),
                body,
            )
            .await;
        }
    })
}

/// Who, if anyone, is currently recording `note_id`. Returns `None` for
/// Personal notes, an unconfigured cloud, or no live lock. The `expires > @now`
/// filter makes the server drop stale locks, so the client never trusts its own
/// clock. Drives the Note view's "X is recording…" banner + disabled Record.
#[tauri::command]
pub async fn cloud_note_recording_status(
    app: tauri::AppHandle,
    note_id: String,
) -> Result<Option<RecordingLockStatus>, String> {
    let state = app.state::<AppState>();
    let workspace = {
        let conn = state.db.lock();
        db::get_note(&conn, &note_id).map(|n| n.workspace_id).unwrap_or_default()
    };
    if workspace.is_empty() || read_base_url(&state).is_none() {
        return Ok(None);
    }
    let (base, session) = ensure_session(&state).await?;
    let filter = format!("note='{note_id}' && expires > @now");
    let v = authed_get(
        &base,
        &session.token,
        "/api/collections/note_locks/records",
        &[("filter", filter.as_str()), ("perPage", "1"), ("fields", "holder,holder_name")],
    )
    .await?;
    let item = v.get("items").and_then(|a| a.as_array()).and_then(|a| a.first());
    Ok(item.map(|it| RecordingLockStatus {
        holder_id: it.get("holder").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
        holder_name: it.get("holder_name").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
    }))
}

/// Note ids with an unpushed change queued in the sync outbox — drives a
/// per-note "syncing…" indicator. Returns empty when cloud sync isn't running
/// (the `sync_outbox` table won't exist), so the UI just shows nothing.
#[tauri::command]
pub fn cloud_pending_note_ids(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    let conn = state.db.lock();
    // A note is "pending" if it has a queued note push OR a queued per-session
    // push. Session rows pack their entity_id as `<note_id>/<session_id>`, so
    // the note id is the part before the first '/'. Collapsing both keeps the
    // per-note "syncing…" indicator accurate for session-asset uploads too.
    let mut ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Ok(mut stmt) =
        conn.prepare("SELECT DISTINCT entity, entity_id FROM sync_outbox WHERE entity IN ('note','session')")
    {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        }) {
            for (entity, entity_id) in rows.flatten() {
                let note_id = if entity == "session" {
                    entity_id.split('/').next().unwrap_or("").to_string()
                } else {
                    entity_id
                };
                if !note_id.is_empty() {
                    ids.insert(note_id);
                }
            }
        }
    }
    Ok(ids.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_chat_addon_maps_available_absent_and_malformed() {
        // Available, fully specified.
        assert_eq!(
            parse_chat_addon(&serde_json::json!({
                "chat_addon": { "available": true, "price_id": "price_1", "price_cents": 900, "currency": "usd" }
            })),
            Some(ChatAddon {
                available: true,
                price_id: Some("price_1".into()),
                price_cents: Some(900),
                currency: Some("usd".into()),
            })
        );
        // Key absent (self-host / not configured) → None (pitch dropped).
        assert_eq!(parse_chat_addon(&serde_json::json!({ "enabled": true })), None);
        // Present but not available → Some with available:false (no pitch).
        assert_eq!(
            parse_chat_addon(&serde_json::json!({ "chat_addon": { "available": false } })),
            Some(ChatAddon::default())
        );
        // Malformed price / currency → dropped, still parses.
        let m = parse_chat_addon(&serde_json::json!({
            "chat_addon": { "available": true, "price_cents": -1, "currency": "" }
        }))
        .unwrap();
        assert!(m.available && m.price_cents.is_none() && m.currency.is_none());
    }

    /// A 404 is the "route absent on an older/self-hosted server" signal the
    /// chat callers branch on (legacy session fallback / "needs a newer
    /// server"), so it must be distinguishable from every other failure.
    #[test]
    fn only_a_404_status_reads_as_not_found() {
        assert!(CloudReqError::Status {
            status: reqwest::StatusCode::NOT_FOUND,
            reason: String::new(),
            server_message: String::new(),
        }
        .is_not_found());
        assert!(!CloudReqError::Status {
            status: reqwest::StatusCode::PAYMENT_REQUIRED,
            reason: "cap_reached".into(),
            server_message: String::new(),
        }
        .is_not_found());
        // A network / not-signed-in failure is never a missing route.
        assert!(!CloudReqError::Unreachable("Not signed in.".into()).is_not_found());
    }

    /// Rendering routes by variant: a transport/session failure is already a
    /// user message and passes through verbatim, while a server status hands its
    /// `{reason, error}` pair to the caller's own taxonomy.
    #[test]
    fn message_passes_transport_through_and_maps_server_reasons() {
        fn map(reason: &str, server_message: &str) -> String {
            format!("mapped:{reason}/{server_message}")
        }
        assert_eq!(
            CloudReqError::Unreachable("Couldn't reach Team chat: dns error".into()).message(map),
            "Couldn't reach Team chat: dns error"
        );
        assert_eq!(
            CloudReqError::Status {
                status: reqwest::StatusCode::FORBIDDEN,
                reason: "not_owner".into(),
                server_message: "only the owner may".into(),
            }
            .message(map),
            "mapped:not_owner/only the owner may"
        );
        // An empty body still maps — the taxonomy owns the fallback wording.
        assert_eq!(
            CloudReqError::Status {
                status: reqwest::StatusCode::INTERNAL_SERVER_ERROR,
                reason: String::new(),
                server_message: String::new(),
            }
            .message(map),
            "mapped:/"
        );
    }

    /// Minimal `RemoteSession` builder — only `client_id` matters for the
    /// path-traversal guard; the rest are placeholders.
    fn rec(client_id: &str) -> RemoteSession {
        RemoteSession {
            pb_id: "pbid000000000000".to_string(),
            client_id: client_id.to_string(),
            index: 1,
            started_at_ms: 0,
            duration_ms: 0,
            streams: Vec::new(),
            deleted: false,
            files: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn retain_drops_traversal_client_id() {
        // The confirmed exploit payload: a `../…` client_id aimed at wiping an
        // arbitrary dir via the tombstone branch's `remove_dir_all`.
        let out = retain_safe_sessions(vec![rec("../../../../Desktop")]);
        assert!(out.is_empty(), "traversal client_id must be dropped entirely");
    }

    #[test]
    fn retain_drops_absolute_and_separator_ids() {
        let hostile = vec![
            rec("/etc"),
            rec("a/b"),
            rec("..\\..\\windows"),
            rec(".."),
            rec(""),
            rec("has space"),
        ];
        let n = hostile.len();
        let out = retain_safe_sessions(hostile);
        assert!(out.is_empty(), "all {n} hostile ids must be dropped");
    }

    #[test]
    fn retain_keeps_valid_uuid_and_legacy() {
        let good = vec![
            rec("550e8400-e29b-41d4-a716-446655440000"),
            rec(crate::sessions::LEGACY_SESSION_ID), // "__legacy__"
            rec("session-1_take.2"), // dots/dashes/underscores are fine as a segment
        ];
        let out = retain_safe_sessions(good);
        assert_eq!(out.len(), 3, "valid UUID + __legacy__ + plain segment must pass");
        assert_eq!(out[1].client_id, "__legacy__");
    }

    #[test]
    fn retain_filters_mixed_batch_only_safe_survive() {
        let batch = vec![
            rec("550e8400-e29b-41d4-a716-446655440000"),
            rec("../../../../Desktop"),
            rec("__legacy__"),
        ];
        let out = retain_safe_sessions(batch);
        let ids: Vec<&str> = out.iter().map(|r| r.client_id.as_str()).collect();
        assert_eq!(ids, ["550e8400-e29b-41d4-a716-446655440000", "__legacy__"]);
        assert!(
            !ids.iter().any(|id| id.contains("..")),
            "no traversal id may survive into the reconcile input"
        );
    }
}
