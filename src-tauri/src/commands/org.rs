//! Organisation policies over IPC, and the one refresh of their cache.

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Serialize;
use tauri::{Emitter, State};
use tokio::sync::Notify;

use crate::org_policy::{self, AudioSweep, EffectivePolicy, FetchOutcome, OrgSummary};
use crate::{db, AppState};

#[derive(Serialize)]
pub struct OrgStatus {
    pub policy: EffectivePolicy,
    /// The organisations whose templates and terms apply here.
    pub orgs: Vec<OrgSummary>,
}

#[tauri::command]
pub fn org_status(state: State<'_, AppState>) -> Result<OrgStatus, String> {
    let conn = state.db.lock();
    let policy = org_policy::effective(&conn).map_err(|e| format!("organisation policy: {e}"))?;
    let config = org_policy::cached(&conn).map_err(|e| format!("organisation policy: {e}"))?;
    Ok(OrgStatus { policy, orgs: org_policy::summaries(config.as_ref()) })
}

/// The organisation whose `audio_forbidden` still owes the sweep of audio on
/// this Mac, for the one-time notice.
#[tauri::command]
pub fn org_audio_notice(state: State<'_, AppState>) -> Option<AudioSweep> {
    org_policy::audio_sweep_due(&state.db.lock())
}

/// Delete every note's stored audio for `org_id`'s `audio_forbidden` and record
/// that it ran. Refused unless that organisation's sweep is due right now.
#[tauri::command]
pub fn org_audio_sweep(app: tauri::AppHandle, state: State<'_, AppState>, org_id: String) -> Result<usize, String> {
    authorise_audio_sweep(&state.db.lock(), &org_id)?;
    let removed = super::delete_stored_audio(app)?;
    org_policy::record_audio_sweep(&state.db.lock(), &org_id).map_err(|e| format!("organisation policy: {e}"))?;
    Ok(removed)
}

fn authorise_audio_sweep(conn: &rusqlite::Connection, org_id: &str) -> Result<(), String> {
    match org_policy::audio_sweep_due(conn) {
        Some(due) if due.org_id == org_id => Ok(()),
        _ => Err("No organisation asks for stored audio to be deleted.".into()),
    }
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
    /// `apply_local_only` or `apply_device_policies` rewrote settings
    /// (`org_settings_rewritten`).
    pub settings: bool,
    /// The organisations' templates, terms or names differ
    /// (`org_content_changed`).
    pub content: bool,
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
    if changes.content {
        let _ = app.emit("org_content_changed", ());
    }
    *LAST_ATTEMPT.lock() = Some(Instant::now());
    let attempt = Attempt::of(&outcome);
    if attempt == Attempt::Answered {
        ROUTE_ANSWERED.notify_waiters();
    }
    attempt
}

