//! Organisation policies over IPC, and the one refresh of their cache.

use std::sync::Arc;

use parking_lot::Mutex;
use serde::Serialize;
use tauri::{Emitter, State};

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

/// Fetch the organisation config with a live session and cache it. Emits
/// `org_policy_changed` when the cache changed.
pub(crate) async fn refresh_org_config(
    db: &Arc<Mutex<rusqlite::Connection>>,
    app: &tauri::AppHandle,
    base: &str,
    token: &str,
) {
    let outcome = org_policy::fetch(base, token).await;
    match &outcome {
        FetchOutcome::SignedOut => super::cloud_auth::forget_session_token(token),
        FetchOutcome::Failed(e) => eprintln!("cloud: {e}"),
        _ => {}
    }
    let (changed, applied) = {
        let conn = db.lock();
        let changed = org_policy::store(&conn, &outcome);
        (changed, apply_local_only(&conn))
    };
    if let Err(e) = &changed {
        eprintln!("cloud: couldn't cache the organisation config: {e:#}");
    }
    if let Err(e) = &applied {
        eprintln!("org policy: couldn't apply local_only to settings: {e:#}");
    }
    if matches!(changed, Ok(true)) || matches!(applied, Ok(true)) {
        let _ = app.emit("org_policy_changed", ());
    }
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
