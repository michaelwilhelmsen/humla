//! Invites to an organisation's departments. Joining one takes the person's
//! consent, since the organisation's policies then apply on their Mac
//! (humla-cloud `docs/organisation-layer.md` §14). A server without these
//! routes answers 404, which reads as no invites.

use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::{Emitter, State};

use super::cloud_auth::{authed_call, CloudError, Verb};
use crate::AppState;

pub(crate) const LIST_PATH: &str = "/api/humla/invites";
const ACCEPT_PATH: &str = "/api/humla/invites/accept";
const DECLINE_PATH: &str = "/api/humla/invites/decline";
const PENDING_PATH: &str = "/api/humla/invites/pending";
const WITHDRAW_PATH: &str = "/api/humla/invites/withdraw";

/// Sent with the whole list whenever it changes.
pub(crate) const CHANGED_EVENT: &str = "org_invites_changed";

/// The sync worker asks at most this often; `cloud_status` (window focus)
/// always asks.
#[cfg_attr(not(feature = "cloud"), allow(dead_code))]
const WORKER_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OrgInvite {
    pub id: String,
    pub workspace_id: String,
    #[serde(default)]
    pub workspace_name: String,
    #[serde(default)]
    pub organisation_name: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub invited_by_name: String,
    #[serde(default)]
    pub created: String,
}

#[derive(Deserialize)]
struct InviteList {
    invites: Vec<OrgInvite>,
}

/// An open invite to a department, as its owner and admins see it: by the
/// email it went to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PendingInvite {
    pub id: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub invited_by_name: String,
    #[serde(default)]
    pub created: String,
}

pub(crate) fn parse_pending(body: &serde_json::Value) -> Vec<PendingInvite> {
    body.get("invites")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default()
}

/// The pending invites last heard from the server.
static INVITES: Mutex<Vec<OrgInvite>> = Mutex::new(Vec::new());
static LAST_FETCH: Mutex<Option<Instant>> = Mutex::new(None);

/// The list a reply to `GET /api/humla/invites` gives, or `None` when it says
/// nothing about it and the last list stands. 404 is a server without invites.
pub(crate) fn parse_list(status: u16, body: &str) -> Option<Vec<OrgInvite>> {
    match status {
        200..=299 => serde_json::from_str::<InviteList>(body).ok().map(|l| l.invites),
        404 => Some(Vec::new()),
        _ => None,
    }
}

/// Make `list` the known one. Returns whether it changed.
fn replace(slot: &Mutex<Vec<OrgInvite>>, list: Vec<OrgInvite>) -> bool {
    let mut current = slot.lock();
    if *current == list {
        return false;
    }
    *current = list;
    true
}

fn remove(slot: &Mutex<Vec<OrgInvite>>, invite_id: &str) -> bool {
    let mut current = slot.lock();
    let before = current.len();
    current.retain(|i| i.id != invite_id);
    current.len() != before
}

pub(crate) fn known() -> Vec<OrgInvite> {
    INVITES.lock().clone()
}

/// Forget the list, on sign-out: it belongs to the account.
pub(crate) fn clear() {
    replace(&INVITES, Vec::new());
}

fn announce(app: &tauri::AppHandle) {
    let _ = app.emit(CHANGED_EVENT, known());
}

async fn fetch(base: &str, token: &str) -> Option<Vec<OrgInvite>> {
    let url = format!("{}{LIST_PATH}", base.trim_end_matches('/'));
    let resp = crate::http::client().get(url).bearer_auth(token).send().await.ok()?;
    let status = resp.status().as_u16();
    if status == 401 {
        super::cloud_auth::forget_session_token(token);
    }
    let body = resp.text().await.ok()?;
    parse_list(status, &body)
}

/// Ask the server for the pending invites, announce a change, and return the
/// list now known. A failed request keeps the last one.
pub(crate) async fn refresh(app: &tauri::AppHandle, base: &str, token: &str) -> Vec<OrgInvite> {
    *LAST_FETCH.lock() = Some(Instant::now());
    if let Some(list) = fetch(base, token).await {
        if replace(&INVITES, list) {
            announce(app);
        }
    }
    known()
}

