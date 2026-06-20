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
use tauri::State;

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
    let val: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("pocketbase: invalid response ({e})"))?;
    if !status.is_success() {
        let msg = val.get("message").and_then(|m| m.as_str()).unwrap_or("request failed");
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
    "member".into()
}

async fn list_workspaces_inner(base: &str, session: &Session) -> Result<Vec<CloudWorkspace>, String> {
    let filter = format!("members.id ?= '{}'", session.user_id);
    let val = authed_get(
        base,
        &session.token,
        "/api/collections/workspaces/records",
        &[("filter", filter.as_str()), ("perPage", "200"), ("sort", "name")],
    )
    .await?;
    let items = val.get("items").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    Ok(items
        .iter()
        .map(|it| CloudWorkspace {
            id: it.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            name: it.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
            role: derive_role(it, &session.user_id),
        })
        .collect())
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
        });
    };

    let workspaces = list_workspaces_inner(&base_url, &session).await.unwrap_or_default();
    let current_id = read_workspace_id(&state);
    let current_workspace = current_id
        .as_ref()
        .and_then(|id| workspaces.iter().find(|w| &w.id == id).cloned());

    Ok(CloudStatus {
        configured: true,
        logged_in: true,
        base_url,
        user: Some(CloudUser { id: session.user_id, email: session.email, name: session.name }),
        current_workspace,
        workspaces,
    })
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
    Ok(CloudWorkspace { id, name, role: "owner".into() })
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

/// Fetch the workspace's current `members` + `admins` id arrays.
async fn fetch_relations(
    base: &str,
    token: &str,
    workspace_id: &str,
) -> Result<(Vec<String>, Vec<String>), String> {
    let path = format!("/api/collections/workspaces/records/{workspace_id}");
    let val = authed_get(base, token, &path, &[]).await?;
    Ok((ids(&val, "members"), ids(&val, "admins")))
}

#[tauri::command]
pub async fn cloud_add_member(
    state: State<'_, AppState>,
    workspace_id: String,
    email: String,
) -> Result<(), String> {
    let (base, session) = ensure_session(&state).await?;
    let email = email.trim();

    // Look up the user by email (requires the relaxed users list rule).
    let filter = format!("email='{email}'");
    let found = authed_get(
        &base,
        &session.token,
        "/api/collections/users/records",
        &[("filter", filter.as_str()), ("perPage", "1")],
    )
    .await?;
    let user_id = found
        .get("items")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|u| u.get("id"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let Some(user_id) = user_id else {
        return Err(format!(
            "No Humla account found for {email}. Email invites aren't wired up yet — ask them to sign up first."
        ));
    };

    let (mut members, _admins) = fetch_relations(&base, &session.token, &workspace_id).await?;
    if !members.contains(&user_id) {
        members.push(user_id);
        let path = format!("/api/collections/workspaces/records/{workspace_id}");
        authed_patch(&base, &session.token, &path, serde_json::json!({ "members": members })).await?;
    }
    Ok(())
}

#[tauri::command]
pub async fn cloud_remove_member(
    state: State<'_, AppState>,
    workspace_id: String,
    user_id: String,
) -> Result<(), String> {
    let (base, session) = ensure_session(&state).await?;
    let (mut members, mut admins) = fetch_relations(&base, &session.token, &workspace_id).await?;
    members.retain(|m| m != &user_id);
    admins.retain(|a| a != &user_id);
    let path = format!("/api/collections/workspaces/records/{workspace_id}");
    authed_patch(
        &base,
        &session.token,
        &path,
        serde_json::json!({ "members": members, "admins": admins }),
    )
    .await?;
    Ok(())
}

/// Set a member's role to `admin` or `member` (owner is immutable here).
#[tauri::command]
pub async fn cloud_set_member_role(
    state: State<'_, AppState>,
    workspace_id: String,
    user_id: String,
    role: String,
) -> Result<(), String> {
    let (base, session) = ensure_session(&state).await?;
    let (members, mut admins) = fetch_relations(&base, &session.token, &workspace_id).await?;
    match role.as_str() {
        "admin" => {
            if !admins.contains(&user_id) {
                admins.push(user_id);
            }
        }
        "member" => admins.retain(|a| a != &user_id),
        other => return Err(format!("unknown role: {other}")),
    }
    // Ensure an admin is also a member.
    let members = if admins.iter().all(|a| members.contains(a)) {
        members
    } else {
        let mut m = members;
        for a in &admins {
            if !m.contains(a) {
                m.push(a.clone());
            }
        }
        m
    };
    let path = format!("/api/collections/workspaces/records/{workspace_id}");
    authed_patch(
        &base,
        &session.token,
        &path,
        serde_json::json!({ "members": members, "admins": admins }),
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
