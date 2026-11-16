//! Which Keychain account holds each provider's API key, as named in
//! `providers::REGISTRY`. The service, the reads and the cache in front of
//! them are `crate::keychain`'s.

use crate::providers::ProviderId;

/// Keychain account for a provider id, `None` for one that takes no key
/// (local Whisper) and for an id the registry doesn't know.
pub fn keychain_account_for(provider_id: &str) -> Option<&'static str> {
    ProviderId::parse(provider_id)?.keychain_account()
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
