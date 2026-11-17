//! Signing in to GitHub for Copilot: the OAuth device flow under Humla's own
//! OAuth app, and keeping its token fresh. The token is a GitHub user token,
//! which the Copilot CLI exchanges for Copilot access itself. It expires after
//! 8 h and refreshes for 6 months without a client secret, because it came from
//! the device flow.

use serde::{Deserialize, Serialize};

/// Humla's OAuth app. Public by design: the device flow has no secret.
pub const CLIENT_ID: &str = "Ov23li0tjvtZ2DAfOFUu";
pub const GITHUB: &str = "https://github.com";

/// Refresh this long before the access token runs out, so a summary that
/// starts on a token with seconds left doesn't fail half-way.
const REFRESH_MARGIN_MS: i64 = 5 * 60 * 1000;

/// What the Keychain holds (`copilot_credentials`, as JSON).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Credentials {
    pub login: String,
    pub access_token: String,
    /// Epoch ms. `None` for a token that doesn't expire.
    pub access_expires_at: Option<i64>,
    pub refresh_token: Option<String>,
    pub refresh_expires_at: Option<i64>,
}

/// The first answer of the device flow: the code the user types at
/// `verification_uri`.
#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

/// A token, as `login/oauth/access_token` grants it to either grant type.
#[derive(Clone, Debug, PartialEq)]
pub struct Grant {
    pub access_token: String,
    pub access_expires_at: Option<i64>,
    pub refresh_token: Option<String>,
    pub refresh_expires_at: Option<i64>,
}

