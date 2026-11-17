//! Organisation policies over IPC, and the one refresh of their cache.

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Serialize;
use tauri::{Emitter, State};
use tokio::sync::Notify;

use crate::org_policy::{self, EffectivePolicy, FetchOutcome};
use crate::{db, AppState};

#[derive(Serialize)]
pub struct OrgStatus {
    pub policy: EffectivePolicy,
}

#[tauri::command]
pub fn org_status(state: State<'_, AppState>) -> Result<OrgStatus, String> {
    let conn = state.db.lock();
    let policy = org_policy::effective(&conn).map_err(|e| format!("organisation policy: {e}"))?;
    Ok(OrgStatus { policy })
}

/// How often the worker refreshes the organisation config, and the longest it
/// backs off to while the server can't be reached.
#[cfg_attr(not(feature = "cloud"), allow(dead_code))]
pub(crate) const REFRESH_INTERVAL: Duration = Duration::from_secs(15);
#[cfg_attr(not(feature = "cloud"), allow(dead_code))]
pub(crate) const MAX_BACKOFF: Duration = Duration::from_secs(15 * 60);

/// How one refresh ended, as far as pacing the next one is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Attempt {
    /// The route answered: a config, or a session it won't take.
    Answered,
    /// The server has no such route.
    Unsupported,
    Failed,
}

impl Attempt {
    fn of(outcome: &FetchOutcome) -> Self {
        match outcome {
            FetchOutcome::Fetched(_) | FetchOutcome::SignedOut | FetchOutcome::TwoFactorRequired => {
                Attempt::Answered
            }
            FetchOutcome::Unsupported => Attempt::Unsupported,
            FetchOutcome::Failed(_) => Attempt::Failed,
        }
    }
}

/// What applying one fetch changed, and so which events to send.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Changes {
    /// The policy in force differs (`org_policy_changed`).
    pub policy: bool,
    /// `apply_local_only` rewrote settings (`org_settings_rewritten`).
    pub settings: bool,
}

/// When the last refresh from any caller finished, so a restarted worker
/// doesn't fetch again straight after `cloud_status` did.
static LAST_ATTEMPT: Mutex<Option<Instant>> = Mutex::new(None);
/// The last failure logged, so an outage logs once rather than every round.
static LAST_FAILURE: Mutex<Option<String>> = Mutex::new(None);
/// Wakes a worker loop that stopped on `Unsupported` once the route answers.
static ROUTE_ANSWERED: Notify = Notify::const_new();

/// Fetch the organisation config with a live session and cache it. Emits
/// `org_policy_changed` when the policy in force changed and
/// `org_settings_rewritten` when settings were moved to enforced values.
pub(crate) async fn refresh_org_config(
    db: &Arc<Mutex<rusqlite::Connection>>,
    app: &tauri::AppHandle,
    base: &str,
    token: &str,
) -> Attempt {
    let outcome = org_policy::fetch(base, token).await;
    if outcome == FetchOutcome::SignedOut {
        super::cloud_auth::forget_session_token(token);
    }
    if let Some(line) = failure_to_log(&mut LAST_FAILURE.lock(), &outcome) {
        eprintln!("cloud: {line}");
    }
    let changes = {
        let conn = db.lock();
        apply_fetch(&conn, &outcome)
    };
    if changes.policy {
        let _ = app.emit("org_policy_changed", ());
    }
    if changes.settings {
        let _ = app.emit("org_settings_rewritten", ());
    }
    *LAST_ATTEMPT.lock() = Some(Instant::now());
    let attempt = Attempt::of(&outcome);
    if attempt == Attempt::Answered {
        ROUTE_ANSWERED.notify_waiters();
    }
    attempt
}

/// Cache a fetch and apply `local_only` to settings. Never clears the cache on
/// anything but an authenticated answer (`org_policy::store`).
pub(crate) fn apply_fetch(conn: &rusqlite::Connection, outcome: &FetchOutcome) -> Changes {
    let before = org_policy::effective(conn).ok();
    if let Err(e) = org_policy::store(conn, outcome) {
        eprintln!("cloud: couldn't cache the organisation config: {e:#}");
    }
    let after = org_policy::effective(conn).ok();
    let settings = apply_local_only(conn).unwrap_or_else(|e| {
        eprintln!("org policy: couldn't apply local_only to settings: {e:#}");
        false
    });
    Changes { policy: before != after, settings }
}

