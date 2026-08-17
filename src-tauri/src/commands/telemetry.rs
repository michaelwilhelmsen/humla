//! Anonymous onboarding counters.
//!
//! Answers one question and no more: how many people get Humla running, and how many
//! get through setup rather than abandoning it. Onboarding completion is invisible
//! from outside the app — unlike "wrote a first note", which is already derivable
//! server-side — so it is the one thing that has to be reported.
//!
//! WHAT IS SENT: one field, an event name from the allowlist below. No install id, no
//! account, no version, no locale, no OS, nothing about what was configured or
//! recorded. The server discards anything else, stores the events as plain per-day
//! tallies rather than rows, and its access-log entry for this path is skipped by
//! Caddy and pruned from PocketBase's own log — see humla-cloud's
//! `server/pb_hooks/telemetry.pb.js`, which carries the full reasoning.
//!
//! TWO GATES, both required, and both fail CLOSED:
//!   1. `telemetry_enabled` must be exactly "true". **Unset means no.** That is what
//!      grandfathers every install made before this existed: those users were told
//!      Humla sends nothing, they never see the setup wizard again, and so they are
//!      never enrolled unless they switch it on themselves in Settings.
//!   2. The event must not already be marked sent (`telemetry_sent_<event>`), so each
//!      milestone counts at most once per install. That client-side dedupe is what
//!      lets the server count without holding a device id — the usual reason an
//!      "anonymous" endpoint ends up with one.
//!
//! The disclosure is shown in the setup wizard's footer on every step, and writing
//! `telemetry_enabled` is what that footer does. So the order is always: the user is
//! told, then the first event is sent — never the reverse.
//!
//! Failures are silent and unmarked, so an offline first launch simply retries on the
//! next one. The rare cost is a double count if a 204 is lost in transit; these are
//! directional numbers (the endpoint is unauthenticated and therefore inflatable
//! anyway), so that is the right side to err on.

use crate::db;
use crate::AppState;
use tauri::Manager;

/// Humla's own server, deliberately NOT the configured sync base. The question is
/// "how many people use Humla", which is a property of the app, not of whichever
/// sync server a self-hoster happens to point at — and a self-hosted PocketBase has
/// no telemetry endpoint to receive it.
const TELEMETRY_BASE: &str = "https://sync.humla.team";

/// The complete vocabulary, mirroring the allowlist in humla-cloud's
/// `telemetry.pb.js` and the step ids in `src/pages/onboarding/types.ts`. Three
/// mirrored lists; change them together. Validated here as well as on the server so
/// a typo fails in development rather than silently never being counted.
const ALLOWED: &[&str] = &[
    "app_first_run",
    "onboarding_finished",
    "onboarding_skipped",
    "onboarding_reached_welcome",
    "onboarding_reached_permissions",
    "onboarding_reached_language",
    "onboarding_reached_transcription",
    "onboarding_reached_summary",
    "onboarding_reached_cloud",
    "onboarding_reached_done",
];

/// True when this event may be sent right now: enabled, and not already sent.
/// Separate from the command so the gating is testable without a network.
pub(crate) fn should_send(enabled: Option<String>, already_sent: Option<String>, event: &str) -> bool {
    if !ALLOWED.contains(&event) {
        return false;
    }
    // Unset is NO, not "default on" — see gate 1 in the module docs.
    if enabled.as_deref() != Some("true") {
        return false;
    }
    already_sent.is_none()
}

fn sent_key(event: &str) -> String {
    format!("telemetry_sent_{event}")
}

/// Report one milestone. Fire-and-forget: never blocks the UI, never surfaces an
/// error to the caller, and never returns whether anything was sent — a caller that
/// could tell would be a caller that could be used to probe the setting.
#[tauri::command]
pub fn telemetry_event(app: tauri::AppHandle, event: String) {
    let (enabled, already) = {
        let state = app.state::<AppState>();
        let conn = state.db.lock();
        (
            db::get_setting(&conn, "telemetry_enabled").ok().flatten(),
            db::get_setting(&conn, &sent_key(&event)).ok().flatten(),
        )
    };
    if !should_send(enabled, already, &event) {
        return;
    }

    tauri::async_runtime::spawn(async move {
        let client = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
        {
            Ok(c) => c,
            Err(_) => return,
        };
        let ok = client
            .post(format!("{TELEMETRY_BASE}/api/humla/telemetry/event"))
            .json(&serde_json::json!({ "event": event }))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false);
        if !ok {
            return; // leave unmarked so it retries on a later launch
        }
        let state = app.state::<AppState>();
        let conn = state.db.lock();
        let _ = db::set_setting(&conn, &sent_key(&event), "1");
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_enabled_never_sends() {
        // The grandfathering rule: an install predating this feature has no
        // `telemetry_enabled` row and must stay silent.
        assert!(!should_send(None, None, "app_first_run"));
    }

    #[test]
    fn explicit_false_never_sends() {
        assert!(!should_send(Some("false".into()), None, "app_first_run"));
    }

    #[test]
    fn anything_but_true_is_off() {
        // Guards against a truthy-ish value being written by mistake.
        for v in ["1", "yes", "TRUE", "", "on"] {
            assert!(!should_send(Some(v.into()), None, "app_first_run"), "{v} should be off");
        }
    }

    #[test]
    fn enabled_and_unsent_sends_once() {
        assert!(should_send(Some("true".into()), None, "app_first_run"));
        // Marked → never again on this install.
        assert!(!should_send(Some("true".into()), Some("1".into()), "app_first_run"));
    }

    #[test]
    fn unknown_event_never_sends() {
        assert!(!should_send(Some("true".into()), None, "note_created"));
        assert!(!should_send(Some("true".into()), None, ""));
    }

    #[test]
    fn every_onboarding_step_has_an_event() {
        // Mirrors STEP_ORDER in src/pages/onboarding/types.ts.
        for step in ["welcome", "permissions", "language", "transcription", "summary", "cloud", "done"] {
            let ev = format!("onboarding_reached_{step}");
            assert!(ALLOWED.contains(&ev.as_str()), "missing event for step {step}");
        }
    }
}
