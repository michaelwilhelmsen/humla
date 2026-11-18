//! Organisation policies over IPC, and the one refresh of their cache.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Serialize;
use tauri::{Emitter, Manager, State};
use tokio::sync::Notify;

use crate::org_policy::{self, AudioSweep, EffectivePolicy, FetchOutcome, OrgSummary};
use crate::{db, AppState};

#[derive(Serialize)]
pub struct OrgStatus {
    pub policy: EffectivePolicy,
    /// The organisations whose templates and terms apply here.
    pub orgs: Vec<OrgSummary>,
    /// Days of retention per workspace (`org_policy::retention_scope`).
    pub retention: BTreeMap<String, u32>,
    /// Each organisation workspace, with whether opening a note there is
    /// recorded (`org_audit::scope`).
    pub audit: BTreeMap<String, bool>,
}

#[tauri::command]
pub fn org_status(state: State<'_, AppState>) -> Result<OrgStatus, String> {
    let conn = state.db.lock();
    let policy = org_policy::effective(&conn).map_err(|e| format!("organisation policy: {e}"))?;
    let config = org_policy::cached(&conn).map_err(|e| format!("organisation policy: {e}"))?;
    let retention = config.as_ref().map(org_policy::retention_scope).unwrap_or_default();
    let audit = config.as_ref().map(crate::org_audit::scope).unwrap_or_default();
    Ok(OrgStatus { policy, orgs: org_policy::summaries(config.as_ref()), retention, audit })
}

/// A note was opened in the app: queued for its organisation's audit log under
/// `audit_reads`, once per note per few minutes.
#[tauri::command]
pub fn audit_note_opened(state: State<'_, AppState>, note_id: String) -> Result<(), String> {
    audit_note(&state, &note_id, crate::org_audit::Action::Open)
}

/// A panel's Copy button copied the note's text.
#[tauri::command]
pub fn audit_note_copied(state: State<'_, AppState>, note_id: String) -> Result<(), String> {
    audit_note(&state, &note_id, crate::org_audit::Action::Export(crate::org_audit::Format::Copy))
}

