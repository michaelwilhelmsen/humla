//! Per-provider Keychain access, one slot per cloud provider as named in
//! `providers::REGISTRY`. Cache lives on AppState as a HashMap so each
//! provider's first read triggers exactly one Keychain prompt.

use std::collections::HashMap;
use std::sync::Arc;
use parking_lot::Mutex;
use crate::providers::ProviderId;

pub const KEYCHAIN_SERVICE: &str = "no.humla.app";

pub type ApiKeyCache = Arc<Mutex<HashMap<&'static str, Option<String>>>>;

pub fn new_cache() -> ApiKeyCache {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Keychain account for a provider id, `None` for one that takes no key
/// (local Whisper) and for an id the registry doesn't know.
pub fn keychain_account_for(provider_id: &str) -> Option<&'static str> {
    ProviderId::parse(provider_id)?.keychain_account()
}

pub fn requires_api_key(provider_id: &str) -> bool {
    keychain_account_for(provider_id).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_providers_have_keychain_accounts() {
        assert_eq!(keychain_account_for("openai"), Some("openai_api_key"));
        assert_eq!(keychain_account_for("anthropic"), Some("anthropic_api_key"));
        assert_eq!(keychain_account_for("deepgram"), Some("deepgram_api_key"));
        assert_eq!(keychain_account_for("groq"), Some("groq_api_key"));
        assert_eq!(keychain_account_for("local"), None);
        assert_eq!(keychain_account_for("nonsense"), None);
    }
}
