//! Signing in with Microsoft (Entra ID) through PocketBase's realtime OAuth2
//! flow (humla-cloud `docs/organisation-layer.md` §15): the browser finishes at
//! the server's own redirect page, which hands the code back over a realtime
//! connection this process holds open. The session is stored like a password
//! sign-in's, without a password, so it ends as any token does.

use super::cloud_auth::{
    finish_sign_in, http, parse_auth_response, status_and_body, str_field, CloudError,
    PasswordLogin, SignInStep, NOT_CONFIGURED,
};
use crate::AppState;
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::Value;
use std::sync::LazyLock;
use std::time::Duration;
use tauri::State;
use tokio::sync::oneshot;

const PROVIDER: &str = "microsoft";
const WAIT: Duration = Duration::from_secs(5 * 60);

/// What `auth-methods` says about the Microsoft provider.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Provider {
    pub auth_url: String,
    pub code_verifier: String,
}

/// The Microsoft provider, when the server offers OAuth2 and lists it.
pub(crate) fn microsoft_provider(body: &Value) -> Option<Provider> {
    let oauth2 = body.get("oauth2")?;
    if !oauth2.get("enabled").and_then(Value::as_bool).unwrap_or(false) {
        return None;
    }
    let provider = oauth2
        .get("providers")?
        .as_array()?
        .iter()
        .find(|p| str_field(p, "name") == PROVIDER)?;
    let auth_url = str_field(provider, "authURL");
    let code_verifier = str_field(provider, "codeVerifier");
    if auth_url.is_empty() || code_verifier.is_empty() {
        return None;
    }
    Some(Provider { auth_url, code_verifier })
}

pub(crate) fn redirect_url(base: &str) -> String {
    format!("{}/api/oauth2-redirect", base.trim_end_matches('/'))
}

/// The provider's `authURL` with `state` set to the realtime client id,
/// `redirect_uri` to the server's redirect page and `login_hint` to the email
/// typed in the form, if any. Only an http(s) URL is opened.
pub(crate) fn browser_url(
    auth_url: &str,
    client_id: &str,
    redirect: &str,
    login_hint: &str,
) -> Option<String> {
    let mut url = reqwest::Url::parse(auth_url).ok()?;
    if !matches!(url.scheme(), "https" | "http") {
        return None;
    }
    let mut pairs: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(k, _)| k != "state" && k != "redirect_uri" && k != "login_hint")
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    pairs.push(("state".into(), client_id.into()));
    pairs.push(("redirect_uri".into(), redirect.into()));
    let login_hint = login_hint.trim();
    if !login_hint.is_empty() {
        pairs.push(("login_hint".into(), login_hint.into()));
    }
    url.query_pairs_mut().clear().extend_pairs(pairs);
    Some(url.into())
}

/// humla-cloud's answer to whether `email` signs in with Microsoft. A blank
/// email asks whether the server offers it to everyone.
pub(crate) fn sign_in_methods_url(base: &str, email: &str) -> Option<String> {
    let mut url =
        reqwest::Url::parse(&format!("{}/api/humla/sign-in-methods", base.trim_end_matches('/')))
            .ok()?;
    let email = email.trim();
    if !email.is_empty() {
        url.query_pairs_mut().append_pair("email", email);
    }
    Some(url.into())
}

/// Anything but a 2xx answer saying `microsoft: true` is no Microsoft; an older
/// server answers 404.
pub(crate) fn parse_sign_in_methods(status: u16, body: &Value) -> SignInMethods {
    let offered = body.get("microsoft").and_then(Value::as_bool).unwrap_or(false);
    SignInMethods { microsoft: (200..300).contains(&status) && offered }
}

/// One server-sent event.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SseEvent {
    pub event: String,
    pub data: String,
}

/// Server-sent events from a byte stream that may split anywhere.
#[derive(Default)]
pub(crate) struct SseParser {
    buf: Vec<u8>,
    event: String,
    data: Vec<String>,
}