fn audit_note(state: &State<'_, AppState>, note_id: &str, action: crate::org_audit::Action) -> Result<(), String> {
    let conn = state.db.lock();
    let note = db::get_note(&conn, note_id).map_err(|e| e.to_string())?;
    crate::org_audit::record(&conn, action, &note.workspace_id, &note.id);
    Ok(())
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
    /// Which workspaces retention covers, or for how long, differs: sweep now.
    pub retention: bool,
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
    if changes.policy || changes.retention {
        let _ = app.emit("org_policy_changed", ());
    }
    if changes.settings {
        let _ = app.emit("org_settings_rewritten", ());
    }
    if changes.content {
        let _ = app.emit("org_content_changed", ());
    }
    if changes.retention {
        let (db, app) = (db.clone(), app.clone());
        let _ = tauri::async_runtime::spawn_blocking(move || org_retention_sweep(&app, &db)).await;
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
    let retention_before = org_policy::retention_in_force(conn);
    let orgs_before = org_policy::cached(conn).ok().map(|c| org_policy::summaries(c.as_ref()));
    let audit_before = crate::org_audit::scope_in_force(conn);
    let stored = org_policy::store(conn, outcome).inspect_err(|e| {
        eprintln!("cloud: couldn't cache the organisation config: {e:#}");
    });
    if let (FetchOutcome::Fetched(config), Ok(true)) = (outcome, &stored) {
        if config.orgs.is_empty() {
            release_audit_queue(conn);
        }
    }
    let after = org_policy::effective(conn).ok();
    let orgs_after = org_policy::cached(conn).ok().map(|c| org_policy::summaries(c.as_ref()));
    let templates = match (outcome, stored) {
        (FetchOutcome::Fetched(config), Ok(_)) => org_policy::apply_templates(conn, config).unwrap_or_else(|e| {
            eprintln!("org policy: couldn't cache the organisation's templates: {e:#}");
            false
        }),
        _ => false,
    };
    let settings = apply_policy_to_settings(conn);
    Changes {
        policy: before != after || audit_before != crate::org_audit::scope_in_force(conn),
        settings,
        content: templates || orgs_before != orgs_after,
        retention: retention_before != org_policy::retention_in_force(conn),
    }
}

/// An authenticated answer listing no organisation: the user left, or was
/// removed from, every one, so their queued reports go too.
fn release_audit_queue(conn: &rusqlite::Connection) {
    match crate::org_audit::discard_all(conn) {
        Ok(0) => {}
        Ok(n) => eprintln!("org audit: in no organisation any more, discarded {n} reports"),
        Err(e) => eprintln!("org audit: couldn't discard the queue: {e:#}"),
    }
}

/// [`apply_local_only`] then [`apply_device_policies`]. Returns whether either
/// rewrote a setting.
pub(crate) fn apply_policy_to_settings(conn: &rusqlite::Connection) -> bool {
    let local_only = apply_local_only(conn).unwrap_or_else(|e| {
        eprintln!("org policy: couldn't apply local_only to settings: {e:#}");
        false
    });
    let device = apply_device_policies(conn).unwrap_or_else(|e| {
        eprintln!("org policy: couldn't apply audio and telemetry policies to settings: {e:#}");
        false
    });
    local_only || device
}

/// At launch, before the sync observer is built: pin a managed server URL
/// (dropping a session that belongs to another server) and move settings to
/// what the policy in force enforces, so a managed profile works before any
/// sign-in.
pub(crate) fn apply_managed_at_launch(conn: &rusqlite::Connection, cache: &crate::keychain::Cache) {
    pin_managed_server(conn, org_policy::managed().server_url.as_deref(), || {
        super::cloud_auth::forget_account(cache)
    });
    apply_policy_to_settings(conn);
}

/// Pin `managed` as the server URL. When that changes it, the stored account
/// belongs to another server, and `forget` drops all of it: a password left
/// behind would sign in to the new server. Returns whether it changed.
pub(crate) fn pin_managed_server(conn: &rusqlite::Connection, managed: Option<&str>, forget: impl FnOnce()) -> bool {
    match super::cloud::pin_server_url(conn, managed) {
        Ok(true) => {
            forget();
            true
        }
        Ok(false) => false,
        Err(e) => {
            eprintln!("org policy: couldn't pin the managed server URL: {e:#}");
            false
        }
    }
}

/// Re-read the managed configuration profile and act on what changed: pin a
/// new server URL, move settings to enforced values and tell the UI. Runs from
/// `cloud_status`, so window focus picks up a profile installed meanwhile.
pub(crate) fn refresh_managed_config(app: &tauri::AppHandle, state: &State<'_, AppState>) {
    let change = org_policy::reload_managed();
    if change.server_url {
        let managed = org_policy::managed().server_url;
        let pinned = pin_managed_server(&state.db.lock(), managed.as_deref(), || {
            super::cloud_auth::forget_account(&state.keychain_cache)
        });
        if pinned {
            state.sync.config_changed();
        }
    }
    if change.policy {
        let settings = apply_policy_to_settings(&state.db.lock());
        let _ = app.emit("org_policy_changed", ());
        if settings {
            let _ = app.emit("org_settings_rewritten", ());
        }
    }
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

/// How often the retention sweep runs after the one at launch. Hourly rather
/// than daily: the timer doesn't advance while the Mac sleeps.
pub(crate) const RETENTION_SWEEP_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// Purge every note past its organisation's `retention_days`, then remove the
/// notes' files and tell the UI. Purge, never Trash, and nothing is pushed: the
/// server runs its own retention. Busy notes ([`note_busy`]) wait for the next
/// sweep; `recording_start` and `transcribe_note` refuse a note past its
/// cutoff, which closes the gap between that check and the purge. Takes the
/// database rather than `AppState`, which the org refresh loop may run before.
pub(crate) fn org_retention_sweep(app: &tauri::AppHandle, db: &Arc<Mutex<rusqlite::Connection>>) -> usize {
    let now = db::now_ms();
    let candidates = expired_notes(&db.lock(), now);
    let purged: Vec<String> = candidates
        .into_iter()
        .filter(|id| !note_busy(app, id) && purge_if_still_expired(&db.lock(), id, now))
        .collect();
    announce_purged(app, &purged);
    purged.len()
}

/// Whether a note is in use. A capture that is starting or stopping doesn't
/// say which note it holds, so it makes every note busy.
pub(crate) fn note_busy(app: &tauri::AppHandle, note_id: &str) -> bool {
    let Some(state) = app.try_state::<AppState>() else { return false };
    {
        let capture = state.recording.lock();
        if capture.starting
            || capture.stop != crate::recording::StopState::None
            || capture.note_id.as_deref() == Some(note_id)
        {
            return true;
        }
    }
    state.transcribing.lock().contains(note_id) || super::deferred::replay_owed(note_id)
}

/// Every note `org_policy::retention_in_force` puts past its cutoff at `now_ms`.
pub(crate) fn expired_notes(conn: &rusqlite::Connection, now_ms: i64) -> Vec<String> {
    let mut ids = Vec::new();
    for (workspace, rule) in org_policy::retention_in_force(conn) {
        let cutoff = org_policy::retention_cutoff(now_ms, rule.days);
        match db::notes_created_before(conn, &workspace, cutoff) {
            Ok(found) => ids.extend(found),
            Err(e) => eprintln!("retention: couldn't list expired notes: {e:#}"),
        }
    }
    ids
}

/// Purge `ids` that are still expired, returning those that went.
#[cfg(test)]
pub(crate) fn purge_notes(conn: &rusqlite::Connection, ids: &[String], now_ms: i64) -> Vec<String> {
    ids.iter().filter(|id| purge_if_still_expired(conn, id, now_ms)).cloned().collect()
}

/// Purge one note found by [`expired_notes`] if, read again under this lock,
/// it is still in a retention workspace and still past that workspace's
/// cutoff: it may have moved (to Personal, say) or been re-dated since.
pub(crate) fn purge_if_still_expired(conn: &rusqlite::Connection, id: &str, now_ms: i64) -> bool {
    let Ok(note) = db::get_note(conn, id) else { return false };
    let Some(rule) = org_policy::retention_rule(conn, &note.workspace_id) else { return false };
    if !org_policy::is_past(note.created_at, org_policy::retention_cutoff(now_ms, rule.days)) {
        return false;
    }
    match db::purge_note(conn, id) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("retention: couldn't purge note {id}: {e:#}");
            false
        }
    }
}

