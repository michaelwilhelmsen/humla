//! Signing in to Humla Cloud, and staying signed in.
//!
//! One session per process, kept across launches: its token lives in the
//! Keychain beside the email and is renewed with `auth-refresh`. The password
//! is only a fallback for when no token survives, and never for an account with
//! two-factor authentication on, where a password sign-in answers with a
//! challenge that needs a code no background task can give — and each attempt
//! leaves one on the server. The sync worker borrows this session rather than
//! signing in itself.

use super::cloud::CloudStatus;
use crate::AppState;
use serde::{Deserialize, Serialize};
use parking_lot::Mutex;
use std::sync::{Arc, LazyLock};
use tauri::State;

/// The signed-in user, as the server last described them.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Session {
    pub token: String,
    pub user_id: String,
    pub email: String,
    pub name: String,
    pub verified: bool,
}

/// What the Keychain holds for the signed-in account. `password` is absent for
/// an account with two-factor authentication on, which can't sign in with it
/// unattended; `token` is absent until a sign-in stores one.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct StoredAccount {
    pub email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

impl StoredAccount {
    /// Read the Keychain item's JSON. A blank secret reads as absent.
    pub(crate) fn parse(json: &str) -> Option<Self> {
        let mut account: Self = serde_json::from_str(json).ok()?;
        let present = |s: Option<String>| s.filter(|v| !v.trim().is_empty());
        account.password = present(account.password);
        account.token = present(account.token);
        Some(account)
    }
}

/// Why a session couldn't be had.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SessionError {
    /// Nothing left to sign in with: the user has to sign in again.
    SignedOut(String),
    /// The server didn't answer usefully. What the Keychain holds may still work.
    Unreachable(String),
}

impl From<SessionError> for String {
    fn from(e: SessionError) -> String {
        match e {
            SessionError::SignedOut(m) | SessionError::Unreachable(m) => m,
        }
    }
}

const SESSION_ENDED: &str = "Your session has ended. Sign in again.";

/// How the server turned down a request to sign in.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum AuthFailure {
    /// It refused what it was given: the token or the password is no good.
    Rejected(String),
    /// Any other failure, which says nothing about the credentials.
    Unreachable(String),
}

/// The answer to a correct password.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PasswordLogin {
    SignedIn(Session),
    /// The account has two-factor authentication on; `mfa_id` names the
    /// challenge the code goes to.
    TwoFactor { mfa_id: String },
}

fn str_field(val: &serde_json::Value, key: &str) -> String {
    val.get(key).and_then(|v| v.as_str()).unwrap_or_default().to_string()
}

/// The server's own words for a failure: hook routes say `message`, some say
/// `error`.
fn server_message(body: &serde_json::Value) -> String {
    let message = str_field(body, "message");
    if message.is_empty() {
        str_field(body, "error")
    } else {
        message
    }
}

/// A PocketBase auth response (`{ token, record }`) as a session.
pub(crate) fn parse_auth_response(body: &serde_json::Value) -> Option<Session> {
    let token = str_field(body, "token");
    if token.is_empty() {
        return None;
    }
    let record = body.get("record").cloned().unwrap_or_default();
    Some(Session {
        token,
        user_id: str_field(&record, "id"),
        email: str_field(&record, "email"),
        name: str_field(&record, "name"),
        verified: record.get("verified").and_then(|v| v.as_bool()).unwrap_or(false),
    })
}

fn signed_in_or_unreachable(body: &serde_json::Value) -> Result<Session, AuthFailure> {
    parse_auth_response(body)
        .ok_or_else(|| AuthFailure::Unreachable("the server sent no session".into()))
}

/// The answer to `auth-with-password`.
pub(crate) fn parse_password_login(
    status: u16,
    body: &serde_json::Value,
) -> Result<PasswordLogin, AuthFailure> {
    match status {
        200..=299 => signed_in_or_unreachable(body).map(PasswordLogin::SignedIn),
        401 if str_field(body, "reason") == "two_factor_required" => {
            Ok(PasswordLogin::TwoFactor { mfa_id: str_field(body, "mfaId") })
        }
        400 | 401 | 403 | 404 => Err(AuthFailure::Rejected(server_message(body))),
        _ => Err(AuthFailure::Unreachable(format!("the server answered {status}"))),
    }
}

/// The answer to `auth-refresh`.
pub(crate) fn parse_refresh(status: u16, body: &serde_json::Value) -> Result<Session, AuthFailure> {
    match status {
        200..=299 => signed_in_or_unreachable(body),
        401 | 403 => Err(AuthFailure::Rejected(server_message(body))),
        _ => Err(AuthFailure::Unreachable(format!("the server answered {status}"))),
    }
}

/// A failed cloud request as the two-factor UI receives it: `reason` to branch
/// on (the server's own code, or empty), `message` to show.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CloudError {
    pub reason: String,
    pub message: String,
}

impl From<String> for CloudError {
    fn from(message: String) -> Self {
        Self { reason: String::new(), message }
    }
}

impl From<SessionError> for CloudError {
    fn from(e: SessionError) -> Self {
        String::from(e).into()
    }
}

/// Also the wording for a workspace route's 403, wherever it surfaces.
pub(crate) const TWO_FACTOR_REQUIRED: &str =
    "This workspace requires two-factor authentication. Set it up in Settings → Account.";

