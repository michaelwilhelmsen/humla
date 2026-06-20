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
}

/// Process-lifetime auth cache. Kept out of `AppState` so the whole cloud layer
/// stays self-contained and easy to extract.
static SESSION: LazyLock<Mutex<Option<Session>>> = LazyLock::new(|| Mutex::new(None));

#[derive(Serialize, Clone)]
pub struct CloudUser {
    pub id: String,
    pub email: String,
    pub name: String,
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

    // Per-workspace subscription status. The subscriptions listRule scopes to the
    // user's memberships, so an unfiltered fetch returns exactly their rows.
    // Best-effort: any error just leaves statuses "none" (billing_enabled gates
    // the UI), and self-host servers simply have no rows.
    let mut status_by_ws: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    if let Ok(subs) =
        authed_get(base, &session.token, "/api/collections/subscriptions/records", &[("perPage", "200")]).await
    {
        if let Some(arr) = subs.get("items").and_then(|v| v.as_array()) {
            for it in arr {
                let ws = it.get("workspace").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                let st = it.get("status").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                if !ws.is_empty() {
                    status_by_ws.insert(ws, st);
                }
            }
        }
    }

    Ok(items
        .iter()
        .map(|it| {
            let id = it.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            let plan_status = status_by_ws.get(&id).cloned().unwrap_or_else(|| "none".to_string());
            CloudWorkspace {
                id,
                name: it.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                role: derive_role(it, &session.user_id),
                plan_status,
            }
        })
        .collect())
}

/// Whether the configured server enforces billing (humla-cloud) vs runs free
/// (self-host). Reads the public `/api/humla/billing/config` flag; any failure
/// (older server, network) is treated as "not enabled" so the UI degrades to the
/// free/self-host experience.
async fn fetch_billing_enabled(base: &str) -> bool {
    let Ok(resp) = http().get(format!("{base}/api/humla/billing/config")).send().await else {
        return false;
    };
    resp.json::<serde_json::Value>()
        .await
        .ok()
        .and_then(|v| v.get("enabled").and_then(|b| b.as_bool()))
        .unwrap_or(false)
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
        });
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

    let billing_enabled = fetch_billing_enabled(&base_url).await;
    Ok(CloudStatus {
        configured: true,
        logged_in: true,
        base_url,
        user: Some(CloudUser { id: session.user_id, email: session.email, name: session.name }),
        current_workspace,
        workspaces,
        billing_enabled,
    })
}

/// Start (or resume) a Stripe Checkout for a workspace's team subscription.
/// Returns a hosted Checkout URL the client opens in the browser. Owner-only +
/// billing-config checks happen server-side in the billing hook.
#[tauri::command]
pub async fn cloud_billing_checkout(state: State<'_, AppState>, workspace_id: String) -> Result<String, String> {
    let (base, session) = ensure_session(&state).await?;
    let val = authed_post(
        &base,
        &session.token,
        "/api/humla/billing/checkout",
        serde_json::json!({ "workspace_id": workspace_id }),
    )
    .await?;
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
    Ok(CloudWorkspace { id, name, role: "owner".into(), plan_status: "none".into() })
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

fn playback_path(app: &tauri::AppHandle, note_id: &str) -> Result<std::path::PathBuf, String> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("recordings")
        .join(note_id)
        .join("playback.wav"))
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

/// Note ids with an unpushed change queued in the sync outbox — drives a
/// per-note "syncing…" indicator. Returns empty when cloud sync isn't running
/// (the `sync_outbox` table won't exist), so the UI just shows nothing.
#[tauri::command]
pub fn cloud_pending_note_ids(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    let conn = state.db.lock();
    let ids = conn
        .prepare("SELECT DISTINCT entity_id FROM sync_outbox WHERE entity = 'note'")
        .and_then(|mut stmt| {
            stmt.query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap_or_default();
    Ok(ids)
}
