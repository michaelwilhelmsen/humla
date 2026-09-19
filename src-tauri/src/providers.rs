//! The one list of providers Humla knows: Keychain account, key test and
//! capabilities per id. `src/lib/providers.ts` mirrors `REGISTRY` and
//! `providers.test.ts` reads this file to keep the two aligned.

use std::fmt;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ProviderId {
    OpenAi,
    Deepgram,
    Groq,
    /// Local Whisper for transcription; any OpenAI-compatible server (Ollama,
    /// LM Studio, llama-server) for summaries and chat.
    Local,
}

/// How a provider's key rides on a request.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AuthScheme {
    Bearer,
    Token,
}

impl AuthScheme {
    pub fn header_value(self, key: &str) -> String {
        match self {
            AuthScheme::Bearer => format!("Bearer {key}"),
            AuthScheme::Token => format!("Token {key}"),
        }
    }
}

/// A cheap authenticated GET that proves a key works.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct KeyTest {
    pub url: &'static str,
    pub auth: AuthScheme,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Capabilities {
    pub summarize: bool,
    pub chat: bool,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ProviderSpec {
    pub id: ProviderId,
    pub id_str: &'static str,
    /// Display name for user-facing copy.
    pub label: &'static str,
    /// Keychain account under `KEYCHAIN_SERVICE`; `None` for a provider that
    /// takes no key.
    pub keychain_account: Option<&'static str>,
    pub key_test: Option<KeyTest>,
    pub capabilities: Capabilities,
}

pub const REGISTRY: &[ProviderSpec] = &[
    ProviderSpec {
        id: ProviderId::OpenAi,
        id_str: "openai",
        label: "OpenAI",
        keychain_account: Some("openai_api_key"),
        key_test: Some(KeyTest {
            url: "https://api.openai.com/v1/models",
            auth: AuthScheme::Bearer,
        }),
        capabilities: Capabilities { summarize: true, chat: true },
    },
    ProviderSpec {
        id: ProviderId::Deepgram,
        id_str: "deepgram",
        label: "Deepgram",
        keychain_account: Some("deepgram_api_key"),
        key_test: Some(KeyTest {
            url: "https://api.deepgram.com/v1/projects",
            auth: AuthScheme::Token,
        }),
        capabilities: Capabilities { summarize: false, chat: false },
    },
    ProviderSpec {
        id: ProviderId::Groq,
        id_str: "groq",
        label: "Groq",
        keychain_account: Some("groq_api_key"),
        key_test: Some(KeyTest {
            url: "https://api.groq.com/openai/v1/models",
            auth: AuthScheme::Bearer,
        }),
        capabilities: Capabilities { summarize: false, chat: false },
    },
    ProviderSpec {
        id: ProviderId::Local,
        id_str: "local",
        label: "Local",
        keychain_account: None,
        key_test: None,
        capabilities: Capabilities { summarize: true, chat: true },
    },
];

/// The chat setting stores the local provider as `ollama` from before the
/// vocabularies were unified; stored rows are never rewritten, so it parses.
const LOCAL_ALIASES: &[&str] = &["ollama"];

impl ProviderId {
    pub fn parse(s: &str) -> Option<Self> {
        if let Some(spec) = REGISTRY.iter().find(|p| p.id_str == s) {
            return Some(spec.id);
        }
        if LOCAL_ALIASES.contains(&s) {
            return Some(ProviderId::Local);
        }
        None
    }

    pub fn spec(self) -> &'static ProviderSpec {
        REGISTRY
            .iter()
            .find(|p| p.id == self)
            .expect("every ProviderId has a REGISTRY row")
    }

    pub fn as_str(self) -> &'static str {
        self.spec().id_str
    }

    pub fn keychain_account(self) -> Option<&'static str> {
        self.spec().keychain_account
    }

    pub fn capabilities(self) -> Capabilities {
        self.spec().capabilities
    }

    pub fn label(self) -> &'static str {
        self.spec().label
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_id_has_exactly_one_row_and_round_trips() {
        for spec in REGISTRY {
            assert_eq!(ProviderId::parse(spec.id_str), Some(spec.id));
            assert_eq!(spec.id.as_str(), spec.id_str);
            assert_eq!(REGISTRY.iter().filter(|p| p.id == spec.id).count(), 1);
        }
    }

    #[test]
    fn unknown_ids_are_none_and_nothing_is_trimmed() {
        assert_eq!(ProviderId::parse("nonsense"), None);
        assert_eq!(ProviderId::parse(""), None);
        assert_eq!(ProviderId::parse(" groq "), None);
    }

    #[test]
    fn ollama_is_the_stored_alias_for_local() {
        assert_eq!(ProviderId::parse("ollama"), Some(ProviderId::Local));
        assert_eq!(ProviderId::Local.as_str(), "local");
    }

    #[test]
    fn keychain_accounts_are_unchanged() {
        assert_eq!(ProviderId::OpenAi.keychain_account(), Some("openai_api_key"));
        assert_eq!(ProviderId::Deepgram.keychain_account(), Some("deepgram_api_key"));
        assert_eq!(ProviderId::Groq.keychain_account(), Some("groq_api_key"));
        assert_eq!(ProviderId::Local.keychain_account(), None);
    }

    #[test]
    fn a_key_test_exists_exactly_where_a_key_does() {
        for spec in REGISTRY {
            assert_eq!(spec.key_test.is_some(), spec.keychain_account.is_some(), "{}", spec.id_str);
        }
    }

    #[test]
    fn openai_key_test_sits_under_the_summary_base_url() {
        let url = ProviderId::OpenAi.spec().key_test.unwrap().url;
        assert_eq!(url, format!("{}/models", crate::openai::BASE));
    }

    #[test]
    fn auth_header_values() {
        assert_eq!(AuthScheme::Bearer.header_value("k"), "Bearer k");
        assert_eq!(AuthScheme::Token.header_value("k"), "Token k");
    }
}