fn join_names(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// A failed two-factor route (humla-cloud/docs/two-factor.md) as the user reads
/// it. The server's wording is kept except where its answer carries more than
/// the wording says: how long a lock lasts, which workspaces stand in the way.
pub(crate) fn two_factor_error(status: u16, body: &serde_json::Value) -> CloudError {
    let reason = str_field(body, "reason");
    let message = match reason.as_str() {
        "locked" => match body.get("retry_after").and_then(|v| v.as_u64()) {
            Some(secs) => {
                let minutes = secs.div_ceil(60).max(1);
                let unit = if minutes == 1 { "minute" } else { "minutes" };
                format!("Too many wrong codes. Try again in {minutes} {unit}.")
            }
            None => "Too many wrong codes. Try again in a few minutes.".to_string(),
        },
        "required_by_workspace" => {
            let names: Vec<String> = body
                .get("workspaces")
                .and_then(|w| w.as_array())
                .map(|a| a.iter().map(|w| str_field(w, "name")).filter(|n| !n.is_empty()).collect())
                .unwrap_or_default();
            match names.len() {
                0 => "A workspace you're in requires two-factor authentication. Leave it before \
                      turning it off."
                    .to_string(),
                1 => format!(
                    "{} requires two-factor authentication. Leave it before turning it off.",
                    names[0]
                ),
                _ => format!(
                    "{} require two-factor authentication. Leave them before turning it off.",
                    join_names(&names)
                ),
            }
        }
        "two_factor_required" => TWO_FACTOR_REQUIRED.to_string(),
        _ => {
            let server = server_message(body);
            if server.trim().is_empty() {
                format!("Something went wrong ({status}). Try again.")
            } else {
                server
            }
        }
    };
    CloudError { reason, message }
}

/// [`two_factor_error`] for a request to `path`. A server without two-factor
/// authentication answers its routes 404 with no reason, which says nothing
/// useful in its own words.
pub(crate) fn route_error(path: &str, status: u16, body: &serde_json::Value) -> CloudError {
    if status == 404 && path.starts_with("/api/humla/2fa/") && str_field(body, "reason").is_empty() {
        return CloudError {
            reason: "unsupported".into(),
            message: "This server doesn't support two-factor authentication.".into(),
        };
    }
    two_factor_error(status, body)
}

/// The second factor a route takes: an authenticator code (spaces dropped, as
/// apps show it in groups of three) or else a recovery code.
pub(crate) fn second_factor(
    code: Option<String>,
    recovery_code: Option<String>,
) -> Result<serde_json::Value, CloudError> {
    let code: String = code.unwrap_or_default().chars().filter(|c| !c.is_whitespace()).collect();
    if !code.is_empty() {
        return Ok(serde_json::json!({ "code": code }));
    }
    let recovery = recovery_code.unwrap_or_default().trim().to_string();
    if !recovery.is_empty() {
        return Ok(serde_json::json!({ "recovery_code": recovery }));
    }
    Err(CloudError {
        reason: "missing_code".into(),
        message: "Enter the code from your authenticator app.".into(),
    })
}

/// A workspace named by id and name only: all a member is told about one that
/// requires two-factor authentication they don't have.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct WorkspaceRef {
    pub id: String,
    pub name: String,
}

/// `GET /api/humla/2fa/status`. `available` is false on a server that doesn't
/// offer it, and the app then shows none of it.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct TwoFactorStatus {
    pub available: bool,
    pub enabled: bool,
    pub recovery_codes_left: u32,
    pub required_by: Vec<WorkspaceRef>,
}

pub(crate) fn parse_two_factor_status(body: &serde_json::Value) -> TwoFactorStatus {
    let flag = |k: &str| body.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
    TwoFactorStatus {
        available: flag("available"),
        enabled: flag("enabled"),
        recovery_codes_left: body
            .get("recovery_codes_left")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32,
        required_by: body
            .get("required_by")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .map(|w| WorkspaceRef { id: str_field(w, "id"), name: str_field(w, "name") })
                    .filter(|w| !w.id.is_empty())
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// The workspaces that hide themselves from the workspace list (`visible_ids`)
/// until the user has two-factor authentication. None, once they have it.
pub(crate) fn locked_workspaces(status: &TwoFactorStatus, visible_ids: &[&str]) -> Vec<WorkspaceRef> {
    if status.enabled {
        return Vec::new();
    }
    status.required_by.iter().filter(|w| !visible_ids.contains(&w.id.as_str())).cloned().collect()
}

/// A QR code as an SVG path of its dark modules, one unit per module, so the
/// page draws it with no markup of ours. The four-module quiet zone is the
/// page's to add.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct QrCode {
    pub size: u32,
    pub path: String,
}

pub(crate) fn qr_code(text: &str) -> Option<QrCode> {
    let qr = qrcodegen::QrCode::encode_text(text, qrcodegen::QrCodeEcc::Medium).ok()?;
    let size = qr.size();
    let mut path = String::new();
    for y in 0..size {
        let mut x = 0;
        while x < size {
            if !qr.get_module(x, y) {
                x += 1;
                continue;
            }
            let start = x;
            while x < size && qr.get_module(x, y) {
                x += 1;
            }
            let run = x - start;
            path.push_str(&format!("M{start} {y}h{run}v1h-{run}z"));
        }
    }
    Some(QrCode { size: size as u32, path })
}

/// What renewing a session asks of the server.
pub(crate) trait AuthServer {
    async fn refresh(&self, token: &str) -> Result<Session, AuthFailure>;
    async fn password_login(&self, email: &str, password: &str)
        -> Result<PasswordLogin, AuthFailure>;
}

/// Renew the session from what the Keychain holds: the token while it works, the
/// password only once no token is left. `account` comes back holding what
/// changed — the renewed token, a dead token or a password that can no longer
/// sign in removed — for the caller to store.
pub(crate) async fn renew(
    server: &impl AuthServer,
    account: &mut StoredAccount,
) -> Result<Session, SessionError> {
    if let Some(token) = account.token.clone() {
        match server.refresh(&token).await {
            Ok(session) => {
                account.token = Some(session.token.clone());
                return Ok(session);
            }
            Err(AuthFailure::Rejected(_)) => account.token = None,
            Err(AuthFailure::Unreachable(e)) => return Err(SessionError::Unreachable(e)),
        }
    }
    let Some(password) = account.password.clone() else {
        return Err(SessionError::SignedOut(SESSION_ENDED.into()));
    };
    match server.password_login(&account.email, &password).await {
        Ok(PasswordLogin::SignedIn(session)) => {
            account.token = Some(session.token.clone());
            Ok(session)
        }
        Ok(PasswordLogin::TwoFactor { .. }) | Err(AuthFailure::Rejected(_)) => {
            account.password = None;
            Err(SessionError::SignedOut(SESSION_ENDED.into()))
        }
        Err(AuthFailure::Unreachable(e)) => Err(SessionError::Unreachable(e)),
    }
}

// ---- the process's session --------------------------------------------------

const CRED_ACCOUNT: &str = "cloud_credentials";
const NOT_CONFIGURED: &str = "Cloud isn't configured — set the server URL first.";

static SESSION: LazyLock<Mutex<Option<Session>>> = LazyLock::new(|| Mutex::new(None));

/// Held while a session is renewed, so callers that find none at the same
/// moment wait for one renewal instead of each signing in.
static RENEWING: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));