/// The line to log for this outcome, or `None` when it repeats the last
/// failure. Anything but a failure resets it.
fn failure_to_log(last: &mut Option<String>, outcome: &FetchOutcome) -> Option<String> {
    let FetchOutcome::Failed(e) = outcome else {
        *last = None;
        return None;
    };
    if last.as_deref() == Some(e.as_str()) {
        return None;
    }
    *last = Some(e.clone());
    Some(e.clone())
}

/// How long a newly started worker waits before its first refresh: the rest of
/// the interval when another caller refreshed within it.
#[cfg_attr(not(feature = "cloud"), allow(dead_code))]
pub(crate) fn first_delay(since_last: Option<Duration>) -> Duration {
    since_last.map_or(Duration::ZERO, |d| REFRESH_INTERVAL.saturating_sub(d))
}

#[cfg_attr(not(feature = "cloud"), allow(dead_code))]
pub(crate) fn since_last_attempt() -> Option<Duration> {
    LAST_ATTEMPT.lock().map(|at| at.elapsed())
}

/// The wait before the next refresh, or `None` to stop until the route answers
/// (`Unsupported`). `attempt` is `None` when there was no session to ask with.
/// Failures double the wait up to [`MAX_BACKOFF`]; anything else resets it.
#[cfg_attr(not(feature = "cloud"), allow(dead_code))]
pub(crate) fn next_delay(previous: Duration, attempt: Option<Attempt>) -> Option<Duration> {
    match attempt {
        Some(Attempt::Unsupported) => None,
        Some(Attempt::Failed) => Some((previous.max(REFRESH_INTERVAL) * 2).min(MAX_BACKOFF)),
        Some(Attempt::Answered) | None => Some(REFRESH_INTERVAL),
    }
}

/// Resolves the next time any refresh hears the route answer.
#[cfg_attr(not(feature = "cloud"), allow(dead_code))]
pub(crate) async fn route_answered() {
    ROUTE_ANSWERED.notified().await;
}