/// [`refresh`] for the sync worker's round, at most once per [`WORKER_INTERVAL`].
#[cfg_attr(not(feature = "cloud"), allow(dead_code))]
pub(crate) async fn refresh_if_due(app: &tauri::AppHandle, base: &str, token: &str) {
    let due = LAST_FETCH.lock().is_none_or(|t| t.elapsed() >= WORKER_INTERVAL);
    if due {
        refresh(app, base, token).await;
    }
}

fn forget(app: &tauri::AppHandle, invite_id: &str) {
    if remove(&INVITES, invite_id) {
        announce(app);
    }
}

pub(crate) fn invite_body(invite_id: &str) -> serde_json::Value {
    serde_json::json!({ "invite_id": invite_id })
}

/// The invite is no longer on the server: accepted elsewhere, declined,
/// withdrawn, or its department left the organisation.
fn gone(reason: &str) -> bool {
    matches!(reason, "not_found" | "invite_expired")
}

pub(crate) const ACCEPT_TWO_FACTOR: &str =
    "This department requires two-factor authentication. Set it up in Settings → Account, then accept again.";
pub(crate) const INVITE_GONE: &str = "This invite is no longer valid.";

/// A failed accept as the user reads it.
pub(crate) fn accept_error(e: CloudError) -> CloudError {
    if e.reason == "two_factor_required" {
        return CloudError { reason: e.reason, message: ACCEPT_TWO_FACTOR.into() };
    }
    if gone(&e.reason) {
        return CloudError { reason: e.reason, message: INVITE_GONE.into() };
    }
    e
}

/// The workspace an accepted invite joined.
pub(crate) fn accepted_workspace(body: &serde_json::Value) -> Result<String, CloudError> {
    body.get("workspace_id")
        .and_then(|v| v.as_str())
        .filter(|id| !id.is_empty())
        .map(String::from)
        .ok_or_else(|| CloudError::from("The server didn't say which department you joined.".to_string()))
}

/// Join the invite's department, apply its organisation's policies and switch
/// to it. A department that requires two-factor authentication refuses until
/// the user has it, and the invite stays.
#[tauri::command]
pub async fn cloud_accept_invite(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    invite_id: String,
) -> Result<String, CloudError> {
    let val = match authed_call(&state, Verb::Post, ACCEPT_PATH, &invite_body(&invite_id), &[]).await {
        Ok(val) => val,
        Err(e) => {
            if gone(&e.reason) {
                forget(&app, &invite_id);
            }
            return Err(accept_error(e));
        }
    };
    let workspace_id = accepted_workspace(&val)?;
    forget(&app, &invite_id);
    let (base, session) = super::cloud_auth::ensure_session(&state).await?;
    super::org::refresh_org_config(&state.db, &app, &base, &session.token).await;
    super::cloud::cloud_select_workspace(state, workspace_id.clone())?;
    Ok(workspace_id)
}

#[tauri::command]
pub async fn cloud_decline_invite(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    invite_id: String,
) -> Result<(), CloudError> {
    match authed_call(&state, Verb::Post, DECLINE_PATH, &invite_body(&invite_id), &[]).await {
        Ok(_) => {}
        Err(e) if gone(&e.reason) => {}
        Err(e) => return Err(e),
    }
    forget(&app, &invite_id);
    Ok(())
}