/// A password sign-in waiting for its code: the one the code step answers.
#[derive(Clone)]
struct Challenge {
    base: String,
    email: String,
    mfa_id: String,
}

static CHALLENGE: LazyLock<Mutex<Option<Challenge>>> = LazyLock::new(|| Mutex::new(None));

pub(crate) fn cached_session() -> Option<Session> {
    SESSION.lock().clone()
}

fn set_session(session: Option<Session>) {
    *SESSION.lock() = session;
}

/// The signed-in user's PocketBase id, or empty when no session is cached.
pub(crate) fn current_user_id() -> String {
    cached_session().map(|s| s.user_id).unwrap_or_default()
}

/// Drop the cached session so the next [`ensure_session`] renews it — call
/// after a 401 from a direct endpoint call.
pub(crate) fn forget_session() {
    set_session(None);
}

/// [`forget_session`], unless the cache already holds a newer token than the
/// one the server refused.
pub(crate) fn forget_session_token(token: &str) {
    let mut cached = SESSION.lock();
    if cached.as_ref().is_some_and(|s| s.token == token) {
        *cached = None;
    }
}

/// Debug builds can take a password from the environment and skip the Keychain:
/// a fresh dev build's Keychain trust doesn't always stick. Compiled out of
/// release builds, and nothing it signs in with is ever written back.
fn dev_account() -> Option<StoredAccount> {
    #[cfg(debug_assertions)]
    {
        let email = std::env::var("HUMLA_DEV_SYNC_EMAIL").unwrap_or_default();
        let password = std::env::var("HUMLA_DEV_SYNC_PASSWORD").unwrap_or_default();
        if !email.is_empty() && !password.is_empty() {
            return Some(StoredAccount { email, password: Some(password), token: None });
        }
    }
    None
}

pub(crate) fn read_account(cache: &crate::keychain::Cache) -> Option<StoredAccount> {
    if let Some(dev) = dev_account() {
        return Some(dev);
    }
    let json = crate::keychain::read(cache, CRED_ACCOUNT).ok().flatten()?;
    StoredAccount::parse(&json)
}

/// Store the account, or delete it with `None`.
fn store_account(cache: &crate::keychain::Cache, account: Option<&StoredAccount>) -> Result<(), String> {
    if dev_account().is_some() {
        return Ok(());
    }
    let json = account.map(serde_json::to_string).transpose().map_err(|e| e.to_string())?;
    crate::keychain::write(cache, CRED_ACCOUNT, json.as_deref())
}

/// Whether there is anything to sign in with: what the sync worker needs
/// before it is worth starting.
pub(crate) fn has_credentials(cache: &crate::keychain::Cache) -> bool {
    read_account(cache).is_some_and(|a| a.token.is_some() || a.password.is_some())
}

/// The email of an account whose session ended without the user signing out,
/// for the sign-in form to ask them back.
pub(crate) fn signed_out_email(cache: &crate::keychain::Cache) -> Option<String> {
    read_account(cache).map(|a| a.email).filter(|e| !e.is_empty())
}

/// Return a usable `(base_url, session)`: the cached one, else one renewed
/// from the Keychain.
pub(crate) async fn ensure_session(
    state: &State<'_, AppState>,
) -> Result<(String, Session), SessionError> {
    obtain_session(state).await.map(|(base, session, _)| (base, session))
}

/// [`ensure_session`], also saying whether the session was renewed just now.
async fn obtain_session(
    state: &State<'_, AppState>,
) -> Result<(String, Session, bool), SessionError> {
    obtain_session_from(&state.db, &state.keychain_cache).await
}

/// [`ensure_session`] for the sync worker, which has the database and the
/// Keychain cache but can't rely on `AppState`: it is built before `AppState` is.
pub(crate) async fn ensure_session_from(
    db: &Arc<parking_lot::Mutex<rusqlite::Connection>>,
    cache: &crate::keychain::Cache,
) -> Result<(String, Session), SessionError> {
    obtain_session_from(db, cache).await.map(|(base, session, _)| (base, session))
}

async fn obtain_session_from(
    db: &Arc<parking_lot::Mutex<rusqlite::Connection>>,
    cache: &crate::keychain::Cache,
) -> Result<(String, Session, bool), SessionError> {
    let base = super::cloud::base_url(&db.lock())
        .ok_or_else(|| SessionError::SignedOut(NOT_CONFIGURED.into()))?;
    if let Some(session) = cached_session() {
        return Ok((base, session, false));
    }
    let _renewing = RENEWING.lock().await;
    if let Some(session) = cached_session() {
        return Ok((base, session, false));
    }
    let Some(mut account) = read_account(cache) else {
        return Err(SessionError::SignedOut("Not signed in.".into()));
    };
    let before = account.clone();
    let renewed = renew(&PocketBase { base: &base }, &mut account).await;
    if account != before {
        if let Err(e) = store_account(cache, Some(&account)) {
            eprintln!("cloud: couldn't store the renewed session: {e}");
        }
    }
    let session = renewed?;
    set_session(Some(session.clone()));
    adopt_audit_account(db, &session.user_id);
    Ok((base, session, true))
}