/// Under `local_only`, move each setting the policy refuses to its enforced
/// value, so Settings shows what actually runs and keeps it when the policy is
/// lifted: summaries and chat to the local server, refused transcription slots
/// to on-device Whisper, the MCP switch off. A local server at a non-loopback
/// URL is left for the user to point at this Mac; the call-time checks refuse it
/// meanwhile. Returns whether anything was written.
pub(crate) fn apply_local_only(conn: &rusqlite::Connection) -> anyhow::Result<bool> {
    use crate::providers::ProviderId;
    let policy = org_policy::effective(conn)?;
    if !policy.local_only {
        return Ok(false);
    }
    let mut wrote = false;
    for (key, local) in [("summary_provider", "local"), ("chat_provider", "ollama")] {
        let stored = db::get_setting(conn, key)?.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        let id = ProviderId::parse(stored.as_deref().unwrap_or("openai"));
        let allowed = match id {
            Some(ProviderId::Local) => true,
            Some(id) => super::check_llm_endpoint(conn, id).is_ok(),
            None => false,
        };
        if !allowed {
            db::set_setting(conn, key, local)?;
            wrote = true;
        }
    }
    if let Some(json) = db::get_setting(conn, "transcribe_config")? {
        if let Ok(cfg) = serde_json::from_str::<crate::stt::TranscribeConfig>(&json) {
            let custom = super::transcription_config::custom_base_url(conn);
            let enforced = super::transcription_config::enforce(&cfg, &policy, custom.as_deref());
            if enforced != cfg {
                db::set_setting(conn, "transcribe_config", &serde_json::to_string(&enforced)?)?;
                wrote = true;
            }
        }
    }
    if crate::mcp::is_switched_on(conn) {
        db::set_setting(conn, crate::mcp::SETTING_ENABLED, "false")?;
        wrote = true;
    }
    Ok(wrote)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::ProviderId;

    fn conn() -> (tempfile::TempDir, rusqlite::Connection) {
        let dir = tempfile::tempdir().unwrap();
        let conn = db::open(&dir.path().join("t.sqlite")).unwrap();
        (dir, conn)
    }

    const EMPTY: &str = r#"{"orgs":[]}"#;

    #[test]
    fn the_first_empty_answer_changes_nothing() {
        let (_d, conn) = conn();
        db::set_setting(&conn, "summary_provider", "anthropic").unwrap();
        let changes = apply_fetch(&conn, &org_policy::classify(200, EMPTY));
        assert_eq!(changes, Changes::default());
        assert_eq!(apply_fetch(&conn, &org_policy::classify(200, EMPTY)), Changes::default());
    }

    #[test]
    fn a_policy_that_rewrites_nothing_reports_no_settings_change() {
        let (_d, conn) = conn();
        db::set_setting(&conn, "summary_provider", "local").unwrap();
        db::set_setting(&conn, "chat_provider", "ollama").unwrap();
        let body = r#"{"orgs":[{"id":"o1","name":"Northwind","policy":{"local_only":false,"retention_days":30,"telemetry_off":true,"audio_forbidden":false}}]}"#;
        assert_eq!(
            apply_fetch(&conn, &org_policy::classify(200, body)),
            Changes { policy: true, settings: false }
        );
        let local_only = body.replace(r#""local_only":false"#, r#""local_only":true"#);
        assert_eq!(
            apply_fetch(&conn, &org_policy::classify(200, &local_only)),
            Changes { policy: true, settings: false },
            "summaries and chat are already local"
        );
    }

    #[test]
    fn local_only_arriving_reports_both_changes() {
        let (_d, conn) = conn();
        db::set_setting(&conn, "summary_provider", "anthropic").unwrap();
        let body = r#"{"orgs":[{"id":"o1","name":"Northwind","policy":{"local_only":true,"retention_days":0,"telemetry_off":false,"audio_forbidden":false}}]}"#;
        assert_eq!(
            apply_fetch(&conn, &org_policy::classify(200, body)),
            Changes { policy: true, settings: true }
        );
    }

    #[test]
    fn failures_never_change_anything() {
        let (_d, conn) = conn();
        org_policy::cache_local_only(&conn, "Northwind");
        apply_local_only(&conn).unwrap();
        for outcome in [
            FetchOutcome::Unsupported,
            FetchOutcome::Failed("offline".into()),
            FetchOutcome::SignedOut,
            FetchOutcome::TwoFactorRequired,
        ] {
            assert_eq!(apply_fetch(&conn, &outcome), Changes::default(), "{outcome:?}");
            assert!(org_policy::effective(&conn).unwrap().local_only, "{outcome:?}");
        }
    }

    #[test]
    fn failures_back_off_to_fifteen_minutes_and_reset_on_an_answer() {
        let mut delay = REFRESH_INTERVAL;
        let mut seen = vec![];
        for _ in 0..8 {
            delay = next_delay(delay, Some(Attempt::Failed)).unwrap();
            seen.push(delay.as_secs());
        }
        assert_eq!(seen, [30, 60, 120, 240, 480, 900, 900, 900]);
        assert_eq!(next_delay(delay, Some(Attempt::Answered)), Some(REFRESH_INTERVAL));
        assert_eq!(next_delay(delay, None), Some(REFRESH_INTERVAL), "no session");
        assert_eq!(next_delay(Duration::from_secs(3), Some(Attempt::Failed)), Some(Duration::from_secs(30)));
    }

    #[test]
    fn an_unsupported_server_stops_the_loop() {
        assert_eq!(next_delay(REFRESH_INTERVAL, Some(Attempt::Unsupported)), None);
        assert_eq!(Attempt::of(&FetchOutcome::Unsupported), Attempt::Unsupported);
        assert_eq!(Attempt::of(&FetchOutcome::SignedOut), Attempt::Answered);
    }

    #[test]
    fn a_restarted_worker_waits_out_a_recent_refresh() {
        assert_eq!(first_delay(None), Duration::ZERO);
        assert_eq!(first_delay(Some(Duration::from_secs(5))), Duration::from_secs(10));
        assert_eq!(first_delay(Some(Duration::from_secs(60))), Duration::ZERO);
    }

    #[test]
    fn a_repeated_failure_logs_once() {
        let mut last = None;
        let offline = FetchOutcome::Failed("offline".into());
        assert_eq!(failure_to_log(&mut last, &offline).as_deref(), Some("offline"));
        assert_eq!(failure_to_log(&mut last, &offline), None);
        let http = FetchOutcome::Failed("organisation config: HTTP 502".into());
        assert!(failure_to_log(&mut last, &http).is_some());
        assert_eq!(failure_to_log(&mut last, &FetchOutcome::Unsupported), None);
        assert!(failure_to_log(&mut last, &http).is_some(), "an answer in between resets it");
    }

    #[test]
    fn nothing_moves_without_local_only() {
        let (_d, conn) = conn();
        db::set_setting(&conn, "summary_provider", "anthropic").unwrap();
        db::set_setting(&conn, crate::mcp::SETTING_ENABLED, "true").unwrap();
        assert!(!apply_local_only(&conn).unwrap());
        assert_eq!(db::get_setting(&conn, "summary_provider").unwrap().as_deref(), Some("anthropic"));
    }

    #[test]
    fn local_only_moves_refused_settings_to_their_enforced_values() {
        let (_d, conn) = conn();
        db::set_setting(&conn, "summary_provider", "anthropic").unwrap();
        db::set_setting(&conn, crate::mcp::SETTING_ENABLED, "true").unwrap();
        let mut cfg = crate::stt::TranscribeConfig::default_fallback();
        cfg.per_language.insert("no".into(), crate::stt::from_legacy_settings(Some("local"), None, None, None, None));
        db::set_setting(&conn, "transcribe_config", &serde_json::to_string(&cfg).unwrap()).unwrap();
        org_policy::cache_local_only(&conn, "Northwind");

        assert!(apply_local_only(&conn).unwrap());
        let get = |k: &str| db::get_setting(&conn, k).unwrap();
        assert_eq!(get("summary_provider").as_deref(), Some("local"));
        assert_eq!(get("chat_provider").as_deref(), Some("ollama"), "unset means openai");
        assert_eq!(get(crate::mcp::SETTING_ENABLED).as_deref(), Some("false"));
        let cfg: crate::stt::TranscribeConfig = serde_json::from_str(&get("transcribe_config").unwrap()).unwrap();
        assert_eq!(cfg.default.provider(), ProviderId::Local);
        assert!(!apply_local_only(&conn).unwrap(), "settled settings stay as they are");
    }

    #[test]
    fn an_unreadable_cache_writes_nothing() {
        let (_d, conn) = conn();
        db::set_setting(&conn, "summary_provider", "anthropic").unwrap();
        db::set_setting(&conn, crate::mcp::SETTING_ENABLED, "true").unwrap();
        db::set_setting(&conn, org_policy::SETTING_ORG_CONFIG, "{not json").unwrap();
        assert!(apply_local_only(&conn).is_err());
        assert_eq!(db::get_setting(&conn, "summary_provider").unwrap().as_deref(), Some("anthropic"));
        assert_eq!(db::get_setting(&conn, crate::mcp::SETTING_ENABLED).unwrap().as_deref(), Some("true"));
    }

    #[test]
    fn a_loopback_custom_endpoint_is_kept() {
        let (_d, conn) = conn();
        db::set_setting(&conn, "summary_provider", "custom").unwrap();
        db::set_setting(&conn, "chat_provider", "custom").unwrap();
        db::set_setting(&conn, "custom_base_url", "http://localhost:1234/v1").unwrap();
        org_policy::cache_local_only(&conn, "Northwind");
        apply_local_only(&conn).unwrap();
        assert_eq!(db::get_setting(&conn, "summary_provider").unwrap().as_deref(), Some("custom"));
        assert_eq!(db::get_setting(&conn, "chat_provider").unwrap().as_deref(), Some("custom"));
    }
}
