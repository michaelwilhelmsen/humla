//! Per-provider API key commands + helpers. Stores keys in the macOS
//! Keychain (service `no.humla.app`), caches them on `AppState`, and
//! tests them with a lightweight authenticated GET. Commands are
//! surfaced to the frontend via `commands::provider_key_*` (re-exported
//! from the parent module). `read_provider_api_key` is also called by
//! the recording / transcribe / summary paths that stay in the parent
//! module, hence `pub(crate)`.

use super::err;
use crate::db;
use crate::openai;
use crate::AppState;
use tauri::State;

// Legacy SQLite row that used to hold the OpenAI API key. New installs
// never touch it; existing installs migrate it forward to Keychain on
// first read (see `read_openai_api_key`) and blank the row so a future
// DB export / backup doesn't leak the secret.
const API_KEY: &str = "__openai_api_key__";

/// Read the API key for the given provider from the macOS Keychain.
/// Returns Ok(None) if no key is stored or the provider doesn't take one
/// (e.g. local Whisper). Cached per-provider on AppState; first call per
/// provider per session triggers exactly one Keychain prompt.
///
/// On first call for OpenAI after upgrading from a pre-Keychain build,
/// migrates the legacy plaintext row from SQLite into the Keychain and
/// blanks the SQLite copy. Other providers were never stored anywhere
/// else, so they have no migration path.
pub(crate) fn read_provider_api_key(
    state: &State<AppState>,
    provider_id: &'static str,
) -> Result<Option<String>, String> {
    if let Some(cached) = state.api_key_cache.lock().get(provider_id).cloned() {
        return Ok(cached);
    }
    let Some(account) = crate::stt::keychain_account_for(provider_id) else {
        return Ok(None);
    };
    let entry = keyring::Entry::new(crate::stt::KEYCHAIN_SERVICE, account)
        .map_err(|e| format!("keychain entry: {e}"))?;
    let result = match entry.get_password() {
        Ok(s) => {
            let t = s.trim().to_string();
            Ok(if t.is_empty() { None } else { Some(t) })
        }
        Err(keyring::Error::NoEntry) => {
            if provider_id == "openai" {
                migrate_legacy_api_key(state, &entry)
            } else {
                Ok(None)
            }
        }
        Err(e) => Err(format!("keychain read: {e}")),
    };
    if let Ok(value) = &result {
        state
            .api_key_cache
            .lock()
            .insert(provider_id, value.clone());
    }
    result
}

fn migrate_legacy_api_key(
    state: &State<AppState>,
    entry: &keyring::Entry,
) -> Result<Option<String>, String> {
    let legacy = {
        let conn = state.db.lock();
        db::get_setting(&conn, API_KEY).map_err(err)?.unwrap_or_default()
    };
    let trimmed = legacy.trim().to_string();
    if trimmed.is_empty() {
        return Ok(None);
    }
    entry
        .set_password(&trimmed)
        .map_err(|e| format!("keychain migrate: {e}"))?;
    let conn = state.db.lock();
    let _ = db::set_setting(&conn, API_KEY, "");
    Ok(Some(trimmed))
}

/// Write the API key for the given provider to the macOS Keychain.
/// Empty input deletes the entry. For OpenAI, also blanks the legacy
/// SQLite row in lockstep. Updates the in-memory cache so subsequent
/// reads return the new value without a fresh Keychain prompt.
fn set_provider_api_key(
    state: &State<AppState>,
    provider_id: &'static str,
    key: &str,
) -> Result<(), String> {
    let trimmed = key.trim();
    let account = crate::stt::keychain_account_for(provider_id)
        .ok_or_else(|| format!("provider {provider_id} has no Keychain slot"))?;
    let entry = keyring::Entry::new(crate::stt::KEYCHAIN_SERVICE, account)
        .map_err(|e| format!("keychain entry: {e}"))?;
    if trimmed.is_empty() {
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => {}
            Err(e) => return Err(format!("keychain delete: {e}")),
        }
    } else {
        entry
            .set_password(trimmed)
            .map_err(|e| format!("keychain write: {e}"))?;
    }
    if provider_id == "openai" {
        let conn = state.db.lock();
        let _ = db::set_setting(&conn, API_KEY, "");
    }
    state.api_key_cache.lock().insert(
        provider_id,
        if trimmed.is_empty() { None } else { Some(trimmed.to_string()) },
    );
    Ok(())
}

#[derive(serde::Serialize)]
pub struct TestResult {
    ok: bool,
    status: u16,
    error: Option<String>,
}

/// Map a frontend-supplied provider string to a static id we trust.
/// Rejecting unknown ids prevents the frontend from probing arbitrary
/// Keychain accounts via the Tauri bridge.
fn canonical_provider_id(s: &str) -> Option<&'static str> {
    match s {
        "openai" => Some("openai"),
        "deepgram" => Some("deepgram"),
        "groq" => Some("groq"),
        "local" => Some("local"),
        _ => None,
    }
}

#[tauri::command]
pub fn provider_key_get(
    state: State<AppState>,
    provider: String,
) -> Result<Option<String>, String> {
    let id = canonical_provider_id(&provider)
        .ok_or_else(|| format!("unknown provider: {provider}"))?;
    Ok(read_provider_api_key(&state, id)?.map(|_| "stored".to_string()))
}

#[tauri::command]
pub fn provider_key_set(
    state: State<AppState>,
    provider: String,
    key: String,
) -> Result<(), String> {
    let id = canonical_provider_id(&provider)
        .ok_or_else(|| format!("unknown provider: {provider}"))?;
    set_provider_api_key(&state, id, &key)
}

#[tauri::command]
pub async fn provider_key_test(
    state: State<'_, AppState>,
    provider: String,
) -> Result<TestResult, String> {
    let id = canonical_provider_id(&provider)
        .ok_or_else(|| format!("unknown provider: {provider}"))?;
    let key = read_provider_api_key(&state, id)?
        .ok_or_else(|| "No API key stored".to_string())?;

    let (url, auth_header) = match id {
        "openai" => (
            format!("{}/models", openai::BASE),
            format!("Bearer {key}"),
        ),
        "deepgram" => (
            "https://api.deepgram.com/v1/projects".to_string(),
            format!("Token {key}"),
        ),
        "groq" => (
            "https://api.groq.com/openai/v1/models".to_string(),
            format!("Bearer {key}"),
        ),
        _ => return Err(format!("provider {id} doesn't support test")),
    };
    let r = openai::client()
        .get(url)
        .header("Authorization", auth_header)
        .send()
        .await
        .map_err(|e| format!("network: {e}"))?;
    let status = r.status();
    if status.is_success() {
        return Ok(TestResult { ok: true, status: status.as_u16(), error: None });
    }
    let body = r.text().await.unwrap_or_default();
    let snippet: String = body.chars().take(300).collect();
    Ok(TestResult { ok: false, status: status.as_u16(), error: Some(snippet) })
}