/// The session with its record read again from the server — `verified` changes
/// when the user clicks the email link — or the cached one when the server
/// can't be asked.
pub(crate) async fn fresh_session(
    state: &State<'_, AppState>,
) -> Result<(String, Session), SessionError> {
    let (base, session, renewed) = obtain_session(state).await?;
    if renewed {
        return Ok((base, session));
    }
    match (PocketBase { base: &base }).refresh(&session.token).await {
        Ok(fresh) => {
            set_session(Some(fresh.clone()));
            Ok((base, fresh))
        }
        Err(AuthFailure::Rejected(_)) => {
            forget_session_token(&session.token);
            ensure_session(state).await
        }
        Err(AuthFailure::Unreachable(_)) => Ok((base, session)),
    }
}

/// Cloud `(base_url, auth token)` for a command that calls the server directly
/// (Teams chat, issue #50). A 401 from the endpoint should call
/// [`forget_session`] and retry once.
pub(crate) async fn cloud_session(state: &State<'_, AppState>) -> Result<(String, String), String> {
    let (base, session) = ensure_session(state).await?;
    Ok((base, session.token))
}

/// Make `session` the signed-in one, here and in the Keychain. `password` is
/// kept only for an account that can sign in with it alone.
fn adopt(
    state: &State<'_, AppState>,
    session: Session,
    email: &str,
    password: Option<&str>,
) -> Result<(), String> {
    let account = StoredAccount {
        email: if session.email.is_empty() { email.to_string() } else { session.email.clone() },
        password: password.map(String::from),
        token: Some(session.token.clone()),
    };
    store_account(&state.keychain_cache, Some(&account))?;
    super::invites::clear();
    adopt_audit_account(&state.db, &session.user_id);
    set_session(Some(session));
    Ok(())
}

/// Reports queued under another account never go out as this one.
fn adopt_audit_account(db: &Arc<parking_lot::Mutex<rusqlite::Connection>>, user_id: &str) {
    if let Err(e) = crate::org_audit::adopt_account(&db.lock(), user_id) {
        eprintln!("org audit: couldn't switch accounts: {e:#}");
    }
}

/// Forget the session, any sign-in waiting for its code, and the stored account.
pub(crate) fn sign_out(state: &State<'_, AppState>) {
    forget_account(&state.keychain_cache);
}

/// [`sign_out`] through the Keychain cache, for callers that run before
/// `AppState` exists.
pub(crate) fn forget_account(cache: &crate::keychain::Cache) {
    forget_account_with(|account| store_account(cache, account));
}

fn forget_account_with(store: impl FnOnce(Option<&StoredAccount>) -> Result<(), String>) {
    super::invites::clear();
    if let Err(e) = store(None) {
        eprintln!("cloud: couldn't remove the stored account: {e}");
    }
    set_session(None);
    *CHALLENGE.lock() = None;
}

/// A new server invalidates the session, and its token must not be sent to it.
pub(crate) fn server_changed(state: &State<'_, AppState>) {
    if let Some(mut account) = read_account(&state.keychain_cache) {
        if account.token.take().is_some() {
            let _ = store_account(&state.keychain_cache, Some(&account));
        }
    }
    set_session(None);
    *CHALLENGE.lock() = None;
}

fn http() -> reqwest::Client {
    crate::http::client()
}

/// Read a response as `(status, JSON body)`; a body that isn't JSON reads as null.
async fn status_and_body(resp: reqwest::Response) -> (u16, serde_json::Value) {
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or_default())
}

struct PocketBase<'a> {
    base: &'a str,
}

impl AuthServer for PocketBase<'_> {
    async fn refresh(&self, token: &str) -> Result<Session, AuthFailure> {
        let resp = http()
            .post(format!("{}/api/collections/users/auth-refresh", self.base))
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| AuthFailure::Unreachable(e.to_string()))?;
        let (status, body) = status_and_body(resp).await;
        parse_refresh(status, &body)
    }

    async fn password_login(&self, email: &str, password: &str) -> Result<PasswordLogin, AuthFailure> {
        let resp = http()
            .post(format!("{}/api/collections/users/auth-with-password", self.base))
            .json(&serde_json::json!({ "identity": email, "password": password }))
            .send()
            .await
            .map_err(|e| AuthFailure::Unreachable(e.to_string()))?;
        let (status, body) = status_and_body(resp).await;
        parse_password_login(status, &body)
    }
}

fn unreachable(e: reqwest::Error) -> CloudError {
    format!("Couldn't reach the server ({e}).").into()
}

/// Sign in with a route's auth response, and restart sync on its token.
fn adopt_response(
    state: &State<'_, AppState>,
    body: &serde_json::Value,
    email: &str,
    password: Option<&str>,
) -> Result<(), CloudError> {
    let session = parse_auth_response(body)
        .ok_or_else(|| CloudError::from("The server sent no session.".to_string()))?;
    adopt(state, session, email, password)?;
    state.sync.config_changed();
    Ok(())
}

fn login_error(failure: AuthFailure) -> String {
    match failure {
        AuthFailure::Rejected(e) => {
            format!("login failed — check the server URL, email, and password ({e})")
        }
        AuthFailure::Unreachable(e) => format!("Couldn't reach the server ({e})."),
    }
}

// ---- signing in --------------------------------------------------------------

/// Where a password sign-in landed: signed in, or waiting for the code from the
/// user's authenticator app ([`cloud_verify_two_factor`]).
#[derive(Serialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum SignInStep {
    SignedIn { status: Box<CloudStatus> },
    TwoFactor,
}