/// Cache a fetch and move settings the policy refuses to their enforced values. Never clears the cache on
/// anything but an authenticated answer (`org_policy::store`).
pub(crate) fn apply_fetch(conn: &rusqlite::Connection, outcome: &FetchOutcome) -> Changes {
    let before = org_policy::effective(conn).ok();
    let orgs_before = org_policy::cached(conn).ok().map(|c| org_policy::summaries(c.as_ref()));
    let stored = org_policy::store(conn, outcome).inspect_err(|e| {
        eprintln!("cloud: couldn't cache the organisation config: {e:#}");
    });
    let after = org_policy::effective(conn).ok();
    let orgs_after = org_policy::cached(conn).ok().map(|c| org_policy::summaries(c.as_ref()));
    let templates = match (outcome, stored) {
        (FetchOutcome::Fetched(config), Ok(_)) => org_policy::apply_templates(conn, config).unwrap_or_else(|e| {
            eprintln!("org policy: couldn't cache the organisation's templates: {e:#}");
            false
        }),
        _ => false,
    };
    let local_only = apply_local_only(conn).unwrap_or_else(|e| {
        eprintln!("org policy: couldn't apply local_only to settings: {e:#}");
        false
    });
    let device = apply_device_policies(conn).unwrap_or_else(|e| {
        eprintln!("org policy: couldn't apply audio and telemetry policies to settings: {e:#}");
        false
    });
    let settings = local_only || device;
    Changes { policy: before != after, settings, content: templates || orgs_before != orgs_after }
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

/// Under `audio_forbidden`, turn `keep_audio` off; under `telemetry_off`, turn
/// `telemetry_enabled` off. Only an explicit "true" is rewritten: unset already
/// means off for both. Returns whether anything was written.
pub(crate) fn apply_device_policies(conn: &rusqlite::Connection) -> anyhow::Result<bool> {
    let policy = org_policy::effective(conn)?;
    let mut wrote = false;
    for (on, key) in [(policy.audio_forbidden, "keep_audio"), (policy.telemetry_off, "telemetry_enabled")] {
        if on && db::get_setting(conn, key)?.as_deref() == Some("true") {
            db::set_setting(conn, key, "false")?;
            wrote = true;
        }
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

    const WITH_TEMPLATE: &str = r#"{"orgs":[{"id":"o1","name":"Northwind","policy":{"local_only":false,"retention_days":0,"telemetry_off":false,"audio_forbidden":false},"vocabulary":["Northwind"],"templates":[{"client_id":"t1","name":"Case meeting","content":"Decisions.","workspaces":[],"deleted":false}]}]}"#;

    fn prompts(conn: &rusqlite::Connection) -> Vec<db::SummaryPrompt> {
        db::list_summary_prompts(conn, "").unwrap()
    }

    #[test]
    fn without_an_organisation_no_template_rows_appear() {
        let (_d, conn) = conn();
        let mine = db::create_summary_prompt(&conn, "Mine", "My text", "").unwrap();
        let before = prompts(&conn);
        apply_fetch(&conn, &org_policy::classify(200, EMPTY));
        apply_fetch(&conn, &FetchOutcome::Failed("offline".into()));
        let after = prompts(&conn);
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].id, mine.id);
        assert_eq!(after[0].updated_at, before[0].updated_at);
    }

    #[test]
    fn templates_arrive_and_leave_with_the_config() {
        let (_d, conn) = conn();
        let arrived = apply_fetch(&conn, &org_policy::classify(200, WITH_TEMPLATE));
        assert!(arrived.content && !arrived.policy);
        assert_eq!(prompts(&conn)[0].org_id, "o1");
        assert_eq!(apply_fetch(&conn, &org_policy::classify(200, WITH_TEMPLATE)), Changes::default());
        for outcome in [FetchOutcome::SignedOut, FetchOutcome::Failed("offline".into()), FetchOutcome::Unsupported] {
            assert_eq!(apply_fetch(&conn, &outcome), Changes::default(), "{outcome:?}");
            assert_eq!(prompts(&conn).len(), 1, "{outcome:?}");
        }
        let edited = WITH_TEMPLATE.replace(r#""vocabulary":["Northwind"]"#, r#""vocabulary":["Northwind","Fjord"]"#);
        assert!(apply_fetch(&conn, &org_policy::classify(200, &edited)).content, "terms changed");
        let released = apply_fetch(&conn, &org_policy::classify(200, EMPTY));
        assert!(released.content);
        assert!(prompts(&conn).is_empty());
    }

    #[test]
    fn a_policy_that_rewrites_nothing_reports_no_settings_change() {
        let (_d, conn) = conn();
        db::set_setting(&conn, "summary_provider", "local").unwrap();
        db::set_setting(&conn, "chat_provider", "ollama").unwrap();
        let body = r#"{"orgs":[{"id":"o1","name":"Northwind","policy":{"local_only":false,"retention_days":30,"telemetry_off":true,"audio_forbidden":false}}]}"#;
        assert_eq!(
            apply_fetch(&conn, &org_policy::classify(200, body)),
            Changes { policy: true, settings: false, content: true }
        );
        let local_only = body.replace(r#""local_only":false"#, r#""local_only":true"#);
        assert_eq!(
            apply_fetch(&conn, &org_policy::classify(200, &local_only)),
            Changes { policy: true, settings: false, content: false },
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
            Changes { policy: true, settings: true, content: true }
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
    fn device_body(audio_forbidden: bool, telemetry_off: bool) -> String {
        format!(
            r#"{{"orgs":[{{"id":"o1","name":"Northwind","policy":{{"local_only":false,"retention_days":0,"telemetry_off":{telemetry_off},"audio_forbidden":{audio_forbidden}}}}}]}}"#
        )
    }

    fn get(conn: &rusqlite::Connection, key: &str) -> Option<String> {
        db::get_setting(conn, key).unwrap()
    }

    #[test]
    fn without_an_organisation_audio_and_telemetry_settings_are_untouched() {
        for (keep, telemetry) in [(Some("true"), Some("true")), (Some("false"), Some("false")), (None, None)] {
            let (_d, conn) = conn();
            if let Some(v) = keep {
                db::set_setting(&conn, "keep_audio", v).unwrap();
            }
            if let Some(v) = telemetry {
                db::set_setting(&conn, "telemetry_enabled", v).unwrap();
            }
            for outcome in [
                org_policy::classify(200, EMPTY),
                org_policy::classify(200, EMPTY),
                FetchOutcome::Failed("offline".into()),
                FetchOutcome::Unsupported,
                FetchOutcome::SignedOut,
            ] {
                assert_eq!(apply_fetch(&conn, &outcome), Changes::default(), "{outcome:?}");
            }
            assert!(!apply_device_policies(&conn).unwrap());
            assert_eq!(get(&conn, "keep_audio").as_deref(), keep);
            assert_eq!(get(&conn, "telemetry_enabled").as_deref(), telemetry);
            assert_eq!(org_policy::audio_sweep_due(&conn), None);
            assert!(authorise_audio_sweep(&conn, "o1").is_err());
            assert!(authorise_audio_sweep(&conn, "").is_err());
        }
    }

    #[test]
    fn audio_forbidden_arriving_turns_keep_audio_off_once() {
        let (_d, conn) = conn();
        db::set_setting(&conn, "keep_audio", "true").unwrap();
        db::set_setting(&conn, "transcribe_manually", "true").unwrap();
        assert_eq!(
            apply_fetch(&conn, &org_policy::classify(200, &device_body(true, false))),
            Changes { policy: true, settings: true, content: true }
        );
        assert_eq!(get(&conn, "keep_audio").as_deref(), Some("false"));
        assert_eq!(get(&conn, "transcribe_manually").as_deref(), Some("true"), "the closed gate defers nothing");
        assert_eq!(apply_fetch(&conn, &org_policy::classify(200, &device_body(true, false))), Changes::default());
        assert!(authorise_audio_sweep(&conn, "o1").is_ok());
        assert!(authorise_audio_sweep(&conn, "o2").is_err());
    }

    #[test]
    fn telemetry_off_arriving_turns_telemetry_off_and_leaves_unset_alone() {
        let (_d, conn) = conn();
        db::set_setting(&conn, "telemetry_enabled", "true").unwrap();
        assert!(apply_fetch(&conn, &org_policy::classify(200, &device_body(false, true))).settings);
        assert_eq!(get(&conn, "telemetry_enabled").as_deref(), Some("false"));

        let (_d2, unset) = super::tests::conn();
        assert!(!apply_fetch(&unset, &org_policy::classify(200, &device_body(false, true))).settings);
        assert_eq!(get(&unset, "telemetry_enabled"), None);
    }

    #[test]
    fn an_unreadable_cache_rewrites_nothing_and_authorises_no_sweep() {
        let (_d, conn) = conn();
        db::set_setting(&conn, "keep_audio", "true").unwrap();
        db::set_setting(&conn, "telemetry_enabled", "true").unwrap();
        db::set_setting(&conn, org_policy::SETTING_ORG_CONFIG, "{not json").unwrap();
        assert!(apply_device_policies(&conn).is_err());
        assert_eq!(get(&conn, "keep_audio").as_deref(), Some("true"));
        assert_eq!(get(&conn, "telemetry_enabled").as_deref(), Some("true"));
        assert!(authorise_audio_sweep(&conn, "o1").is_err());
        assert!(authorise_audio_sweep(&conn, "").is_err());
    }

    #[test]
    fn a_swept_organisation_authorises_no_second_sweep() {
        let (_d, conn) = conn();
        apply_fetch(&conn, &org_policy::classify(200, &device_body(true, false)));
        org_policy::record_audio_sweep(&conn, "o1").unwrap();
        assert!(authorise_audio_sweep(&conn, "o1").is_err());
    }
}
