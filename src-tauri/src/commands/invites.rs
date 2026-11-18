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

/// The most often the list is fetched, unless the account changed, its email
/// was just verified, or an invite was answered here.
const INTERVAL: Duration = Duration::from_secs(60);

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
    /// The organisation's policies, which apply here once joined. `None` from a
    /// server that doesn't send them.
    #[serde(default, deserialize_with = "lenient")]
    pub policy: Option<InvitePolicy>,
    #[serde(default)]
    pub policy_reason: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct InvitePolicy {
    #[serde(default)]
    pub local_only: bool,
    /// Days before notes are deleted; 0 is off.
    #[serde(default)]
    pub retention_days: u32,
    #[serde(default)]
    pub telemetry_off: bool,
    #[serde(default)]
    pub audio_forbidden: bool,
}

/// A policy that doesn't read is left out rather than losing the invite.
/// PocketBase numbers may arrive as `30.0`.
fn lenient<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<InvitePolicy>, D::Error> {
    let value = Option::<serde_json::Value>::deserialize(d)?;
    let Some(serde_json::Value::Object(mut obj)) = value else {
        return Ok(None);
    };
    if let Some(days) = obj.get("retention_days").and_then(|v| v.as_f64()) {
        let days = if days.is_finite() && days > 0.0 { days.min(u32::MAX as f64).round() as u32 } else { 0 };
        obj.insert("retention_days".into(), days.into());
    }
    Ok(serde_json::from_value(serde_json::Value::Object(obj)).ok())
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

/// The pending invites last heard from the server, and whose they are.
static STORE: Mutex<Store> = Mutex::new(Store::new());

struct Store {
    user: String,
    verified: bool,
    /// Bumped by every local change, so a fetch that started before one can't
    /// undo it.
    generation: u64,
    list: Vec<OrgInvite>,
    last_fetch: Option<Instant>,
}

/// Who a fetch is for, and the state it started from.
#[derive(Debug)]
struct Ticket {
    user: String,
    generation: u64,
}

impl Store {
    const fn new() -> Self {
        Self { user: String::new(), verified: false, generation: 0, list: Vec::new(), last_fetch: None }
    }

    fn due(&self, user: &str, verified: bool, now: Instant) -> bool {
        self.user != user || self.verified != verified || self.last_fetch.is_none_or(|t| now.duration_since(t) >= INTERVAL)
    }

    /// Start a fetch for `user`. Another account's list goes first; returns
    /// whether that dropped anything.
    fn begin(&mut self, user: &str, verified: bool, now: Instant) -> (Ticket, bool) {
        let mut dropped = false;
        if self.user != user {
            dropped = !self.list.is_empty();
            self.user = user.to_string();
            self.list.clear();
            self.generation += 1;
        }
        self.verified = verified;
        self.last_fetch = Some(now);
        (Ticket { user: user.to_string(), generation: self.generation }, dropped)
    }

    /// Take a fetch's list, unless the account or the list changed since it
    /// started. Returns whether the list changed.
    fn apply(&mut self, ticket: &Ticket, list: Vec<OrgInvite>) -> bool {
        if ticket.user != self.user || ticket.generation != self.generation || self.list == list {
            return false;
        }
        self.list = list;
        true
    }

    fn forget(&mut self, invite_id: &str) -> bool {
        self.generation += 1;
        self.last_fetch = None;
        let before = self.list.len();
        self.list.retain(|i| i.id != invite_id);
        self.list.len() != before
    }

    fn clear(&mut self) {
        let generation = self.generation + 1;
        *self = Self::new();
        self.generation = generation;
    }

    fn list_for(&self, user: &str) -> Vec<OrgInvite> {
        if self.user == user {
            self.list.clone()
        } else {
            Vec::new()
        }
    }
}

/// The list a reply to `GET /api/humla/invites` gives, or `None` when it says
/// nothing about it and the last list stands. 404 is a server without invites.
pub(crate) fn parse_list(status: u16, body: &str) -> Option<Vec<OrgInvite>> {
    match status {
        200..=299 => serde_json::from_str::<InviteList>(body).ok().map(|l| l.invites),
        404 => Some(Vec::new()),
        _ => None,
    }
}

/// Forget the list on every change of account: it belongs to the account.
pub(crate) fn clear() {
    STORE.lock().clear();
}

fn announce(app: &tauri::AppHandle, list: Vec<OrgInvite>) {
    let _ = app.emit(CHANGED_EVENT, list);
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

/// Ask the server for `session`'s pending invites when due, announce a change,
/// and return the list now known for that account. A failed request keeps the
/// last one.
pub(crate) async fn refresh(app: &tauri::AppHandle, base: &str, session: &super::cloud_auth::Session) -> Vec<OrgInvite> {
    let user = session.user_id.as_str();
    let ticket = {
        let mut store = STORE.lock();
        if !store.due(user, session.verified, Instant::now()) {
            return store.list_for(user);
        }
        let (ticket, dropped) = store.begin(user, session.verified, Instant::now());
        if dropped {
            announce(app, Vec::new());
        }
        ticket
    };
    if let Some(list) = fetch(base, &session.token).await {
        let mut store = STORE.lock();
        if store.apply(&ticket, list) {
            let list = store.list.clone();
            drop(store);
            announce(app, list);
        }
    }
    STORE.lock().list_for(user)
}

fn forget(app: &tauri::AppHandle, invite_id: &str) {
    let mut store = STORE.lock();
    if store.forget(invite_id) {
        let list = store.list.clone();
        drop(store);
        announce(app, list);
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
            policy: None,
            policy_reason: String::new(),
        }
    }

    #[test]
    fn an_invite_carries_its_organisations_policy() {
        let body = r#"{"invites":[{"id":"i1","workspace_id":"ws1","policy_reason":"Health data.",
            "policy":{"local_only":true,"retention_days":30.0,"telemetry_off":false,"audio_forbidden":true}}]}"#;
        let list = parse_list(200, body).unwrap();
        assert_eq!(
            list[0].policy,
            Some(InvitePolicy { local_only: true, retention_days: 30, telemetry_off: false, audio_forbidden: true })
        );
        assert_eq!(list[0].policy_reason, "Health data.");
    }

    #[test]
    fn a_policy_that_doesnt_read_keeps_the_invite() {
        let list = parse_list(200, r#"{"invites":[{"id":"i1","workspace_id":"ws1","policy":"strict"}]}"#).unwrap();
        assert_eq!(list[0].policy, None);
        let list = parse_list(200, r#"{"invites":[{"id":"i1","workspace_id":"ws1","policy":null}]}"#).unwrap();
        assert_eq!(list[0].policy, None);
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
    fn a_fetch_takes_effect_only_for_the_account_it_was_for() {
        let now = Instant::now();
        let mut store = Store::new();
        let (a, _) = store.begin("userA", true, now);
        assert!(store.apply(&a, vec![invite("i1")]));
        assert_eq!(store.list_for("userA"), vec![invite("i1")]);
        assert!(store.list_for("userB").is_empty(), "never another account's list");

        // B signs in and starts a fetch that fails: A's invites are gone anyway.
        let (b, dropped) = store.begin("userB", true, now);
        assert!(dropped);
        assert!(store.list_for("userB").is_empty());
        // A late reply for A is ignored.
        assert!(!store.apply(&a, vec![invite("i1")]));
        assert!(store.list_for("userB").is_empty());
        assert!(store.apply(&b, vec![invite("i2")]));
        assert_eq!(store.list_for("userB"), vec![invite("i2")]);
    }

    #[test]
    fn a_fetch_in_flight_across_a_sign_out_restores_nothing() {
        let mut store = Store::new();
        let (a, _) = store.begin("userA", true, Instant::now());
        store.clear();
        assert!(!store.apply(&a, vec![invite("i1")]));
        assert!(store.list_for("userA").is_empty());
    }

    #[test]
    fn an_answered_invite_stays_gone_when_an_older_fetch_lands() {
        let now = Instant::now();
        let mut store = Store::new();
        let (first, _) = store.begin("userA", true, now);
        store.apply(&first, vec![invite("i1"), invite("i2")]);
        let (older, _) = store.begin("userA", true, now);
        assert!(store.forget("i1"));
        assert!(!store.apply(&older, vec![invite("i1"), invite("i2")]));
        assert_eq!(store.list_for("userA"), vec![invite("i2")]);
        assert!(!store.forget("i1"));
    }

    #[test]
    fn fetches_are_paced_unless_something_changed() {
        let now = Instant::now();
        let mut store = Store::new();
        assert!(store.due("userA", false, now));
        store.begin("userA", false, now);
        assert!(!store.due("userA", false, now + Duration::from_secs(5)), "window focus doesn't refetch");
        assert!(store.due("userA", false, now + INTERVAL));
        assert!(store.due("userA", true, now), "a just-verified email asks at once");
        assert!(store.due("userB", false, now), "so does another account");
        store.forget("i1");
        assert!(store.due("userA", false, now), "and an answer given here");
        store.begin("userA", false, now);
        store.clear();
        assert!(store.due("userA", false, now), "and a sign-in");
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