#[tauri::command]
pub async fn cloud_login(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    email: String,
    password: String,
) -> Result<SignInStep, String> {
    let base = super::cloud::read_base_url(&state).ok_or(NOT_CONFIGURED)?;
    let email = email.trim();
    match (PocketBase { base: &base }).password_login(email, &password).await {
        Ok(PasswordLogin::SignedIn(session)) => {
            adopt(&state, session, email, Some(&password))?;
            *CHALLENGE.lock() = None;
            state.sync.config_changed(); // signed in → start the sync worker
            let status = super::cloud::cloud_status(app, state).await?;
            Ok(SignInStep::SignedIn { status: Box::new(status) })
        }
        Ok(PasswordLogin::TwoFactor { mfa_id }) => {
            *CHALLENGE.lock() = Some(Challenge { base, email: email.to_string(), mfa_id });
            Ok(SignInStep::TwoFactor)
        }
        Err(failure) => Err(login_error(failure)),
    }
}

/// Sign a just-created account in. It can't have two-factor authentication yet.
pub(crate) async fn sign_in_new_account(
    state: &State<'_, AppState>,
    base: &str,
    email: &str,
    password: &str,
) -> Result<(), String> {
    match (PocketBase { base }).password_login(email, password).await {
        Ok(PasswordLogin::SignedIn(session)) => adopt(state, session, email, Some(password)),
        Ok(PasswordLogin::TwoFactor { .. }) => {
            Err("The new account asked for a code. Sign in from Settings → Account.".into())
        }
        Err(failure) => Err(login_error(failure)),
    }
}

fn challenge_expired() -> CloudError {
    CloudError {
        reason: "challenge_expired".into(),
        message: "That sign-in has expired. Enter your password again.".into(),
    }
}

/// Finish a password sign-in that asked for a second factor: the code from the
/// authenticator app, or one of the recovery codes.
#[tauri::command]
pub async fn cloud_verify_two_factor(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    code: Option<String>,
    recovery_code: Option<String>,
) -> Result<CloudStatus, CloudError> {
    let challenge = CHALLENGE.lock().clone().ok_or_else(challenge_expired)?;
    let mut body = second_factor(code, recovery_code)?;
    body["mfaId"] = serde_json::Value::String(challenge.mfa_id.clone());
    let resp = http()
        .post(format!("{}/api/humla/2fa/verify", challenge.base))
        .json(&body)
        .send()
        .await
        .map_err(unreachable)?;
    let (status, body) = status_and_body(resp).await;
    if !(200..300).contains(&status) {
        let failure = two_factor_error(status, &body);
        if failure.reason == "challenge_expired" {
            *CHALLENGE.lock() = None;
        }
        return Err(failure);
    }
    adopt_response(&state, &body, &challenge.email, None)?;
    *CHALLENGE.lock() = None;
    Ok(super::cloud::cloud_status(app, state).await?)
}

// ---- managing two-factor authentication -------------------------------------

#[derive(Clone, Copy)]
pub(crate) enum Verb {
    Get,
    Post,
    Patch,
}

/// A request to a two-factor route or a workspace record, as the signed-in user:
/// the session is renewed and the request sent once more if the server refuses
/// its token. Failures read through [`route_error`].
pub(crate) async fn authed_call(
    state: &State<'_, AppState>,
    verb: Verb,
    path: &str,
    body: &serde_json::Value,
    query: &[(&str, &str)],
) -> Result<serde_json::Value, CloudError> {
    for attempt in 0..2 {
        let (base, session) = ensure_session(state).await?;
        let url = format!("{base}{path}");
        let req = match verb {
            Verb::Get => http().get(url).query(query),
            Verb::Post => http().post(url).json(body),
            Verb::Patch => http().patch(url).json(body),
        };
        let resp = req
            .bearer_auth(&session.token)
            .send()
            .await
            .map_err(unreachable)?;
        let (status, body) = status_and_body(resp).await;
        if status == 401 {
            forget_session_token(&session.token);
            if attempt == 0 {
                continue;
            }
            return Err(CloudError { reason: "unauthorized".into(), message: SESSION_ENDED.into() });
        }
        if !(200..300).contains(&status) {
            return Err(route_error(path, status, &body));
        }
        return Ok(body);
    }
    unreachable!("the loop returns on its second attempt")
}

/// The signed-in user's two-factor status, for `cloud_status`. A server that
/// doesn't offer it, or doesn't answer, reads as unavailable.
pub(crate) async fn fetch_two_factor_status(base: &str, token: &str) -> TwoFactorStatus {
    let Ok(resp) = http().get(format!("{base}/api/humla/2fa/status")).bearer_auth(token).send().await
    else {
        return TwoFactorStatus::default();
    };
    let (status, body) = status_and_body(resp).await;
    if (200..300).contains(&status) {
        parse_two_factor_status(&body)
    } else {
        TwoFactorStatus::default()
    }
}

/// What the authenticator app is set up with.
#[derive(Serialize)]
pub struct TwoFactorSetup {
    pub secret: String,
    pub otpauth_uri: String,
    pub qr: Option<QrCode>,
}

/// Start linking an authenticator app. With two-factor authentication already on
/// this replaces the app, and takes its code or a recovery code as well.
#[tauri::command]
pub async fn cloud_two_factor_setup(
    state: State<'_, AppState>,
    password: String,
    code: Option<String>,
    recovery_code: Option<String>,
) -> Result<TwoFactorSetup, CloudError> {
    let mut body = second_factor(code, recovery_code).unwrap_or_else(|_| serde_json::json!({}));
    body["password"] = serde_json::Value::String(password);
    let val = authed_call(&state, Verb::Post, "/api/humla/2fa/setup", &body, &[]).await?;
    let otpauth_uri = str_field(&val, "otpauth_uri");
    Ok(TwoFactorSetup { secret: str_field(&val, "secret"), qr: qr_code(&otpauth_uri), otpauth_uri })
}