impl SseParser {
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Vec<SseEvent> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(end) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                if !self.data.is_empty() {
                    out.push(SseEvent {
                        event: std::mem::take(&mut self.event),
                        data: std::mem::take(&mut self.data).join("\n"),
                    });
                }
                self.event.clear();
                continue;
            }
            if line.starts_with(':') {
                continue;
            }
            let (field, value) = line.split_once(':').unwrap_or((line, ""));
            let value = value.strip_prefix(' ').unwrap_or(value);
            match field {
                "event" => self.event = value.to_string(),
                "data" => self.data.push(value.to_string()),
                _ => {}
            }
        }
        out
    }
}

/// The realtime client id from the connection's first event.
pub(crate) fn connect_client_id(event: &SseEvent) -> Option<String> {
    if event.event != "PB_CONNECT" {
        return None;
    }
    let body: Value = serde_json::from_str(&event.data).ok()?;
    Some(str_field(&body, "clientId")).filter(|id| !id.is_empty())
}

fn failure(reason: &str, message: &str) -> CloudError {
    CloudError { reason: reason.into(), message: message.into() }
}

fn unavailable() -> CloudError {
    failure("unavailable", "This server doesn't offer sign-in with Microsoft.")
}

fn unreachable() -> CloudError {
    failure("unreachable", "Couldn't reach the server.")
}

fn did_not_finish() -> CloudError {
    failure("failed", "Sign-in with Microsoft didn't finish. Try again.")
}

fn cancelled() -> CloudError {
    failure("cancelled", "Sign-in with Microsoft was cancelled.")
}

fn timed_out() -> CloudError {
    failure("timeout", "Sign-in with Microsoft timed out. Try again.")
}

/// The code from the `@oauth2` event, which counts only when its `state` is
/// this connection's client id.
pub(crate) fn oauth2_code(data: &str, client_id: &str) -> Result<String, CloudError> {
    let body: Value = serde_json::from_str(data).map_err(|_| did_not_finish())?;
    if client_id.is_empty() || str_field(&body, "state") != client_id {
        return Err(did_not_finish());
    }
    match str_field(&body, "error").as_str() {
        "" => {}
        "access_denied" => return Err(cancelled()),
        _ => return Err(did_not_finish()),
    }
    Some(str_field(&body, "code")).filter(|c| !c.is_empty()).ok_or_else(did_not_finish)
}

/// The answer to `auth-with-oauth2`. Refusals read from Humla's own reasons,
/// never from the server's wording.
pub(crate) fn parse_oauth2_login(status: u16, body: &Value) -> Result<PasswordLogin, CloudError> {
    let reason = str_field(body, "reason");
    match (status, reason.as_str()) {
        (200..=299, _) => parse_auth_response(body)
            .map(PasswordLogin::SignedIn)
            .ok_or_else(did_not_finish),
        (401, "two_factor_required") => match str_field(body, "mfaId") {
            mfa_id if !mfa_id.is_empty() => Ok(PasswordLogin::TwoFactor { mfa_id }),
            _ => Err(did_not_finish()),
        },
        (_, "entra_tenant_not_enabled") => Err(failure(
            &reason,
            "Your organisation doesn't sign in to Humla with Microsoft.",
        )),
        (_, "entra_account_unsupported") => {
            Err(failure(&reason, "This Microsoft account can't sign in to Humla."))
        }
        (_, "entra_unavailable" | "entra_groups_unavailable") => {
            Err(failure(&reason, "Microsoft didn't answer. Try again shortly."))
        }
        (500.., _) => Err(failure(
            "server_error",
            "The server couldn't finish the sign-in. Try again shortly.",
        )),
        _ => Err(did_not_finish()),
    }
}

// ---- the flow ----------------------------------------------------------------

