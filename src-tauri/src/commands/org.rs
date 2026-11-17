//! Organisation policies over IPC, and the one refresh of their cache.

use std::sync::Arc;

use parking_lot::Mutex;
use serde::Serialize;
use tauri::{Emitter, State};

use crate::org_policy::{self, EffectivePolicy, FetchOutcome};
use crate::AppState;

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
    let changed = {
        let conn = db.lock();
        org_policy::store(&conn, &outcome)
    };
    match changed {
        Ok(true) => {
            let _ = app.emit("org_policy_changed", ());
        }
        Ok(false) => {}
        Err(e) => eprintln!("cloud: couldn't cache the organisation config: {e:#}"),
    }
}