fn days_phrase(days: u32) -> String {
    format!("{days} {}", if days == 1 { "day" } else { "days" })
}

/// Refuse to record into or transcribe a note past its organisation's
/// retention: the next sweep purges it. Under retention a missing note is
/// refused too, since a sweep may just have purged it; without retention a
/// missing note is left to the caller, as before.
pub(crate) fn refuse_past_retention(conn: &rusqlite::Connection, note_id: &str, now_ms: i64) -> Result<(), String> {
    let Ok(note) = db::get_note(conn, note_id) else {
        if org_policy::retention_in_force(conn).is_empty() {
            return Ok(());
        }
        return Err("This note has been deleted.".into());
    };
    let Some(rule) = org_policy::retention_rule(conn, &note.workspace_id) else { return Ok(()) };
    if org_policy::is_past(note.created_at, org_policy::retention_cutoff(now_ms, rule.days)) {
        return Err(format!("This note is past {}'s retention and will be deleted.", rule.source));
    }
    Ok(())
}

/// Refuse to move a note into a workspace whose retention it is already past:
/// the server would refuse it and the sweep would purge the only copy.
pub(crate) fn check_move(
    conn: &rusqlite::Connection,
    note: &db::Note,
    to: &str,
    to_name: Option<&str>,
    now_ms: i64,
) -> Result<(), String> {
    let Some(rule) = org_policy::retention_rule(conn, to) else { return Ok(()) };
    if org_policy::is_past(note.created_at, org_policy::retention_cutoff(now_ms, rule.days)) {
        let name = to_name.map(str::trim).filter(|n| !n.is_empty()).unwrap_or("this workspace");
        return Err(format!("Notes older than {} can't be moved to {name}.", days_phrase(rule.days)));
    }
    Ok(())
}