/// A department's open invites, for its owner and admins.
#[tauri::command]
pub async fn cloud_pending_invites(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<Vec<PendingInvite>, CloudError> {
    let val = authed_call(&state, Verb::Get, PENDING_PATH, &serde_json::Value::Null, &[("workspace_id", &workspace_id)])
        .await?;
    Ok(parse_pending(&val))
}

/// Withdraw an open invite. One already gone counts as withdrawn.
#[tauri::command]
pub async fn cloud_withdraw_invite(state: State<'_, AppState>, invite_id: String) -> Result<(), CloudError> {
    match authed_call(&state, Verb::Post, WITHDRAW_PATH, &invite_body(&invite_id), &[]).await {
        Ok(_) => Ok(()),
        Err(e) if gone(&e.reason) => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invite(id: &str) -> OrgInvite {
        OrgInvite {
            id: id.into(),
            workspace_id: "ws1".into(),
            workspace_name: "Child welfare".into(),
            organisation_name: "Bergen kommune".into(),
            role: "member".into(),
            invited_by_name: "Kari".into(),
            created: "2026-11-18 10:00:00.000Z".into(),
        }
    }

    #[test]
    fn a_listed_reply_reads_every_invite() {
        let body = r#"{"invites":[{"id":"i1","workspace_id":"ws1","workspace_name":"Child welfare",
            "organisation_name":"Bergen kommune","role":"member","invited_by_name":"Kari",
            "created":"2026-11-18 10:00:00.000Z"}]}"#;
        assert_eq!(parse_list(200, body), Some(vec![invite("i1")]));
    }

    #[test]
    fn missing_optional_fields_default_to_empty() {
        let list = parse_list(200, r#"{"invites":[{"id":"i1","workspace_id":"ws1"}]}"#).unwrap();
        assert_eq!(list[0].organisation_name, "");
        assert_eq!(list[0].invited_by_name, "");
    }

    #[test]
    fn a_server_without_the_route_has_no_invites() {
        assert_eq!(parse_list(404, "404 page not found"), Some(vec![]));
    }

    #[test]
    fn a_failed_or_garbled_reply_keeps_the_last_list() {
        assert_eq!(parse_list(401, r#"{"message":"no"}"#), None);
        assert_eq!(parse_list(500, ""), None);
        assert_eq!(parse_list(200, "not json"), None);
        assert_eq!(parse_list(200, r#"{"other":[]}"#), None);
    }

    #[test]
    fn replace_reports_only_a_change() {
        let slot = Mutex::new(Vec::new());
        assert!(!replace(&slot, vec![]));
        assert!(replace(&slot, vec![invite("i1")]));
        assert!(!replace(&slot, vec![invite("i1")]));
        assert!(replace(&slot, vec![]));
    }

    #[test]
    fn remove_drops_one_invite() {
        let slot = Mutex::new(vec![invite("i1"), invite("i2")]);
        assert!(remove(&slot, "i1"));
        assert!(!remove(&slot, "i1"));
        assert_eq!(slot.lock().iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), ["i2"]);
    }

    #[test]
    fn accept_and_decline_send_the_invite_id() {
        assert_eq!(invite_body("i1"), serde_json::json!({ "invite_id": "i1" }));
    }

    #[test]
    fn an_accept_names_the_workspace_joined() {
        assert_eq!(accepted_workspace(&serde_json::json!({ "workspace_id": "ws1" })).unwrap(), "ws1");
        assert!(accepted_workspace(&serde_json::json!({})).is_err());
        assert!(accepted_workspace(&serde_json::json!({ "workspace_id": "" })).is_err());
    }

    #[test]
    fn an_accept_refused_for_two_factor_points_to_account_settings() {
        let refused = super::super::cloud_auth::route_error(
            ACCEPT_PATH,
            403,
            &serde_json::json!({ "reason": "two_factor_required", "message": "This department requires two-factor authentication." }),
        );
        let shown = accept_error(refused);
        assert_eq!(shown.reason, "two_factor_required");
        assert_eq!(shown.message, ACCEPT_TWO_FACTOR);
        assert!(!gone(&shown.reason), "the invite is kept");
    }

    #[test]
    fn an_accept_of_an_invite_that_is_gone_says_so() {
        for reason in ["not_found", "invite_expired"] {
            let refused = super::super::cloud_auth::route_error(
                ACCEPT_PATH,
                404,
                &serde_json::json!({ "reason": reason, "message": "Invite not found." }),
            );
            let shown = accept_error(refused);
            assert_eq!(shown.reason, reason);
            assert_eq!(shown.message, INVITE_GONE);
        }
    }

    #[test]
    fn pending_invites_read_by_email() {
        let body = serde_json::json!({ "invites": [
            { "id": "i1", "email": "ola@example.no", "role": "member", "invited_by_name": "Kari", "created": "2026-11-18" }
        ]});
        let list = parse_pending(&body);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].email, "ola@example.no");
        assert!(parse_pending(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn an_invite_the_server_no_longer_has_is_gone() {
        assert!(gone("not_found"));
        assert!(gone("invite_expired"));
        assert!(!gone(""));
        assert!(!gone("two_factor_required"));
    }
}