/// One poll of the device flow.
#[derive(Clone, Debug, PartialEq)]
pub enum Poll {
    Pending,
    /// GitHub asks for a longer interval; the new one, in seconds.
    SlowDown(u64),
    Granted(Grant),
    Denied,
    Expired,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AuthFailure {
    /// GitHub answered and said no: the grant is gone.
    Rejected(String),
    /// No answer worth acting on: offline, 5xx, a page that isn't JSON.
    Unreachable(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum TokenError {
    /// Nothing usable is left; the user has to sign in again.
    SignedOut,
    Unreachable(String),
}

/// Read an answer from `login/oauth/access_token`. GitHub reports device-flow
/// states as 200s with an `error` field, so the status code says little.
pub fn parse_token_response(json: &serde_json::Value, now_ms: i64) -> Result<Poll, AuthFailure> {
    if let Some(token) = json.get("access_token").and_then(|v| v.as_str()) {
        let secs = |key: &str| json.get(key).and_then(|v| v.as_i64()).filter(|s| *s > 0);
        return Ok(Poll::Granted(Grant {
            access_token: token.to_string(),
            access_expires_at: secs("expires_in").map(|s| now_ms + s * 1000),
            refresh_token: json
                .get("refresh_token")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            refresh_expires_at: secs("refresh_token_expires_in").map(|s| now_ms + s * 1000),
        }));
    }
    let error = json.get("error").and_then(|v| v.as_str()).unwrap_or_default();
    match error {
        "authorization_pending" => Ok(Poll::Pending),
        "slow_down" => Ok(Poll::SlowDown(
            json.get("interval").and_then(|v| v.as_u64()).unwrap_or(10),
        )),
        "access_denied" => Ok(Poll::Denied),
        "expired_token" => Ok(Poll::Expired),
        "" => Err(AuthFailure::Unreachable("GitHub sent an answer Humla can't read.".into())),
        other => Err(AuthFailure::Rejected(
            json.get("error_description")
                .and_then(|v| v.as_str())
                .unwrap_or(other)
                .to_string(),
        )),
    }
}

/// What keeping a token fresh asks of GitHub.
pub(crate) trait OAuthServer {
    async fn refresh(&self, refresh_token: &str) -> Result<Grant, AuthFailure>;
}

/// An access token good for at least [`REFRESH_MARGIN_MS`], refreshing it when
/// it isn't. `creds` comes back holding the refreshed grant for the caller to
/// store; a refresh GitHub rejects empties it, which signs the user out.
pub(crate) async fn fresh_token(
    server: &impl OAuthServer,
    creds: &mut Option<Credentials>,
    now_ms: i64,
) -> Result<String, TokenError> {
    let Some(current) = creds.as_ref() else {
        return Err(TokenError::SignedOut);
    };
    let usable = |at: Option<i64>| at.is_none_or(|at| at - REFRESH_MARGIN_MS > now_ms);
    if usable(current.access_expires_at) {
        return Ok(current.access_token.clone());
    }
    let refresh = current
        .refresh_token
        .clone()
        .filter(|_| current.refresh_expires_at.is_none_or(|at| at > now_ms));
    let Some(refresh) = refresh else {
        *creds = None;
        return Err(TokenError::SignedOut);
    };
    match server.refresh(&refresh).await {
        Ok(grant) => {
            let login = current.login.clone();
            let token = grant.access_token.clone();
            *creds = Some(Credentials {
                login,
                access_token: grant.access_token,
                access_expires_at: grant.access_expires_at,
                // GitHub rotates the refresh token; keep the old one only if
                // it sent none.
                refresh_token: grant.refresh_token.or(Some(refresh)),
                refresh_expires_at: grant.refresh_expires_at.or(current.refresh_expires_at),
            });
            Ok(token)
        }
        Err(AuthFailure::Rejected(_)) => {
            *creds = None;
            Err(TokenError::SignedOut)
        }
        Err(AuthFailure::Unreachable(e)) => Err(TokenError::Unreachable(e)),
    }
}

// ---- GitHub over HTTP --------------------------------------------------------

/// Honest about who is calling: GitHub rate-limits and audits by it.
pub fn user_agent() -> String {
    format!("Humla/{}", env!("CARGO_PKG_VERSION"))
}

fn client() -> reqwest::Client {
    crate::http::client_builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("reqwest client")
}

fn unreachable(e: reqwest::Error) -> AuthFailure {
    AuthFailure::Unreachable(format!("Couldn't reach GitHub: {e}"))
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub async fn request_device_code() -> Result<DeviceCode, AuthFailure> {
    let r = client()
        .post(format!("{GITHUB}/login/device/code"))
        .header("Accept", "application/json")
        .header("User-Agent", user_agent())
        .form(&[("client_id", CLIENT_ID), ("scope", "read:user")])
        .send()
        .await
        .map_err(unreachable)?;
    let json: serde_json::Value = r.json().await.map_err(unreachable)?;
    serde_json::from_value(json.clone()).map_err(|_| {
        AuthFailure::Rejected(
            json.get("error_description")
                .and_then(|v| v.as_str())
                .unwrap_or("GitHub refused to start the sign-in.")
                .to_string(),
        )
    })
}

async fn token_request(form: &[(&str, &str)]) -> Result<Poll, AuthFailure> {
    let r = client()
        .post(format!("{GITHUB}/login/oauth/access_token"))
        .header("Accept", "application/json")
        .header("User-Agent", user_agent())
        .form(form)
        .send()
        .await
        .map_err(unreachable)?;
    if r.status().is_server_error() {
        return Err(AuthFailure::Unreachable(format!("GitHub answered {}", r.status())));
    }
    let json: serde_json::Value = r.json().await.map_err(unreachable)?;
    parse_token_response(&json, now_ms())
}

pub async fn poll_device_code(device_code: &str) -> Result<Poll, AuthFailure> {
    token_request(&[
        ("client_id", CLIENT_ID),
        ("device_code", device_code),
        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
    ])
    .await
}

/// The signed-in account's login, shown in Settings.
pub async fn fetch_login(access_token: &str) -> Result<String, AuthFailure> {
    let r = client()
        .get("https://api.github.com/user")
        .header("Authorization", format!("Bearer {access_token}"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", user_agent())
        .send()
        .await
        .map_err(unreachable)?;
    if r.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(AuthFailure::Rejected("GitHub didn't accept the new token.".into()));
    }
    let json: serde_json::Value = r.json().await.map_err(unreachable)?;
    json.get("login")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| AuthFailure::Unreachable("GitHub sent no account name.".into()))
}

pub struct GitHub;

impl OAuthServer for GitHub {
    async fn refresh(&self, refresh_token: &str) -> Result<Grant, AuthFailure> {
        match token_request(&[
            ("client_id", CLIENT_ID),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ])
        .await?
        {
            Poll::Granted(grant) => Ok(grant),
            other => Err(AuthFailure::Rejected(format!("unexpected refresh answer: {other:?}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    const NOW: i64 = 1_000_000_000;
    const HOUR: i64 = 3_600_000;

    fn creds(access_expires_at: Option<i64>, refresh: Option<(&str, i64)>) -> Credentials {
        Credentials {
            login: "octo".into(),
            access_token: "old".into(),
            access_expires_at,
            refresh_token: refresh.map(|(t, _)| t.to_string()),
            refresh_expires_at: refresh.map(|(_, at)| at),
        }
    }

    struct FakeServer {
        answer: Result<Grant, AuthFailure>,
        calls: Mutex<Vec<String>>,
    }

    impl FakeServer {
        fn new(answer: Result<Grant, AuthFailure>) -> Self {
            Self { answer, calls: Mutex::new(Vec::new()) }
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl OAuthServer for FakeServer {
        async fn refresh(&self, refresh_token: &str) -> Result<Grant, AuthFailure> {
            self.calls.lock().unwrap().push(refresh_token.to_string());
            self.answer.clone()
        }
    }

    fn grant(token: &str, refresh: Option<&str>) -> Grant {
        Grant {
            access_token: token.into(),
            access_expires_at: Some(NOW + 8 * HOUR),
            refresh_token: refresh.map(str::to_string),
            refresh_expires_at: Some(NOW + 4000 * HOUR),
        }
    }

    #[test]
    fn a_granted_token_carries_both_expiries() {
        let poll = parse_token_response(
            &json!({
                "access_token": "ghu_x", "expires_in": 28800,
                "refresh_token": "ghr_y", "refresh_token_expires_in": 15897600,
                "token_type": "bearer", "scope": ""
            }),
            NOW,
        )
        .unwrap();
        assert_eq!(
            poll,
            Poll::Granted(Grant {
                access_token: "ghu_x".into(),
                access_expires_at: Some(NOW + 28_800_000),
                refresh_token: Some("ghr_y".into()),
                refresh_expires_at: Some(NOW + 15_897_600_000),
            })
        );
    }

    #[test]
    fn a_token_without_expiry_never_expires() {
        let poll = parse_token_response(&json!({ "access_token": "gho_x", "scope": "read:user" }), NOW);
        assert_eq!(
            poll,
            Ok(Poll::Granted(Grant {
                access_token: "gho_x".into(),
                access_expires_at: None,
                refresh_token: None,
                refresh_expires_at: None,
            }))
        );
    }

    #[test]
    fn device_flow_states_are_read_from_the_error_field() {
        let read = |v| parse_token_response(&v, NOW);
        assert_eq!(read(json!({ "error": "authorization_pending" })), Ok(Poll::Pending));
        assert_eq!(read(json!({ "error": "slow_down", "interval": 10 })), Ok(Poll::SlowDown(10)));
        assert_eq!(read(json!({ "error": "access_denied" })), Ok(Poll::Denied));
        assert_eq!(read(json!({ "error": "expired_token" })), Ok(Poll::Expired));
    }

    #[test]
    fn a_refused_refresh_is_rejected_and_garbage_is_unreachable() {
        assert_eq!(
            parse_token_response(
                &json!({ "error": "bad_refresh_token", "error_description": "The refresh token passed is incorrect or expired." }),
                NOW
            ),
            Err(AuthFailure::Rejected("The refresh token passed is incorrect or expired.".into()))
        );
        assert!(matches!(
            parse_token_response(&json!({ "message": "Not Found" }), NOW),
            Err(AuthFailure::Unreachable(_))
        ));
    }

    #[tokio::test]
    async fn a_token_with_time_left_is_used_as_it_is() {
        let server = FakeServer::new(Err(AuthFailure::Unreachable("not called".into())));
        let mut stored = Some(creds(Some(NOW + HOUR), Some(("ghr", NOW + 1000 * HOUR))));
        assert_eq!(fresh_token(&server, &mut stored, NOW).await, Ok("old".into()));
        assert!(server.calls().is_empty());
    }

    #[tokio::test]
    async fn a_token_about_to_expire_is_refreshed_and_the_new_refresh_token_kept() {
        let server = FakeServer::new(Ok(grant("new", Some("ghr_new"))));
        let mut stored = Some(creds(Some(NOW + 60_000), Some(("ghr_old", NOW + 1000 * HOUR))));

        assert_eq!(fresh_token(&server, &mut stored, NOW).await, Ok("new".into()));

        assert_eq!(server.calls(), vec!["ghr_old"]);
        let stored = stored.unwrap();
        assert_eq!(stored.access_token, "new");
        assert_eq!(stored.refresh_token.as_deref(), Some("ghr_new"));
        assert_eq!(stored.login, "octo");
    }

    #[tokio::test]
    async fn a_rejected_refresh_signs_the_user_out() {
        let server = FakeServer::new(Err(AuthFailure::Rejected("bad_refresh_token".into())));
        let mut stored = Some(creds(Some(NOW - HOUR), Some(("ghr", NOW + 1000 * HOUR))));
        assert_eq!(fresh_token(&server, &mut stored, NOW).await, Err(TokenError::SignedOut));
        assert_eq!(stored, None);
    }

    #[tokio::test]
    async fn an_unreachable_github_keeps_the_credentials() {
        let server = FakeServer::new(Err(AuthFailure::Unreachable("offline".into())));
        let before = creds(Some(NOW - HOUR), Some(("ghr", NOW + 1000 * HOUR)));
        let mut stored = Some(before.clone());
        assert_eq!(
            fresh_token(&server, &mut stored, NOW).await,
            Err(TokenError::Unreachable("offline".into()))
        );
        assert_eq!(stored, Some(before));
    }

    #[tokio::test]
    async fn an_expired_refresh_token_signs_out_without_asking_github() {
        let server = FakeServer::new(Ok(grant("new", None)));
        let mut stored = Some(creds(Some(NOW - HOUR), Some(("ghr", NOW - 1))));
        assert_eq!(fresh_token(&server, &mut stored, NOW).await, Err(TokenError::SignedOut));
        assert!(server.calls().is_empty());
        assert_eq!(stored, None);
    }

    #[tokio::test]
    async fn nothing_stored_is_signed_out() {
        let server = FakeServer::new(Ok(grant("new", None)));
        assert_eq!(fresh_token(&server, &mut None, NOW).await, Err(TokenError::SignedOut));
    }
}