fn recovery_codes(val: &serde_json::Value) -> Vec<String> {
    val.as_array()
        .map(|a| a.iter().filter_map(|c| c.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

/// Confirm the app with its first code. Returns the ten recovery codes, shown
/// once. The server signs out every other session here, so the token it answers
/// with replaces ours, and the password stops being kept.
#[tauri::command]
pub async fn cloud_two_factor_enable(
    state: State<'_, AppState>,
    code: String,
) -> Result<Vec<String>, CloudError> {
    let body = second_factor(Some(code), None)?;
    let val = authed_call(&state, Verb::Post, "/api/humla/2fa/enable", &body, &[]).await?;
    adopt_response(&state, &val, "", None)?;
    Ok(recovery_codes(val.pointer("/meta/recovery_codes").unwrap_or(&serde_json::Value::Null)))
}

/// Replace the recovery codes; the old ones stop working.
#[tauri::command]
pub async fn cloud_two_factor_recovery_codes(
    state: State<'_, AppState>,
    password: String,
    code: Option<String>,
    recovery_code: Option<String>,
) -> Result<Vec<String>, CloudError> {
    let mut body = second_factor(code, recovery_code)?;
    body["password"] = serde_json::Value::String(password);
    let val = authed_call(&state, Verb::Post, "/api/humla/2fa/recovery-codes", &body, &[]).await?;
    Ok(recovery_codes(val.get("recovery_codes").unwrap_or(&serde_json::Value::Null)))
}

/// Turn two-factor authentication off. Like enabling, it signs out every other
/// session; the password is kept again, as the fallback it was before.
#[tauri::command]
pub async fn cloud_two_factor_disable(
    state: State<'_, AppState>,
    password: String,
    code: Option<String>,
    recovery_code: Option<String>,
) -> Result<(), CloudError> {
    let mut body = second_factor(code, recovery_code)?;
    body["password"] = serde_json::Value::String(password.clone());
    let val = authed_call(&state, Verb::Post, "/api/humla/2fa/disable", &body, &[]).await?;
    adopt_response(&state, &val, "", Some(&password))

}

/// Which members of a workspace have two-factor authentication, by user id.
/// The owner and admins only.
#[tauri::command]
pub async fn cloud_two_factor_members(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<std::collections::HashMap<String, bool>, CloudError> {
    let val = authed_call(
        &state,
        Verb::Get,
        "/api/humla/2fa/members",
        &serde_json::Value::Null,
        &[("workspace_id", workspace_id.as_str())],
    )
    .await?;
    Ok(val
        .get("members")
        .and_then(|m| m.as_object())
        .map(|o| o.iter().map(|(id, on)| (id.clone(), on.as_bool().unwrap_or(false))).collect())
        .unwrap_or_default())
}

/// The text file the recovery codes are saved as. Refuses anything that isn't a
/// recovery code, so the command can't write arbitrary text.
pub(crate) fn recovery_codes_file(codes: &[String]) -> Result<String, String> {
    let well_formed = |c: &String| {
        (4..=40).contains(&c.len()) && c.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    };
    if codes.is_empty() || codes.len() > 20 || !codes.iter().all(well_formed) {
        return Err("Those aren't recovery codes.".into());
    }
    Ok(format!(
        "Humla recovery codes\n\nEach code signs you in once, when you can't use your \
         authenticator app.\n\n{}\n",
        codes.join("\n")
    ))
}

/// Save the recovery codes to a text file the user picked.
#[tauri::command]
pub fn cloud_save_recovery_codes(path: String, codes: Vec<String>) -> Result<(), String> {
    if !path.ends_with(".txt") {
        return Err("Recovery codes are saved as a .txt file.".into());
    }
    std::fs::write(&path, recovery_codes_file(&codes)?)
        .map_err(|e| format!("Couldn't save the recovery codes: {e}"))
}

/// Require two-factor authentication of every member. The server refuses unless
/// the caller has it themselves.
#[tauri::command]
pub async fn cloud_set_workspace_require_2fa(
    state: State<'_, AppState>,
    workspace_id: String,
    require: bool,
) -> Result<(), CloudError> {
    let path = format!("/api/collections/workspaces/records/{workspace_id}");
    authed_call(&state, Verb::Patch, &path, &serde_json::json!({ "require_2fa": require }), &[])
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn forgetting_the_account_leaves_no_credentials() {
        set_session(Some(session("old-server-token")));
        *CHALLENGE.lock() = Some(Challenge { base: "https://old".into(), email: "m@example.no".into(), mfa_id: "m1".into() });
        let mut stored = Some(account(Some("hunter2"), Some("t")));
        forget_account_with(|a| {
            stored = a.cloned();
            Ok(())
        });
        assert_eq!(stored, None, "email, password and token all go");
        assert!(cached_session().is_none());
        assert!(CHALLENGE.lock().is_none());
    }

    fn session(token: &str) -> Session {
        Session { token: token.into(), user_id: "u1".into(), ..Session::default() }
    }

    fn account(password: Option<&str>, token: Option<&str>) -> StoredAccount {
        StoredAccount {
            email: "m@example.no".into(),
            password: password.map(String::from),
            token: token.map(String::from),
        }
    }

    /// Answers every call the same way and records which were made.
    struct FakeServer {
        refresh: Result<Session, AuthFailure>,
        login: Result<PasswordLogin, AuthFailure>,
        calls: Mutex<Vec<&'static str>>,
    }

    impl FakeServer {
        fn new(
            refresh: Result<Session, AuthFailure>,
            login: Result<PasswordLogin, AuthFailure>,
        ) -> Self {
            Self { refresh, login, calls: Mutex::new(Vec::new()) }
        }
        fn calls(&self) -> Vec<&'static str> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl AuthServer for FakeServer {
        async fn refresh(&self, _token: &str) -> Result<Session, AuthFailure> {
            self.calls.lock().unwrap().push("refresh");
            self.refresh.clone()
        }
        async fn password_login(&self, _: &str, _: &str) -> Result<PasswordLogin, AuthFailure> {
            self.calls.lock().unwrap().push("login");
            self.login.clone()
        }
    }

    #[tokio::test]
    async fn a_stored_token_is_renewed_without_the_password() {
        let server = FakeServer::new(
            Ok(session("new")),
            Err(AuthFailure::Unreachable("not called".into())),
        );
        let mut stored = account(Some("pw"), Some("old"));

        let renewed = renew(&server, &mut stored).await.unwrap();

        assert_eq!(renewed.token, "new");
        assert_eq!(stored, account(Some("pw"), Some("new")));
        assert_eq!(server.calls(), vec!["refresh"]);
    }

    /// An account without two-factor authentication keeps signing in by itself
    /// once its token has expired.
    #[tokio::test]
    async fn a_rejected_token_falls_back_to_the_password() {
        let server = FakeServer::new(
            Err(AuthFailure::Rejected("expired".into())),
            Ok(PasswordLogin::SignedIn(session("fresh"))),
        );
        let mut stored = account(Some("pw"), Some("dead"));

        let renewed = renew(&server, &mut stored).await.unwrap();

        assert_eq!(renewed.token, "fresh");
        assert_eq!(stored, account(Some("pw"), Some("fresh")));
        assert_eq!(server.calls(), vec!["refresh", "login"]);
    }

    /// Every password sign-in of an account with two-factor authentication
    /// leaves a challenge on the server, so the password is dropped the first
    /// time it is answered with one.
    #[tokio::test]
    async fn a_password_answered_with_a_challenge_is_never_tried_again() {
        let server = FakeServer::new(
            Err(AuthFailure::Rejected("expired".into())),
            Ok(PasswordLogin::TwoFactor { mfa_id: "c1".into() }),
        );
        let mut stored = account(Some("pw"), Some("dead"));

        assert!(matches!(renew(&server, &mut stored).await, Err(SessionError::SignedOut(_))));
        assert_eq!(stored, account(None, None));

        assert!(matches!(renew(&server, &mut stored).await, Err(SessionError::SignedOut(_))));
        assert_eq!(server.calls(), vec!["refresh", "login"], "no second sign-in attempt");
    }

    /// A token that couldn't be checked isn't a dead one: offline at launch
    /// must not cost the session, nor reach for the password.
    #[tokio::test]
    async fn an_unreachable_server_keeps_the_stored_token() {
        let server = FakeServer::new(
            Err(AuthFailure::Unreachable("offline".into())),
            Ok(PasswordLogin::SignedIn(session("fresh"))),
        );
        let mut stored = account(Some("pw"), Some("old"));

        assert_eq!(
            renew(&server, &mut stored).await,
            Err(SessionError::Unreachable("offline".into()))
        );
        assert_eq!(stored, account(Some("pw"), Some("old")));
        assert_eq!(server.calls(), vec!["refresh"]);
    }

    /// An account stored without a token still reads, and signs in with its password.
    #[test]
    fn credentials_stored_before_tokens_still_read() {
        let stored = StoredAccount::parse(r#"{"email":"m@example.no","password":"pw"}"#);
        assert_eq!(stored, Some(account(Some("pw"), None)));
        let blank = StoredAccount::parse(r#"{"email":"m@example.no","password":"  "}"#);
        assert_eq!(blank, Some(account(None, None)));
        assert_eq!(StoredAccount::parse("not json"), None);
    }

    fn auth_body(token: &str) -> serde_json::Value {
        serde_json::json!({
            "token": token,
            "record": { "id": "u1", "email": "m@example.no", "name": "Michael", "verified": true },
        })
    }

    /// The server's half of humla-cloud/docs/two-factor.md, "Signing in".
    #[test]
    fn a_password_sign_in_reads_as_signed_in_challenged_or_refused() {
        let signed_in = parse_password_login(200, &auth_body("t1"));
        assert_eq!(
            signed_in,
            Ok(PasswordLogin::SignedIn(Session {
                token: "t1".into(),
                user_id: "u1".into(),
                email: "m@example.no".into(),
                name: "Michael".into(),
                verified: true,
            }))
        );

        let challenge = serde_json::json!({
            "message": "Enter the code from your authenticator app.",
            "reason": "two_factor_required",
            "mfaId": "c1",
        });
        assert_eq!(
            parse_password_login(401, &challenge),
            Ok(PasswordLogin::TwoFactor { mfa_id: "c1".into() })
        );

        let wrong = serde_json::json!({ "status": 400, "message": "Failed to authenticate." });
        assert!(matches!(parse_password_login(400, &wrong), Err(AuthFailure::Rejected(_))));
        assert!(matches!(
            parse_password_login(503, &serde_json::json!({})),
            Err(AuthFailure::Unreachable(_))
        ));
        // A 2xx without a token is not a session.
        assert!(matches!(
            parse_password_login(200, &serde_json::json!({ "record": {} })),
            Err(AuthFailure::Unreachable(_))
        ));
    }

    #[test]
    fn a_refresh_is_refused_only_by_an_auth_status() {
        assert_eq!(parse_refresh(200, &auth_body("t2")).map(|s| s.token), Ok("t2".into()));
        assert!(matches!(
            parse_refresh(401, &serde_json::json!({ "message": "invalid token" })),
            Err(AuthFailure::Rejected(_))
        ));
        assert!(matches!(
            parse_refresh(502, &serde_json::json!({})),
            Err(AuthFailure::Unreachable(_))
        ));
    }

    /// The reason survives for the UI to branch on (challenge_expired sends the
    /// sign-in back to the password); the message is what the user reads.
    #[test]
    fn a_two_factor_failure_keeps_its_reason_and_says_what_to_do() {
        let wrong = serde_json::json!({ "message": "That code isn't right.", "reason": "invalid_code" });
        assert_eq!(
            two_factor_error(400, &wrong),
            CloudError { reason: "invalid_code".into(), message: "That code isn't right.".into() }
        );

        let locked = |secs: u64| {
            two_factor_error(
                429,
                &serde_json::json!({ "message": "…", "reason": "locked", "retry_after": secs }),
            )
            .message
        };
        assert_eq!(locked(900), "Too many wrong codes. Try again in 15 minutes.");
        assert_eq!(locked(61), "Too many wrong codes. Try again in 2 minutes.");
        assert_eq!(locked(20), "Too many wrong codes. Try again in 1 minute.");

        let required = |names: &[&str]| {
            let workspaces: Vec<_> =
                names.iter().map(|n| serde_json::json!({ "id": "w", "name": n })).collect();
            two_factor_error(
                409,
                &serde_json::json!({ "reason": "required_by_workspace", "workspaces": workspaces }),
            )
            .message
        };
        assert_eq!(
            required(&["Acme"]),
            "Acme requires two-factor authentication. Leave it before turning it off."
        );
        assert_eq!(
            required(&["Acme", "Beta", "Gamma"]),
            "Acme, Beta and Gamma require two-factor authentication. Leave them before turning it off."
        );

        // A server without the routes answers them 404 with no reason; any other
        // 404 keeps its own words.
        let missing = serde_json::json!({ "message": "The requested resource wasn't found." });
        assert_eq!(
            route_error("/api/humla/2fa/setup", 404, &missing).message,
            "This server doesn't support two-factor authentication."
        );
        assert_eq!(
            route_error("/api/collections/workspaces/records/w1", 404, &missing).message,
            "The requested resource wasn't found."
        );
        let gone = serde_json::json!({ "message": "Workspace not found.", "reason": "not_found" });
        assert_eq!(route_error("/api/humla/2fa/members", 404, &gone).message, "Workspace not found.");
        assert!(!two_factor_error(500, &serde_json::json!({})).message.is_empty());
    }

    #[test]
    fn a_second_factor_is_a_code_or_a_recovery_code() {
        assert_eq!(
            second_factor(Some(" 123 456 ".into()), None),
            Ok(serde_json::json!({ "code": "123456" }))
        );
        assert_eq!(
            second_factor(Some("".into()), Some(" ABCDE-12345 ".into())),
            Ok(serde_json::json!({ "recovery_code": "ABCDE-12345" }))
        );
        assert_eq!(second_factor(None, Some("  ".into())).map_err(|e| e.reason), Err("missing_code".into()));
    }

    fn ws(id: &str, name: &str) -> WorkspaceRef {
        WorkspaceRef { id: id.into(), name: name.into() }
    }

    #[test]
    fn the_two_factor_status_reads_whatever_the_server_leaves_out_as_off() {
        let body = serde_json::json!({
            "available": true,
            "enabled": false,
            "recovery_codes_left": 0,
            "required_by": [{ "id": "w1", "name": "Acme" }],
        });
        assert_eq!(
            parse_two_factor_status(&body),
            TwoFactorStatus {
                available: true,
                enabled: false,
                recovery_codes_left: 0,
                required_by: vec![ws("w1", "Acme")],
            }
        );
        assert_eq!(parse_two_factor_status(&serde_json::json!({})), TwoFactorStatus::default());
    }

    /// `required_by` names every workspace that requires it, including the ones
    /// a member with 2FA can open; only those missing from the list are locked.
    #[test]
    fn a_workspace_is_locked_when_it_requires_two_factor_and_is_hidden() {
        let required = vec![ws("w1", "Acme"), ws("w2", "Beta")];
        let off = TwoFactorStatus { available: true, required_by: required.clone(), ..Default::default() };
        assert_eq!(locked_workspaces(&off, &["w2"]), vec![ws("w1", "Acme")]);
        assert_eq!(locked_workspaces(&off, &["w1", "w2"]), vec![]);
        // With it on, nothing is locked — even when the list came back empty.
        let on = TwoFactorStatus { enabled: true, ..off };
        assert_eq!(locked_workspaces(&on, &[]), vec![]);
    }

    /// The file holds the codes and what they are for, and nothing that isn't a
    /// recovery code can be written through it.
    #[test]
    fn recovery_codes_are_saved_as_a_note_with_the_codes_and_nothing_else() {
        let file = recovery_codes_file(&["ABCDE-12345".into(), "FGHIJ-67890".into()]).unwrap();
        assert!(file.starts_with("Humla recovery codes\n"));
        assert!(file.ends_with("\nABCDE-12345\nFGHIJ-67890\n"));
        assert!(recovery_codes_file(&["#!/bin/sh".into()]).is_err());
        assert!(recovery_codes_file(&[]).is_err());
    }

    /// ISO/IEC 18004: every symbol starts with a 7×7 finder pattern, whose top
    /// row is seven dark modules — a known answer that the encoder didn't write.
    #[test]
    fn the_setup_qr_code_is_a_path_of_its_dark_modules() {
        let qr = qr_code("otpauth://totp/Humla:m%40example.no?secret=JBSWY3DPEHPK3PXP&issuer=Humla")
            .expect("fits in a QR code");
        assert!(qr.size >= 21 && (qr.size - 17).is_multiple_of(4), "a valid version's size: {}", qr.size);
        assert!(qr.path.starts_with("M0 0h7v1h-7z"), "{}", &qr.path[..40.min(qr.path.len())]);
        // The finder pattern's second row: dark, five light, dark.
        assert!(qr.path.contains("M0 1h1v1h-1zM6 1h1v1h-1z"));
    }
}