/// Under retention a note may only be dated earlier, as on the server, and no
/// further back than the cutoff (`org_policy::redate_bounds`).
pub(crate) fn check_redate(
    conn: &rusqlite::Connection,
    note: &db::Note,
    created_at: i64,
    now_ms: i64,
) -> Result<(), String> {
    let Some(rule) = org_policy::retention_rule(conn, &note.workspace_id) else { return Ok(()) };
    let (min, max) = org_policy::redate_bounds(rule.days, note.created_at, now_ms);
    if created_at > max {
        return Err(format!("Blocked by {}: a note can't be dated later.", rule.source));
    }
    if created_at < min {
        return Err(format!("Blocked by {}: notes are deleted after {}.", rule.source, days_phrase(rule.days)));
    }
    Ok(())
}

/// After notes were purged: remove their files and tell the UI, which closes
/// an open one. Nothing is sent when nothing went.
pub(crate) fn announce_purged(app: &tauri::AppHandle, ids: &[String]) {
    if ids.is_empty() {
        return;
    }
    for id in ids {
        super::notes::purge_note_assets_best_effort(app, id);
    }
    let _ = app.emit("notes_purged", ids);
    let _ = app.emit("notes_changed", ());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::ProviderId;

    #[test]
    fn a_managed_pin_that_changes_the_server_forgets_the_account() {
        let (_d, conn) = conn();
        let mut forgotten = 0;
        assert!(!pin_managed_server(&conn, None, || forgotten += 1));
        db::set_setting(&conn, super::super::cloud::SETTING_BASE_URL, "https://old.example").unwrap();
        assert!(pin_managed_server(&conn, Some("https://new.example"), || forgotten += 1));
        assert_eq!(forgotten, 1);
        assert!(!pin_managed_server(&conn, Some("https://new.example"), || forgotten += 1));
        assert_eq!(forgotten, 1, "the same server keeps its account");
    }

    fn conn() -> (tempfile::TempDir, rusqlite::Connection) {
        let dir = tempfile::tempdir().unwrap();
        let conn = db::open(&dir.path().join("t.sqlite")).unwrap();
        (dir, conn)
    }

    const EMPTY: &str = r#"{"orgs":[]}"#;

    fn audit_org(reads: bool) -> String {
        format!(
            r#"{{"orgs":[{{"id":"o1","name":"Northwind","policy":{{"local_only":false,"retention_days":0,"telemetry_off":false,"audio_forbidden":false,"audit_reads":{reads}}},"workspaces":["w1aaaaaaaaaaaaa"]}}]}}"#
        )
    }

    fn queue_len(conn: &rusqlite::Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM org_audit_queue", [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn only_a_confirmed_leave_discards_the_audit_queue() {
        use crate::org_audit::{record, Action, Format};
        let (_d, conn) = conn();
        apply_fetch(&conn, &org_policy::classify(200, &audit_org(false)));
        assert!(record(&conn, Action::Export(Format::Pdf), "w1aaaaaaaaaaaaa", "n1"));
        for outcome in [
            FetchOutcome::SignedOut,
            FetchOutcome::TwoFactorRequired,
            FetchOutcome::Unsupported,
            FetchOutcome::Failed("timeout".into()),
            org_policy::classify(200, "{}"),
        ] {
            apply_fetch(&conn, &outcome);
            assert_eq!(queue_len(&conn), 1, "{outcome:?}");
        }
        assert!(record(&conn, Action::AudioSave, "w1aaaaaaaaaaaaa", "n1"), "signed out, the cache still records");
        apply_fetch(&conn, &org_policy::classify(200, EMPTY));
        assert_eq!(queue_len(&conn), 0);
        assert!(!record(&conn, Action::AudioSave, "w1aaaaaaaaaaaaa", "n1"), "and records nothing more");
    }

    #[test]
    fn audit_reads_turning_on_is_a_policy_change() {
        let (_d, conn) = conn();
        apply_fetch(&conn, &org_policy::classify(200, &audit_org(false)));
        assert!(apply_fetch(&conn, &org_policy::classify(200, &audit_org(true))).policy);
        assert!(!apply_fetch(&conn, &org_policy::classify(200, &audit_org(true))).policy);
    }

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
            Changes { policy: true, settings: false, content: true, retention: false }
        );
        let local_only = body.replace(r#""local_only":false"#, r#""local_only":true"#);
        assert_eq!(
            apply_fetch(&conn, &org_policy::classify(200, &local_only)),
            Changes { policy: true, settings: false, content: false, retention: false },
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
            Changes { policy: true, settings: true, content: true, retention: false }
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
            Changes { policy: true, settings: true, content: true, retention: false }
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

    // ---- retention ----

    const DAY: i64 = org_policy::DAY_MS;
    const NOW: i64 = 1_800_000_000_000;

    fn retention_body(days: u32, workspaces: &str) -> String {
        format!(
            r#"{{"orgs":[{{"id":"o1","name":"Northwind","policy":{{"local_only":false,"retention_days":{days},"telemetry_off":false,"audio_forbidden":false}},"workspaces":[{workspaces}]}}]}}"#
        )
    }

    /// One note in `workspace`, created `age_days` before `NOW`.
    fn aged_note(conn: &rusqlite::Connection, workspace: &str, age_days: i64) -> String {
        let id = db::create_note(conn, "en", "meeting", workspace).unwrap().id;
        conn.execute(
            "UPDATE notes SET created_at = ?1 WHERE id = ?2",
            rusqlite::params![NOW - age_days * DAY, id],
        )
        .unwrap();
        id
    }

    fn sweep(conn: &rusqlite::Connection) -> Vec<String> {
        purge_notes(conn, &expired_notes(conn, NOW), NOW)
    }

    fn exists(conn: &rusqlite::Connection, id: &str) -> bool {
        db::note_exists(conn, id).unwrap()
    }

    #[test]
    fn without_an_organisation_the_sweep_purges_nothing() {
        let (_d, conn) = conn();
        let old = aged_note(&conn, "w1", 5_000);
        let personal = aged_note(&conn, "", 5_000);
        assert!(sweep(&conn).is_empty(), "nothing cached");
        apply_fetch(&conn, &org_policy::classify(200, EMPTY));
        assert!(sweep(&conn).is_empty(), "no organisation");
        assert!(exists(&conn, &old) && exists(&conn, &personal));
    }

    #[test]
    fn an_unreadable_cache_purges_nothing() {
        let (_d, conn) = conn();
        apply_fetch(&conn, &org_policy::classify(200, &retention_body(30, r#""w1""#)));
        let old = aged_note(&conn, "w1", 31);
        db::set_setting(&conn, org_policy::SETTING_ORG_CONFIG, r#"{"orgs":[{"id":"o1""#).unwrap();
        assert!(sweep(&conn).is_empty());
        assert!(exists(&conn, &old));
    }

    #[test]
    fn zero_days_purges_nothing() {
        let (_d, conn) = conn();
        apply_fetch(&conn, &org_policy::classify(200, &retention_body(0, r#""w1""#)));
        let old = aged_note(&conn, "w1", 5_000);
        assert!(sweep(&conn).is_empty());
        assert!(exists(&conn, &old));
    }

    #[test]
    fn the_sweep_purges_only_expired_notes_in_the_organisation_workspaces() {
        let (_d, conn) = conn();
        let changes = apply_fetch(&conn, &org_policy::classify(200, &retention_body(30, r#""w1","""#)));
        assert!(changes.retention);
        let expired = aged_note(&conn, "w1", 31);
        let trashed = aged_note(&conn, "w1", 40);
        db::delete_note(&conn, &trashed).unwrap();
        let young = aged_note(&conn, "w1", 29);
        let personal = aged_note(&conn, "", 5_000);
        let outside = aged_note(&conn, "w2", 5_000);
        let undated = aged_note(&conn, "w1", 0);
        conn.execute("UPDATE notes SET created_at = 0 WHERE id = ?1", [&undated]).unwrap();
        let in_seconds = aged_note(&conn, "w1", 0);
        conn.execute("UPDATE notes SET created_at = ?1 WHERE id = ?2", rusqlite::params![org_policy::MIN_EPOCH_MS - 1, in_seconds])
            .unwrap();
        let patch = db::NotePatch { transcript: Some("Hege: the case".into()), ..Default::default() };
        db::update_note(&conn, &expired, &patch).unwrap();
        assert_eq!(db::list_note_revisions(&conn, &expired).unwrap().len(), 1);

        let mut purged = sweep(&conn);
        purged.sort();
        let mut expected = vec![expired.clone(), trashed.clone()];
        expected.sort();
        assert_eq!(purged, expected);
        for id in [&expired, &trashed] {
            assert!(!exists(&conn, id), "purged, not moved to the Trash");
        }
        for id in [&young, &personal, &outside, &undated, &in_seconds] {
            assert!(exists(&conn, id));
        }
        assert!(db::list_note_revisions(&conn, &expired).unwrap().is_empty());
        assert!(sweep(&conn).is_empty(), "a second run finds nothing");
    }

    #[test]
    fn retention_changes_are_reported_and_repeats_are_not() {
        let (_d, conn) = conn();
        assert!(apply_fetch(&conn, &org_policy::classify(200, &retention_body(30, r#""w1""#))).retention);
        assert!(!apply_fetch(&conn, &org_policy::classify(200, &retention_body(30, r#""w1""#))).retention);
        assert!(apply_fetch(&conn, &org_policy::classify(200, &retention_body(7, r#""w1""#))).retention);
        assert!(apply_fetch(&conn, &org_policy::classify(200, &retention_body(7, r#""w1","w2""#))).retention);
        assert!(!apply_fetch(&conn, &FetchOutcome::Failed("offline".into())).retention);
        assert!(apply_fetch(&conn, &org_policy::classify(200, EMPTY)).retention);
    }

    fn get_note(conn: &rusqlite::Connection, id: &str) -> db::Note {
        db::get_note(conn, id).unwrap()
    }

    /// Every input the refusals could see, with no organisation in force: no
    /// cache, an empty answer, an unreadable cache, and a 0-day policy.
    fn without_retention(conn: &rusqlite::Connection, state: usize) {
        match state {
            0 => {}
            1 => {
                apply_fetch(conn, &org_policy::classify(200, EMPTY));
            }
            2 => db::set_setting(conn, org_policy::SETTING_ORG_CONFIG, "{not json").unwrap(),
            _ => {
                apply_fetch(conn, &org_policy::classify(200, &retention_body(0, r#""w1""#)));
            }
        }
    }

    #[test]
    fn without_retention_record_transcribe_move_and_redate_refuse_nothing() {
        for state in 0..4 {
            let (_d, conn) = conn();
            without_retention(&conn, state);
            for (ws, age) in [("w1", 5_000), ("", 5_000), ("w1", 0), ("w2", 31)] {
                let id = aged_note(&conn, ws, age);
                let note = get_note(&conn, &id);
                assert_eq!(refuse_past_retention(&conn, &id, NOW), Ok(()), "state {state}");
                for to in ["w1", "w2", ""] {
                    assert_eq!(check_move(&conn, &note, to, Some("Team"), NOW), Ok(()), "state {state}");
                }
                for date in [NOW, NOW - 10_000 * DAY, note.created_at + DAY, 1, org_policy::MIN_EPOCH_MS - 1] {
                    assert_eq!(check_redate(&conn, &note, date, NOW), Ok(()), "state {state}, date {date}");
                }
            }
            assert_eq!(refuse_past_retention(&conn, "missing", NOW), Ok(()));
        }
    }

    #[test]
    fn a_note_past_retention_refuses_recording_and_transcription() {
        let (_d, conn) = conn();
        apply_fetch(&conn, &org_policy::classify(200, &retention_body(30, r#""w1""#)));
        let past = aged_note(&conn, "w1", 31);
        let young = aged_note(&conn, "w1", 29);
        let personal = aged_note(&conn, "", 5_000);
        let undated = aged_note(&conn, "w1", 0);
        conn.execute("UPDATE notes SET created_at = 5 WHERE id = ?1", [&undated]).unwrap();
        assert_eq!(
            refuse_past_retention(&conn, &past, NOW),
            Err("This note is past Northwind's retention and will be deleted.".into())
        );
        for id in [&young, &personal, &undated] {
            assert_eq!(refuse_past_retention(&conn, id, NOW), Ok(()));
        }
    }

    #[test]
    fn a_note_past_a_workspace_retention_cant_move_there() {
        let (_d, conn) = conn();
        apply_fetch(&conn, &org_policy::classify(200, &retention_body(30, r#""w1""#)));
        let old = get_note(&conn, &aged_note(&conn, "", 31));
        let young = get_note(&conn, &aged_note(&conn, "", 29));
        assert_eq!(
            check_move(&conn, &old, "w1", Some("Case work"), NOW),
            Err("Notes older than 30 days can't be moved to Case work.".into())
        );
        assert_eq!(
            check_move(&conn, &old, "w1", None, NOW),
            Err("Notes older than 30 days can't be moved to this workspace.".into())
        );
        assert_eq!(check_move(&conn, &young, "w1", Some("Case work"), NOW), Ok(()));
        assert_eq!(check_move(&conn, &old, "w2", Some("Other"), NOW), Ok(()), "outside the organisation");
        assert_eq!(check_move(&conn, &old, "", None, NOW), Ok(()), "to Personal");
    }

    #[test]
    fn under_retention_a_note_moves_only_earlier_and_not_past_the_cutoff() {
        let (_d, conn) = conn();
        apply_fetch(&conn, &org_policy::classify(200, &retention_body(30, r#""w1""#)));
        let note = get_note(&conn, &aged_note(&conn, "w1", 10));
        assert_eq!(check_redate(&conn, &note, note.created_at - DAY, NOW), Ok(()));
        assert_eq!(check_redate(&conn, &note, NOW - 30 * DAY, NOW), Ok(()), "the cutoff itself");
        assert_eq!(
            check_redate(&conn, &note, note.created_at + 1, NOW),
            Err("Blocked by Northwind: a note can't be dated later.".into())
        );
        assert_eq!(
            check_redate(&conn, &note, NOW - 30 * DAY - 1, NOW),
            Err("Blocked by Northwind: notes are deleted after 30 days.".into())
        );
        let personal = get_note(&conn, &aged_note(&conn, "", 10));
        assert_eq!(check_redate(&conn, &personal, NOW, NOW), Ok(()), "Personal is unchanged");
    }

    /// The commands themselves take Tauri state, so these pin where each calls
    /// its refusal: after the slot claim or the `transcribing` insert, and in
    /// `notes_set_date` only for a note that exists, as on main.
    #[test]
    fn the_commands_call_their_refusals_in_the_right_place() {
        let capture = include_str!("capture.rs");
        let start = &capture[capture.find("pub async fn recording_start").unwrap()..];
        let claim = start.find("StartClaim::take").unwrap();
        let refuse = start.find("refuse_past_retention").unwrap();
        assert!(claim < refuse && refuse < start.find("ensure_provider_ready").unwrap());

        let deferred = include_str!("deferred.rs");
        let tn = &deferred[deferred.find("pub async fn transcribe_note").unwrap()..];
        assert!(tn.find("transcribing.lock().insert").unwrap() < tn.find("refuse_past_retention").unwrap());

        let notes = include_str!("notes.rs");
        let date = &notes[notes.find("pub fn notes_set_date").unwrap()..];
        assert!(date.contains("if let Ok(note) = db::get_note(&conn, &id) {\n            super::org::check_redate"));
        let mv = &notes[notes.find("pub fn notes_set_workspace").unwrap()..];
        assert!(mv.find("already there").unwrap() < mv.find("check_move").unwrap());
    }

    #[test]
    fn a_note_that_moved_or_was_redated_after_listing_survives() {
        let (_d, conn) = conn();
        apply_fetch(&conn, &org_policy::classify(200, &retention_body(30, r#""w1""#)));
        let moved = aged_note(&conn, "w1", 31);
        let redated = aged_note(&conn, "w1", 31);
        let gone = aged_note(&conn, "w1", 31);
        let still = aged_note(&conn, "w1", 31);
        let ids = expired_notes(&conn, NOW);
        assert_eq!(ids.len(), 4);

        db::set_note_workspace(&conn, &moved, "").unwrap();
        db::set_note_created_at(&conn, &redated, NOW - DAY).unwrap();
        db::purge_note(&conn, &gone).unwrap();

        assert_eq!(purge_notes(&conn, &ids, NOW), [still]);
        assert!(exists(&conn, &moved), "Personal is never purged");
        assert!(exists(&conn, &redated));
    }

    #[test]
    fn under_retention_a_missing_note_is_refused() {
        let (_d, conn) = conn();
        apply_fetch(&conn, &org_policy::classify(200, &retention_body(30, r#""w1""#)));
        assert_eq!(refuse_past_retention(&conn, "purged", NOW), Err("This note has been deleted.".into()));
        let personal = aged_note(&conn, "", 5_000);
        assert_eq!(refuse_past_retention(&conn, &personal, NOW), Ok(()));
    }
}