/// The sign-in in progress. Dropping its sender cancels it, so a new attempt or
/// [`cancel`] ends the one before.
static ATTEMPT: LazyLock<Mutex<Option<(u64, oneshot::Sender<()>)>>> =
    LazyLock::new(|| Mutex::new(None));
static NEXT_ATTEMPT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// End a sign-in waiting on the browser, if there is one.
pub(crate) fn cancel() {
    *ATTEMPT.lock() = None;
}

fn log(what: &str, e: reqwest::Error) {
    eprintln!("cloud: microsoft sign-in: {what}: {}", e.without_url());
}

async fn fetch_provider(base: &str) -> Result<Provider, CloudError> {
    let resp = http()
        .get(format!("{base}/api/collections/users/auth-methods"))
        .send()
        .await
        .map_err(|e| {
            log("auth-methods", e);
            unreachable()
        })?;
    let (status, body) = status_and_body(resp).await;
    if !(200..300).contains(&status) {
        return Err(unavailable());
    }
    microsoft_provider(&body).ok_or_else(unavailable)
}

/// Events from the realtime connection, one at a time.
struct Realtime {
    resp: reqwest::Response,
    parser: SseParser,
    pending: std::collections::VecDeque<SseEvent>,
}

impl Realtime {
    async fn open(base: &str) -> Result<Self, CloudError> {
        let resp = http()
            .get(format!("{base}/api/realtime"))
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .send()
            .await
            .map_err(|e| {
                log("realtime", e);
                unreachable()
            })?;
        if !resp.status().is_success() {
            return Err(unreachable());
        }
        Ok(Self { resp, parser: SseParser::default(), pending: Default::default() })
    }

    /// The next event; the connection ending is a sign-in that didn't finish.
    async fn next(&mut self) -> Result<SseEvent, CloudError> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return Ok(event);
            }
            match self.resp.chunk().await {
                Ok(Some(bytes)) => self.pending.extend(self.parser.push(&bytes)),
                Ok(None) => return Err(did_not_finish()),
                Err(e) => {
                    log("realtime", e);
                    return Err(did_not_finish());
                }
            }
        }
    }
}

/// The flow up to the server's answer. `open_browser` gets the provider URL.
async fn microsoft_login(
    base: &str,
    login_hint: &str,
    open_browser: impl FnOnce(String) -> Result<(), String>,
) -> Result<PasswordLogin, CloudError> {
    let provider = fetch_provider(base).await?;
    let mut realtime = Realtime::open(base).await?;
    let client_id = connect_client_id(&realtime.next().await?).ok_or_else(did_not_finish)?;

    let resp = http()
        .post(format!("{base}/api/realtime"))
        .json(&serde_json::json!({ "clientId": client_id, "subscriptions": ["@oauth2"] }))
        .send()
        .await
        .map_err(|e| {
            log("subscribe", e);
            unreachable()
        })?;
    if !resp.status().is_success() {
        return Err(did_not_finish());
    }

    let redirect = redirect_url(base);
    let url = browser_url(&provider.auth_url, &client_id, &redirect, login_hint)
        .ok_or_else(did_not_finish)?;
    // The error may carry the URL, and with it the login hint.
    open_browser(url).map_err(|_| {
        eprintln!("cloud: microsoft sign-in: couldn't open the browser");
        failure("failed", "Couldn't open your browser.")
    })?;

    let code = loop {
        let event = realtime.next().await?;
        if event.event == "@oauth2" {
            break oauth2_code(&event.data, &client_id)?;
        }
    };
    drop(realtime);

    let resp = http()
        .post(format!("{base}/api/collections/users/auth-with-oauth2"))
        .json(&serde_json::json!({
            "provider": PROVIDER,
            "code": code,
            "codeVerifier": provider.code_verifier,
            "redirectURL": redirect,
        }))
        .send()
        .await
        .map_err(|e| {
            log("auth-with-oauth2", e);
            unreachable()
        })?;
    let (status, body) = status_and_body(resp).await;
    parse_oauth2_login(status, &body)
}

