//! The macOS Keychain: Humla's one service, and the cache in front of it.
//!
//! Every secret Humla keeps — each provider's API key, the webhook endpoint
//! (#213), the cloud credentials — is an account under [`SERVICE`]. Reads go
//! through [`Cache`] so a session prompts the Keychain at most once per
//! account: without it, a signed build whose ACL the user hasn't trusted yet
//! would prompt on every read.
//!
//! Keyed by Keychain account rather than by whatever names the secret, so two
//! callers can't cache one account under two keys and disagree after a write.

use keyring_core::{Entry, Error};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

pub const SERVICE: &str = "no.humla.app";

/// What this session has read or written, per account. `None` is a cached
/// absence: a missing entry stays missing until this session writes one.
pub type Cache = Arc<Mutex<HashMap<&'static str, Option<String>>>>;

pub fn new_cache() -> Cache {
    Arc::new(Mutex::new(HashMap::new()))
}

/// keyring-core reads and writes through a process-wide store that has to be
/// set before the first entry is made. Off macOS there is no Keychain; an
/// in-memory store keeps builds and tests there working.
static STORE: LazyLock<Result<(), String>> = LazyLock::new(|| {
    #[cfg(target_os = "macos")]
    let store = apple_native_keyring_store::keychain::Store::new();
    #[cfg(not(target_os = "macos"))]
    let store = keyring_core::mock::Store::new();
    keyring_core::set_default_store(store.map_err(|e| format!("keychain store: {e}"))?);
    Ok(())
});

pub fn entry(account: &str) -> Result<Entry, String> {
    STORE.clone()?;
    Entry::new(SERVICE, account).map_err(|e| format!("keychain entry: {e}"))
}

/// Read `account`, from the cache when this session already has. A missing or
/// blank entry reads as `None`.
pub fn read(cache: &Cache, account: &'static str) -> Result<Option<String>, String> {
    read_or(cache, account, |_| Ok(None))
}

/// [`read`], with `on_missing` deciding what an absent entry means on a cache
/// miss — the OpenAI key's migration out of its pre-Keychain SQLite row is the
/// one caller that needs it. Its answer is cached like any other.
pub fn read_or(
    cache: &Cache,
    account: &'static str,
    on_missing: impl FnOnce(&Entry) -> Result<Option<String>, String>,
) -> Result<Option<String>, String> {
    if let Some(cached) = cache.lock().get(account).cloned() {
        return Ok(cached);
    }
    let entry = entry(account)?;
    let value = match entry.get_password() {
        Ok(s) => non_blank(&s),
        Err(Error::NoEntry) => on_missing(&entry)?,
        Err(e) => return Err(format!("keychain read: {e}")),
    };
    cache.lock().insert(account, value.clone());
    Ok(value)
}

/// Store `value` under `account`, or delete the entry when it is `None` or
/// blank. The cache follows, so the next read returns it without a prompt.
pub fn write(cache: &Cache, account: &'static str, value: Option<&str>) -> Result<(), String> {
    let value = value.and_then(non_blank);
    let entry = entry(account)?;
    match &value {
        Some(v) => entry
            .set_password(v)
            .map_err(|e| format!("keychain write: {e}"))?,
        None => match entry.delete_credential() {
            Ok(()) | Err(Error::NoEntry) => {}
            Err(e) => return Err(format!("keychain delete: {e}")),
        },
    }
    cache.lock().insert(account, value);
    Ok(())
}

fn non_blank(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}