fn open_in_browser(app: &tauri::AppHandle, url: String) -> Result<(), String> {
    #[allow(deprecated)]
    tauri_plugin_shell::ShellExt::shell(app).open(url, None).map_err(|e| e.to_string())
}

#[derive(Debug, Default, PartialEq, Serialize)]
pub struct SignInMethods {
    pub microsoft: bool,
}

/// The sign-in methods the configured server offers `email` beside the
/// password; with no email, the ones it offers everyone.
#[tauri::command]
pub async fn cloud_sign_in_methods(
    state: State<'_, AppState>,
    email: Option<String>,
) -> Result<SignInMethods, String> {
    let Some(url) = super::cloud::read_base_url(&state)
        .and_then(|base| sign_in_methods_url(&base, email.as_deref().unwrap_or("")))
    else {
        return Ok(SignInMethods::default());
    };
    match http().get(url).send().await {
        Ok(resp) => {
            let (status, body) = status_and_body(resp).await;
            Ok(parse_sign_in_methods(status, &body))
        }
        Err(e) => {
            log("sign-in-methods", e);
            Ok(SignInMethods::default())
        }
    }
}

/// Sign in with Microsoft in the system browser. Resolves once the browser
/// comes back, the user cancels ([`cloud_cancel_microsoft`]) or five minutes
/// pass; an account with two-factor authentication goes on to the code step.
/// `email`, the address typed in the form, prefills Microsoft's sign-in page.
#[tauri::command]
pub async fn cloud_login_microsoft(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    email: Option<String>,
) -> Result<SignInStep, CloudError> {
    let base = super::cloud::read_base_url(&state)
        .ok_or_else(|| CloudError::from(NOT_CONFIGURED.to_string()))?;
    let id = NEXT_ATTEMPT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let (tx, stopped) = oneshot::channel::<()>();
    *ATTEMPT.lock() = Some((id, tx));

    let outcome = tokio::select! {
        r = microsoft_login(&base, email.as_deref().unwrap_or(""), |url| open_in_browser(&app, url)) => r,
        _ = stopped => Err(cancelled()),
        _ = tokio::time::sleep(WAIT) => Err(timed_out()),
    };
    let current = {
        let mut attempt = ATTEMPT.lock();
        let current = attempt.as_ref().is_some_and(|(n, _)| *n == id);
        if current {
            *attempt = None;
        }
        current
    };
    // A sign-in cancelled or replaced while its last request was answered
    // doesn't sign in.
    if !current {
        return Err(cancelled());
    }
    Ok(finish_sign_in(app, state, base, outcome?, "", None, true).await?)
}

#[tauri::command]
pub fn cloud_cancel_microsoft() {
    cancel();
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The whole flow against a real PocketBase whose Microsoft provider points
    /// at humla-cloud's `scripts/fake-entra.py`; the browser is stood in for by
    /// a request to the redirect page with the code Microsoft would send.
    /// Skipped unless HUMLA_TEST_ENTRA_PB_URL and HUMLA_TEST_ENTRA_CODE are set;
    /// HUMLA_TEST_ENTRA_EXPECT is `signed_in:<email>`, `two_factor` or a reason.
    #[tokio::test]
    async fn a_microsoft_sign_in_against_pocketbase() {
        let (Ok(base), Ok(code), Ok(expect)) = (
            std::env::var("HUMLA_TEST_ENTRA_PB_URL"),
            std::env::var("HUMLA_TEST_ENTRA_CODE"),
            std::env::var("HUMLA_TEST_ENTRA_EXPECT"),
        ) else {
            eprintln!("a_microsoft_sign_in_against_pocketbase: skipped");
            return;
        };
        let opened = std::sync::Arc::new(Mutex::new(None::<String>));
        let seen = opened.clone();
        let outcome = microsoft_login(&base, "", move |url| {
            let parsed = reqwest::Url::parse(&url).map_err(|e| e.to_string())?;
            let q = |k: &str| parsed.query_pairs().find(|(n, _)| n == k).map(|(_, v)| v.into_owned());
            let (state, redirect) = (q("state").unwrap(), q("redirect_uri").unwrap());
            *seen.lock() = Some(url);
            tokio::spawn(async move {
                let _ = http().get(redirect).query(&[("state", state), ("code", code)]).send().await;
            });
            Ok(())
        })
        .await;
        let url = opened.lock().clone().expect("the browser was opened");
        assert!(url.contains("redirect_uri="), "{url}");
        let got = match outcome {
            Ok(PasswordLogin::SignedIn(s)) => format!("signed_in:{}", s.email),
            Ok(PasswordLogin::TwoFactor { mfa_id }) => {
                assert!(!mfa_id.is_empty());
                "two_factor".into()
            }
            Err(e) => {
                assert!(!e.message.contains(&base), "{}", e.message);
                e.reason
            }
        };
        assert_eq!(got, expect);
    }

    #[test]
    fn microsoft_is_offered_only_when_the_server_lists_it() {
        let methods = |enabled: bool, providers: Value| {
            json!({ "password": { "enabled": true }, "oauth2": { "enabled": enabled, "providers": providers } })
        };
        let ms = json!({ "name": "microsoft", "authURL": "https://login.example/authorize?client_id=x&state=s&redirect_uri=", "codeVerifier": "v", "state": "s" });
        assert_eq!(
            microsoft_provider(&methods(true, json!([{ "name": "google" }, ms.clone()]))),
            Some(Provider {
                auth_url: "https://login.example/authorize?client_id=x&state=s&redirect_uri=".into(),
                code_verifier: "v".into(),
            })
        );
        assert_eq!(microsoft_provider(&methods(false, json!([ms]))), None);
        assert_eq!(microsoft_provider(&methods(true, json!([{ "name": "google", "authURL": "https://g", "codeVerifier": "v" }]))), None);
        assert_eq!(microsoft_provider(&methods(true, json!([{ "name": "microsoft", "authURL": "" , "codeVerifier": "v" }]))), None);
        assert_eq!(microsoft_provider(&json!({ "password": { "enabled": true } })), None);
    }

    #[test]
    fn the_browser_url_carries_the_client_id_as_state_and_the_servers_redirect() {
        let redirect = redirect_url("https://sync.example.no/");
        assert_eq!(redirect, "https://sync.example.no/api/oauth2-redirect");
        let url = browser_url(
            "https://login.microsoftonline.com/organizations/oauth2/v2.0/authorize?client_id=abc&code_challenge=c%2Bd&response_type=code&scope=User.Read&state=server-state&redirect_uri=",
            "client-1",
            &redirect,
            "",
        )
        .unwrap();
        let parsed = reqwest::Url::parse(&url).unwrap();
        let q: Vec<(String, String)> =
            parsed.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
        let get = |k: &str| q.iter().filter(|(n, _)| n == k).map(|(_, v)| v.as_str()).collect::<Vec<_>>();
        assert_eq!(get("state"), ["client-1"]);
        assert_eq!(get("redirect_uri"), ["https://sync.example.no/api/oauth2-redirect"]);
        assert_eq!(get("client_id"), ["abc"]);
        assert_eq!(get("code_challenge"), ["c+d"]);
        assert_eq!(get("scope"), ["User.Read"]);
        assert_eq!(parsed.path(), "/organizations/oauth2/v2.0/authorize");
        assert!(get("login_hint").is_empty());

        let added = browser_url("https://login.example/authorize?client_id=abc", "c", &redirect, "  ").unwrap();
        assert!(added.contains("state=c") && added.contains("redirect_uri="));
        assert!(!added.contains("login_hint"));
    }

    #[test]
    fn the_typed_email_goes_to_microsoft_as_the_login_hint() {
        let url = browser_url(
            "https://login.microsoftonline.com/organizations/oauth2/v2.0/authorize?client_id=abc&login_hint=old",
            "c",
            "https://sync.example.no/api/oauth2-redirect",
            " kari+humla@rana.kommune.no ",
        )
        .unwrap();
        assert!(url.contains("login_hint=kari%2Bhumla%40rana.kommune.no"), "{url}");
        let parsed = reqwest::Url::parse(&url).unwrap();
        let hints: Vec<String> =
            parsed.query_pairs().filter(|(k, _)| k == "login_hint").map(|(_, v)| v.into_owned()).collect();
        assert_eq!(hints, ["kari+humla@rana.kommune.no"]);
    }

    #[test]
    fn the_lookup_carries_the_email_encoded_and_none_when_blank() {
        assert_eq!(
            sign_in_methods_url("https://sync.example.no/", "").as_deref(),
            Some("https://sync.example.no/api/humla/sign-in-methods")
        );
        assert_eq!(
            sign_in_methods_url("https://sync.example.no", "   ").as_deref(),
            Some("https://sync.example.no/api/humla/sign-in-methods")
        );
        let url = sign_in_methods_url("https://sync.example.no", " kari+x@rana.kommune.no&y=1 ").unwrap();
        assert_eq!(
            url,
            "https://sync.example.no/api/humla/sign-in-methods?email=kari%2Bx%40rana.kommune.no%26y%3D1"
        );
        let parsed = reqwest::Url::parse(&url).unwrap();
        let q: Vec<(String, String)> =
            parsed.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
        assert_eq!(q, [("email".to_string(), "kari+x@rana.kommune.no&y=1".to_string())]);
        assert_eq!(sign_in_methods_url("not a url", "a@b.no"), None);
    }

    #[test]
    fn microsoft_is_offered_only_when_the_server_says_so() {
        let yes = SignInMethods { microsoft: true };
        let no = SignInMethods { microsoft: false };
        assert_eq!(parse_sign_in_methods(200, &json!({ "microsoft": true })), yes);
        assert_eq!(parse_sign_in_methods(200, &json!({ "microsoft": false })), no);
        assert_eq!(parse_sign_in_methods(200, &json!({ "microsoft": "true" })), no);
        assert_eq!(parse_sign_in_methods(200, &json!({})), no);
        assert_eq!(parse_sign_in_methods(200, &Value::Null), no);
        assert_eq!(parse_sign_in_methods(404, &json!({ "microsoft": true })), no);
        assert_eq!(parse_sign_in_methods(404, &json!({ "message": "The requested resource wasn't found." })), no);
        assert_eq!(parse_sign_in_methods(500, &json!({ "microsoft": true })), no);
    }

    #[test]
    fn only_a_web_url_is_opened() {
        assert_eq!(browser_url("file:///etc/passwd?state=x", "c", "r", ""), None);
        assert_eq!(browser_url("not a url", "c", "r", ""), None);
    }

    #[test]
    fn events_are_read_however_the_stream_splits_them() {
        let stream = "id:client-1\r\nevent:PB_CONNECT\r\ndata:{\"clientId\":\"client-1\"}\r\n\r\n: ping\n\nevent: @oauth2\ndata: {\"state\":\"client-1\",\ndata:\"code\":\"æøå\"}\n\n";
        let bytes = stream.as_bytes();
        for split in 0..bytes.len() {
            let mut parser = SseParser::default();
            let mut events = parser.push(&bytes[..split]);
            events.extend(parser.push(&bytes[split..]));
            assert_eq!(events.len(), 2, "split at {split}");
            assert_eq!(connect_client_id(&events[0]).as_deref(), Some("client-1"));
            assert_eq!(events[1].event, "@oauth2");
        }
        let mut parser = SseParser::default();
        let events = parser.push(stream.as_bytes());
        assert_eq!(events[1].data, "{\"state\":\"client-1\",\n\"code\":\"æøå\"}");
    }

    #[test]
    fn only_the_connect_event_names_the_client() {
        let ev = |event: &str, data: &str| SseEvent { event: event.into(), data: data.into() };
        assert_eq!(connect_client_id(&ev("PB_CONNECT", r#"{"clientId":"abc"}"#)).as_deref(), Some("abc"));
        assert_eq!(connect_client_id(&ev("@oauth2", r#"{"clientId":"abc"}"#)), None);
        assert_eq!(connect_client_id(&ev("PB_CONNECT", r#"{"clientId":""}"#)), None);
        assert_eq!(connect_client_id(&ev("PB_CONNECT", "nope")), None);
    }

    #[test]
    fn the_oauth2_event_counts_only_for_this_client() {
        assert_eq!(oauth2_code(r#"{"state":"c1","code":"xyz"}"#, "c1"), Ok("xyz".into()));
        assert_eq!(oauth2_code(r#"{"state":"c2","code":"xyz"}"#, "c1").unwrap_err().reason, "failed");
        assert_eq!(oauth2_code(r#"{"code":"xyz"}"#, "").unwrap_err().reason, "failed");
        assert_eq!(oauth2_code(r#"{"state":"c1","code":""}"#, "c1").unwrap_err().reason, "failed");
        assert_eq!(
            oauth2_code(r#"{"state":"c1","error":"access_denied"}"#, "c1").unwrap_err().reason,
            "cancelled"
        );
        assert_eq!(
            oauth2_code(r#"{"state":"c1","code":"x","error":"server_error"}"#, "c1").unwrap_err().reason,
            "failed"
        );
        assert_eq!(oauth2_code("garbage", "c1").unwrap_err().reason, "failed");
    }

    #[test]
    fn the_servers_answer_is_signed_in_a_code_step_or_a_short_refusal() {
        let signed_in = parse_oauth2_login(
            200,
            &json!({ "token": "t", "record": { "id": "u1", "email": "kari@kommune.no" }, "meta": { "accessToken": "ms" } }),
        );
        match signed_in {
            Ok(PasswordLogin::SignedIn(s)) => {
                assert_eq!((s.token.as_str(), s.user_id.as_str(), s.email.as_str()), ("t", "u1", "kari@kommune.no"))
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            parse_oauth2_login(401, &json!({ "reason": "two_factor_required", "mfaId": "m1" })),
            Ok(PasswordLogin::TwoFactor { mfa_id: "m1".into() })
        );
        assert_eq!(parse_oauth2_login(401, &json!({ "reason": "two_factor_required" })).unwrap_err().reason, "failed");
        assert_eq!(parse_oauth2_login(200, &json!({})).unwrap_err().reason, "failed");

        let secret = "https://sync.example.no/internal detail";
        for (status, reason, expected) in [
            (403, "entra_tenant_not_enabled", "Your organisation doesn't sign in to Humla with Microsoft."),
            (403, "entra_account_unsupported", "This Microsoft account can't sign in to Humla."),
            (503, "entra_unavailable", "Microsoft didn't answer. Try again shortly."),
            (503, "entra_groups_unavailable", "Microsoft didn't answer. Try again shortly."),
        ] {
            let e = parse_oauth2_login(status, &json!({ "reason": reason, "message": secret })).unwrap_err();
            assert_eq!((e.reason.as_str(), e.message.as_str()), (reason, expected));
        }
        let e = parse_oauth2_login(400, &json!({ "message": secret })).unwrap_err();
        assert_eq!(e.reason, "failed");
        assert!(!e.message.contains("sync.example.no"));
        assert_eq!(parse_oauth2_login(502, &json!({ "message": secret })).unwrap_err().reason, "server_error");
    }
}
